//! Reporting what each response serves toward Items' downloads, for the Sender's progress.

use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Bytes;
use futures_util::Stream;
use oxdrop_core::ItemId;

use crate::engine::Shared;

/// The body of a download, whichever kind of Item it is.
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

/// Where one Item's download sits in what a response serves from: bytes `at..at + total`
/// of it are bytes `0..total` of the Item's download `identity`.
pub(crate) struct Share {
    pub(crate) item: ItemId,
    pub(crate) identity: String,
    pub(crate) at: u64,
    pub(crate) total: u64,
}

impl Share {
    fn region(&self) -> Range<u64> {
        self.at..self.at + self.total
    }
}

/// Passes on bytes `range` of a download, reporting what they mean for each Item they
/// belong to. An Item's download counts as started once its first bytes go out, ended
/// once its last bytes in `range` do, and cut off if the response stops before that.
pub(crate) struct Tracked {
    bytes: ByteStream,
    shared: Arc<Shared>,
    shares: Vec<Tracking>,
    /// Where the next byte comes from.
    position: u64,
    end: u64,
    finished: bool,
}

struct Tracking {
    share: Share,
    started: bool,
    ended: bool,
}

impl Tracked {
    pub(crate) fn new(
        bytes: ByteStream,
        shared: Arc<Shared>,
        shares: Vec<Share>,
        range: Range<u64>,
    ) -> Self {
        Self {
            bytes,
            shared,
            shares: shares
                .into_iter()
                .map(|share| Tracking {
                    share,
                    started: false,
                    ended: false,
                })
                .collect(),
            position: range.start,
            end: range.end,
            finished: false,
        }
    }

    /// Reports bytes `sent` of the response's byte space as served.
    fn served(&mut self, sent: Range<u64>) {
        for tracking in &mut self.shares {
            let region = tracking.share.region();
            let overlap = sent.start.max(region.start)..sent.end.min(region.end);
            if overlap.is_empty() || tracking.ended {
                continue;
            }
            let Share {
                item,
                identity,
                at,
                total,
            } = &tracking.share;
            let local = overlap.start - at..overlap.end - at;
            let first = !tracking.started;
            let last = overlap.end == region.end || overlap.end == self.end;
            self.shared.report(*item, |tracker| {
                if first {
                    tracker.start(identity, *total, local.start);
                }
                tracker.sent(identity, local);
                if last {
                    tracker.end(identity, true);
                }
            });
            tracking.started = true;
            tracking.ended = last;
        }
    }

    /// The response ended early: every Item download it was serving was cut off.
    fn cut_off(&mut self) {
        for tracking in &mut self.shares {
            if tracking.started && !tracking.ended {
                let identity = &tracking.share.identity;
                self.shared
                    .report(tracking.share.item, |tracker| tracker.end(identity, false));
                tracking.ended = true;
            }
        }
    }

    /// The response served all it set out to. An Item download with nothing to send
    /// (an empty file) is done as soon as the response is.
    fn complete(&mut self) {
        for tracking in &mut self.shares {
            let Share {
                item,
                identity,
                at,
                total,
            } = &tracking.share;
            if *total == 0 && !tracking.started && (self.position..=self.end).contains(at) {
                self.shared.report(*item, |tracker| {
                    tracker.start(identity, 0, 0);
                    tracker.end(identity, true);
                });
                tracking.started = true;
                tracking.ended = true;
            }
        }
        self.finished = true;
    }
}

impl Stream for Tracked {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.bytes.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(chunk))) => {
                let start = self.position;
                self.position += chunk.len() as u64;
                let end = self.position;
                self.served(start..end);
            }
            Poll::Ready(Some(Err(_))) => self.cut_off(),
            Poll::Ready(None) => self.complete(),
            Poll::Pending => {}
        }
        polled
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        // The server may stop polling once `Content-Length` bytes are out (or never poll
        // an empty body), so having passed on every byte is enough to be complete.
        if self.finished {
            return;
        }
        if self.position == self.end {
            self.complete();
        } else {
            self.cut_off();
        }
    }
}
