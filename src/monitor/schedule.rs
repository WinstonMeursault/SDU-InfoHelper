//! Shared timing mechanics; each caller chooses its existing interval origin.
use std::time::{Duration, Instant};

/// Single-thread schedule: skip missed intervals, and also detect sleep on clocks
/// whose monotonic timer does not advance while the machine is suspended.
#[derive(Clone, Copy)]
pub enum Cadence {
    StartToStart,
    FinishToStart,
}

pub struct Schedule {
    cadence: Cadence,
    interval: Duration,
    next: Instant,
    next_wall: i64,
    last_wall: i64,
}

impl Schedule {
    pub fn new(interval_seconds: u64, now: Instant, wall: i64) -> Self {
        Self::with_cadence(interval_seconds, now, wall, Cadence::StartToStart)
    }

    pub fn with_cadence(interval_seconds: u64, now: Instant, wall: i64, cadence: Cadence) -> Self {
        Self {
            cadence,
            interval: Duration::from_secs(interval_seconds),
            next: now,
            next_wall: wall,
            last_wall: wall,
        }
    }
    pub fn due(&self, now: Instant, wall: i64) -> bool {
        now >= self.next || wall >= self.next_wall || wall < self.last_wall
    }
    pub fn started(&mut self, now: Instant, wall: i64) {
        if matches!(self.cadence, Cadence::StartToStart) {
            self.advance(now, wall);
        }
    }
    pub fn completed(&mut self, now: Instant, wall: i64) {
        if matches!(self.cadence, Cadence::FinishToStart) {
            self.advance(now, wall);
        }
    }
    pub fn remaining(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }
    fn advance(&mut self, now: Instant, wall: i64) {
        self.next = now + self.interval;
        self.next_wall =
            wall.saturating_add(i64::try_from(self.interval.as_secs()).unwrap_or(i64::MAX));
        self.last_wall = wall;
    }
    pub fn next_wall(&self) -> i64 {
        self.next_wall
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_checks_immediately_skips_missed_intervals_and_handles_sleep() {
        let now = Instant::now();
        let mut schedule = Schedule::new(60, now, 1000);
        assert!(schedule.due(now, 1000));
        schedule.started(now, 1000);
        assert!(!schedule.due(now + Duration::from_secs(59), 1059));
        assert!(schedule.due(now + Duration::from_secs(1), 1200));
        assert!(schedule.due(now + Duration::from_secs(1), 900));
        schedule.started(now + Duration::from_secs(180), 1180);
        assert!(!schedule.due(now + Duration::from_secs(180), 1180));
        assert_eq!(schedule.next_wall(), 1240);
    }

    #[test]
    fn long_query_keeps_start_and_completion_cadences_distinct() {
        let now = Instant::now();
        let mut daemon = Schedule::new(60, now, 1000);
        let mut watch = Schedule::with_cadence(60, now, 1000, Cadence::FinishToStart);
        for schedule in [&mut daemon, &mut watch] {
            schedule.started(now, 1000);
        }
        let finished = now + Duration::from_secs(90);
        for schedule in [&mut daemon, &mut watch] {
            schedule.completed(finished, 1090);
        }
        assert_eq!(daemon.remaining(finished), Duration::ZERO);
        assert_eq!(watch.remaining(finished), Duration::from_secs(60));
        assert!(daemon.due(finished, 1090));
        assert!(!watch.due(finished, 1090));
        assert_eq!(watch.next_wall(), 1150);
        daemon.started(finished, 1090);
        assert_eq!(daemon.remaining(finished), Duration::from_secs(60));
    }
}
