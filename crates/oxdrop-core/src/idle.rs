//! When a Transfer left alone ends: its idle deadline and the warning before it.

use std::time::{Duration, Instant};

/// Where a Transfer stands with respect to its idle deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleState {
    /// A download is streaming, so the Transfer can't go idle.
    Busy,
    /// Nothing to warn about yet; it ends at `deadline` unless there is activity.
    Open { deadline: Instant },
    /// The deadline is close; the Sender shows a countdown with "Keep open".
    Warning { deadline: Instant },
    /// The deadline has passed: the Transfer should end.
    Expired,
}

/// Follows a Transfer's activity and works out its [`IdleState`]. Activity is anything
/// that [`touch`](Self::touch)es it, and every download while it is streaming.
#[derive(Debug, Clone)]
pub struct IdleTimer {
    timeout: Duration,
    warning_lead: Duration,
    last_activity: Instant,
    /// Downloads streaming now.
    streaming: usize,
}

impl IdleTimer {
    /// A timer for a Transfer opened at `now`, ending `timeout` after its last activity
    /// and warning `warning_lead` before that.
    pub fn new(now: Instant, timeout: Duration, warning_lead: Duration) -> Self {
        Self {
            timeout,
            warning_lead,
            last_activity: now,
            streaming: 0,
        }
    }

    /// Something happened at `now` that counts as activity.
    pub fn touch(&mut self, now: Instant) {
        self.last_activity = self.last_activity.max(now);
    }

    /// A download started streaming at `now`.
    pub fn stream_started(&mut self, now: Instant) {
        self.streaming += 1;
        self.touch(now);
    }

    /// A download stopped streaming at `now`, finished or not.
    pub fn stream_ended(&mut self, now: Instant) {
        self.streaming = self.streaming.saturating_sub(1);
        self.touch(now);
    }

    pub fn state(&self, now: Instant) -> IdleState {
        let deadline = self.deadline();
        if self.streaming > 0 {
            IdleState::Busy
        } else if now >= deadline {
            IdleState::Expired
        } else if now >= self.warning_at() {
            IdleState::Warning { deadline }
        } else {
            IdleState::Open { deadline }
        }
    }

    /// When [`state`](Self::state) next changes without any activity, if it will.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        if self.streaming > 0 {
            return None;
        }
        [self.warning_at(), self.deadline()]
            .into_iter()
            .find(|&at| at > now)
    }

    fn deadline(&self) -> Instant {
        self.last_activity + self.timeout
    }

    fn warning_at(&self) -> Instant {
        self.deadline() - self.warning_lead.min(self.timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn timer(start: Instant) -> IdleTimer {
        IdleTimer::new(start, 30 * MINUTE, 2 * MINUTE)
    }

    #[test]
    fn left_alone_it_warns_two_minutes_before_the_deadline_then_expires() {
        let start = Instant::now();
        let timer = timer(start);
        let deadline = start + 30 * MINUTE;

        assert_eq!(timer.state(start), IdleState::Open { deadline });
        assert_eq!(timer.next_change(start), Some(start + 28 * MINUTE));
        assert_eq!(
            timer.state(start + 28 * MINUTE),
            IdleState::Warning { deadline }
        );
        assert_eq!(timer.next_change(start + 28 * MINUTE), Some(deadline));
        assert_eq!(
            timer.state(deadline - Duration::from_millis(1)),
            IdleState::Warning { deadline }
        );
        assert_eq!(timer.state(deadline), IdleState::Expired);
        assert_eq!(timer.next_change(deadline), None);
    }

    #[test]
    fn activity_even_during_the_warning_moves_the_deadline() {
        let start = Instant::now();
        let mut timer = timer(start);

        // "Keep open" during the countdown is activity like any other.
        timer.touch(start + 29 * MINUTE);

        let deadline = start + 59 * MINUTE;
        assert_eq!(
            timer.state(start + 29 * MINUTE),
            IdleState::Open { deadline }
        );
        assert_eq!(
            timer.next_change(start + 29 * MINUTE),
            Some(start + 57 * MINUTE)
        );
    }

    #[test]
    fn a_streaming_download_keeps_it_busy_and_its_end_counts_as_activity() {
        let start = Instant::now();
        let mut timer = timer(start);
        timer.stream_started(start + MINUTE);
        timer.stream_started(start + 2 * MINUTE);

        assert_eq!(timer.state(start + 45 * MINUTE), IdleState::Busy);
        assert_eq!(timer.next_change(start + 45 * MINUTE), None);
        timer.stream_ended(start + 50 * MINUTE);
        assert_eq!(
            timer.state(start + 90 * MINUTE),
            IdleState::Busy,
            "one download is still streaming"
        );

        timer.stream_ended(start + 100 * MINUTE);

        let deadline = start + 130 * MINUTE;
        assert_eq!(
            timer.state(start + 100 * MINUTE),
            IdleState::Open { deadline }
        );
        assert_eq!(timer.state(deadline), IdleState::Expired);
    }
}
