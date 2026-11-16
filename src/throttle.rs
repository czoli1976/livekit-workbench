//! "Ask optimal resolution": send the far-end tile's size to the server, but at
//! most once per interval and only when it changed. A change inside the interval
//! is not lost: it is sent when the interval ends (trailing update).

use std::time::{Duration, Instant};

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct SizeThrottle {
    interval: Duration,
    last_sent: Option<((u32, u32), Instant)>,
    pending: Option<(u32, u32)>,
}

impl SizeThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_sent: None,
            pending: None,
        }
    }

    /// Forget what was sent, so the next observed size is sent at once
    /// (used when the option is switched on or a new track is subscribed).
    pub fn reset(&mut self) {
        self.last_sent = None;
        self.pending = None;
    }

    /// Report the size currently on screen. Returns the size to send now, if any.
    pub fn observe(&mut self, size: (u32, u32), now: Instant) -> Option<(u32, u32)> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        match self.last_sent {
            Some((sent, _)) if sent == size => {
                self.pending = None;
                None
            }
            Some((_, at)) if now.duration_since(at) < self.interval => {
                self.pending = Some(size);
                None
            }
            _ => {
                self.last_sent = Some((size, now));
                self.pending = None;
                Some(size)
            }
        }
    }

    /// Call regularly: sends a change that arrived inside the last interval.
    pub fn poll(&mut self, now: Instant) -> Option<(u32, u32)> {
        let pending = self.pending?;
        let (_, at) = self.last_sent?;
        if now.duration_since(at) >= self.interval {
            self.pending = None;
            self.last_sent = Some((pending, now));
            Some(pending)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn first_size_is_sent_at_once() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        assert_eq!(t.observe((1280, 720), t0), Some((1280, 720)));
    }

    #[test]
    fn same_size_is_never_resent() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        t.observe((1280, 720), t0);
        assert_eq!(t.observe((1280, 720), at(t0, 6000)), None);
        assert_eq!(t.poll(at(t0, 20000)), None);
    }

    #[test]
    fn changes_are_rate_limited_and_the_last_one_wins() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        t.observe((1280, 720), t0);
        assert_eq!(t.observe((1000, 560), at(t0, 1000)), None);
        assert_eq!(t.observe((900, 500), at(t0, 2000)), None);
        assert_eq!(t.poll(at(t0, 4999)), None);
        assert_eq!(t.poll(at(t0, 5000)), Some((900, 500)));
        assert_eq!(t.poll(at(t0, 5100)), None);
    }

    #[test]
    fn change_after_interval_goes_straight_out() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        t.observe((1280, 720), t0);
        assert_eq!(t.observe((640, 360), at(t0, 6000)), Some((640, 360)));
    }

    #[test]
    fn returning_to_the_sent_size_cancels_the_pending_one() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        t.observe((1280, 720), t0);
        t.observe((640, 360), at(t0, 1000));
        t.observe((1280, 720), at(t0, 2000));
        assert_eq!(t.poll(at(t0, 6000)), None);
    }

    #[test]
    fn zero_sizes_are_ignored_and_reset_resends() {
        let t0 = Instant::now();
        let mut t = SizeThrottle::new(DEFAULT_INTERVAL);
        assert_eq!(t.observe((0, 720), t0), None);
        t.observe((1280, 720), t0);
        t.reset();
        assert_eq!(t.observe((1280, 720), at(t0, 10)), Some((1280, 720)));
    }
}
