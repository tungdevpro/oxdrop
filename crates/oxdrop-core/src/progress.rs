//! How far the Receiver has got with an Item: bytes served toward its most recent download.

use std::ops::Range;

/// Where an Item's most recent download stands, as the Sender shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    NotStarted,
    /// Being sent, or partly sent with nothing cut off.
    Receiving {
        served: u64,
        total: u64,
    },
    /// Every byte of the download has been served.
    Done,
    /// A response was cut off before every byte had been served.
    Failed {
        served: u64,
        total: u64,
    },
}

/// Follows the responses serving one Item and works out its [`Progress`].
///
/// A download is a byte space named by an identity (the `ETag` it is served under) of
/// `total` bytes. Responses that continue it, such as resumed or parallel `Range`
/// requests, add to what has been served; a response for a different identity, or one
/// starting at byte 0 while nothing else is streaming, starts a new download.
#[derive(Debug, Clone, Default)]
pub struct DownloadTracker {
    download: Option<Download>,
}

#[derive(Debug, Clone)]
struct Download {
    identity: String,
    total: u64,
    /// The bytes served so far, as sorted, non-overlapping, non-touching ranges.
    served: Vec<Range<u64>>,
    /// Responses serving it that haven't ended.
    streaming: usize,
    /// Whether the last response to end was cut off.
    cut_off: bool,
}

impl Download {
    fn served(&self) -> u64 {
        self.served
            .iter()
            .map(|range| range.end - range.start)
            .sum()
    }

    fn add(&mut self, range: Range<u64>) {
        let range = range.start.min(self.total)..range.end.min(self.total);
        if range.is_empty() {
            return;
        }
        // Absorb every range it overlaps or touches, then put it back in order.
        let (mut start, mut end) = (range.start, range.end);
        self.served.retain(|other| {
            let apart = other.end < start || other.start > end;
            if !apart {
                start = start.min(other.start);
                end = end.max(other.end);
            }
            apart
        });
        let at = self.served.partition_point(|other| other.start < start);
        self.served.insert(at, start..end);
    }
}

impl DownloadTracker {
    /// A response begins serving the download `identity` of `total` bytes from byte `from`.
    pub fn start(&mut self, identity: &str, total: u64, from: u64) {
        let continues = self
            .current(identity)
            .is_some_and(|download| from > 0 || download.streaming > 0);
        if !continues {
            self.download = Some(Download {
                identity: identity.to_owned(),
                total,
                served: Vec::new(),
                streaming: 0,
                cut_off: false,
            });
        }
        let download = self.current(identity).expect("just started");
        download.streaming += 1;
    }

    /// A response served bytes `range` of the download `identity`.
    pub fn sent(&mut self, identity: &str, range: Range<u64>) {
        if let Some(download) = self.current(identity) {
            download.add(range);
        }
    }

    /// A response for the download `identity` ended: `complete` if it served everything
    /// it set out to, otherwise it was cut off.
    pub fn end(&mut self, identity: &str, complete: bool) {
        if let Some(download) = self.current(identity) {
            download.streaming = download.streaming.saturating_sub(1);
            download.cut_off = !complete;
        }
    }

    pub fn progress(&self) -> Progress {
        let Some(download) = &self.download else {
            return Progress::NotStarted;
        };
        let (served, total) = (download.served(), download.total);
        if served == total {
            Progress::Done
        } else if download.cut_off && download.streaming == 0 {
            Progress::Failed { served, total }
        } else {
            Progress::Receiving { served, total }
        }
    }

    /// The download being followed, if it is `identity`.
    fn current(&mut self, identity: &str) -> Option<&mut Download> {
        self.download
            .as_mut()
            .filter(|download| download.identity == identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_served_in_full_is_done() {
        let mut tracker = DownloadTracker::default();
        assert_eq!(tracker.progress(), Progress::NotStarted);

        tracker.start("\"v1\"", 1000, 0);
        tracker.sent("\"v1\"", 0..400);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 400,
                total: 1000
            }
        );
        tracker.sent("\"v1\"", 400..1000);
        tracker.end("\"v1\"", true);

        assert_eq!(tracker.progress(), Progress::Done);
    }

    #[test]
    fn a_cut_off_download_fails_and_resuming_it_finishes_it() {
        let mut tracker = DownloadTracker::default();
        tracker.start("\"v1\"", 1000, 0);
        tracker.sent("\"v1\"", 0..300);
        tracker.end("\"v1\"", false);
        assert_eq!(
            tracker.progress(),
            Progress::Failed {
                served: 300,
                total: 1000
            }
        );

        tracker.start("\"v1\"", 1000, 300);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 300,
                total: 1000
            },
            "resuming continues the same download"
        );
        tracker.sent("\"v1\"", 300..1000);
        tracker.end("\"v1\"", true);

        assert_eq!(tracker.progress(), Progress::Done);
    }

    #[test]
    fn downloading_again_from_the_start_or_another_version_starts_over() {
        let mut tracker = DownloadTracker::default();
        tracker.start("\"v1\"", 1000, 0);
        tracker.sent("\"v1\"", 0..1000);
        tracker.end("\"v1\"", true);

        tracker.start("\"v1\"", 1000, 0);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 0,
                total: 1000
            },
            "the same version again"
        );
        tracker.sent("\"v1\"", 0..600);
        tracker.end("\"v1\"", false);

        tracker.start("\"v2\"", 2000, 600);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 0,
                total: 2000
            },
            "a refreshed version, even resumed from mid-way"
        );
        // A response still serving the old version no longer counts.
        tracker.sent("\"v1\"", 600..1000);
        tracker.end("\"v1\"", true);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 0,
                total: 2000
            }
        );
    }

    #[test]
    fn parallel_ranges_add_up_and_one_cut_off_while_others_stream_is_not_a_failure() {
        let mut tracker = DownloadTracker::default();
        tracker.start("\"v1\"", 1000, 0);
        tracker.sent("\"v1\"", 0..200);
        // The browser opens a second connection for the back half, and a third from 0.
        tracker.start("\"v1\"", 1000, 500);
        tracker.start("\"v1\"", 1000, 0);
        tracker.sent("\"v1\"", 500..700);
        tracker.sent("\"v1\"", 100..300);
        tracker.end("\"v1\"", false);
        assert_eq!(
            tracker.progress(),
            Progress::Receiving {
                served: 500,
                total: 1000
            },
            "overlapping bytes count once, and others are still streaming"
        );

        tracker.sent("\"v1\"", 700..1000);
        tracker.end("\"v1\"", true);
        // The first connection is dropped once the bytes it was heading for are served.
        tracker.sent("\"v1\"", 300..500);
        tracker.end("\"v1\"", false);

        assert_eq!(tracker.progress(), Progress::Done);
    }
}
