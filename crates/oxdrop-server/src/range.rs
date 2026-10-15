//! Single-range `Range` requests (RFC 9110 §14), so browsers can resume downloads.

use axum::http::HeaderMap;
use axum::http::header;

/// Which bytes of a `size`-byte representation tagged `etag` the request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Requested {
    /// The whole thing: no `Range`, one we don't serve (several ranges, bad syntax),
    /// or an `If-Range` naming a version that's no longer offered.
    Full,
    /// Bytes `start..=end`.
    Partial { start: u64, end: u64 },
    /// A range that lies entirely past the end.
    Unsatisfiable,
}

impl Requested {
    pub(crate) fn from_headers(headers: &HeaderMap, size: u64, etag: &str) -> Self {
        let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
            return Requested::Full;
        };
        // A resume must continue the same version; otherwise start over.
        if let Some(if_range) = headers.get(header::IF_RANGE)
            && if_range.as_bytes() != etag.as_bytes()
        {
            return Requested::Full;
        }
        parse(range, size)
    }
}

fn parse(range: &str, size: u64) -> Requested {
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return Requested::Full;
    };
    // Several ranges would need a multipart response; serving everything is also allowed.
    let Some((first, last)) = spec.split_once('-').filter(|_| !spec.contains(',')) else {
        return Requested::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    let number = |text: &str| text.parse::<u64>().ok();
    if first.is_empty() {
        // `bytes=-n`: the last n bytes.
        return match number(last) {
            None => Requested::Full,
            Some(0) => Requested::Unsatisfiable,
            Some(_) if size == 0 => Requested::Unsatisfiable,
            Some(suffix) => Requested::Partial {
                start: size.saturating_sub(suffix),
                end: size - 1,
            },
        };
    }
    // `bytes=a-` or `bytes=a-b`.
    let Some(start) = number(first) else {
        return Requested::Full;
    };
    let end = match last {
        "" => None,
        last => match number(last) {
            Some(end) if end >= start => Some(end),
            _ => return Requested::Full,
        },
    };
    if start >= size {
        return Requested::Unsatisfiable;
    }
    Requested::Partial {
        start,
        end: end.map_or(size - 1, |end| end.min(size - 1)),
    }
}
