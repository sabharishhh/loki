//! Ring 2. The two implementations of [`Clock`](crate::ports::clock::Clock): the real one, and the
//! one that makes §21.2 writable.

use std::sync::atomic::{AtomicI64, Ordering};

use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

use crate::ports::clock::Clock;

/// The machine's clock and the machine's zone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn zone(&self) -> TimeZone {
        TimeZone::system()
    }
}

/// A clock a test drives.
///
/// §21.2's corpus has to put six weeks between two sessions to score whether a claim was wrongly
/// retired. With the real clock that is a six-week wait; with this it is one call. Same reason the
/// gate takes `now` as a parameter rather than reading it: a rule about time that can only be
/// checked in real time is a rule nobody checks.
///
/// Microsecond granularity, held in an atomic so a test can advance it while the code under test
/// holds it behind an `Arc<dyn Clock>`.
#[derive(Debug)]
pub struct FakeClock {
    micros: AtomicI64,
    zone: TimeZone,
}

impl FakeClock {
    /// A clock stopped at `at`, in `zone`.
    #[must_use]
    pub fn new(at: Timestamp, zone: TimeZone) -> Self {
        Self {
            micros: AtomicI64::new(at.as_microsecond()),
            zone,
        }
    }

    /// A clock stopped at `at`, UTC.
    #[must_use]
    pub fn utc(at: Timestamp) -> Self {
        Self::new(at, TimeZone::UTC)
    }

    /// Moves the clock forward, or back on a negative span.
    ///
    /// Added in the local zone rather than to the instant, because a span in weeks or days is a
    /// calendar quantity: across a daylight-saving boundary, one day is the same wall time
    /// tomorrow and not twenty-four hours later. `Timestamp` refuses calendar units for exactly
    /// this reason.
    ///
    /// # Panics
    /// Panics if the span cannot be added, which for a test fixture means the test asked for a
    /// date outside the representable range and wants to hear about it.
    pub fn advance(&self, span: Span) {
        let moved = self
            .zoned()
            .checked_add(span)
            .expect("the fake clock was advanced past the representable range");
        self.set(moved.timestamp());
    }

    /// Puts the clock at an exact instant.
    pub fn set(&self, at: Timestamp) {
        self.micros.store(at.as_microsecond(), Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_microsecond(self.micros.load(Ordering::SeqCst))
            .unwrap_or(Timestamp::UNIX_EPOCH)
    }

    fn zone(&self) -> TimeZone {
        self.zone.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    #[test]
    fn a_fake_clock_holds_still_until_it_is_moved() {
        let clock = FakeClock::utc(at("2026-09-02T14:20:00Z"));
        assert_eq!(clock.now(), clock.now());
        assert_eq!(clock.today(), date(2026, 9, 2));
    }

    #[test]
    fn six_weeks_is_one_call_rather_than_a_six_week_wait() {
        let clock = FakeClock::utc(at("2026-07-15T00:00:00Z"));
        clock.advance(Span::new().weeks(6));
        assert_eq!(clock.today(), date(2026, 8, 26));
    }

    /// A day is a calendar quantity, so it is added in the zone and not to the instant.
    #[test]
    fn a_day_across_a_daylight_saving_boundary_is_still_one_day() {
        let london = TimeZone::get("Europe/London").expect("zone");
        // 26 October 2025, the clocks went back at 02:00 local.
        let clock = FakeClock::new(at("2025-10-25T12:00:00Z"), london);
        clock.advance(Span::new().days(1));
        assert_eq!(clock.today(), date(2025, 10, 26));
        assert_eq!(
            clock.zoned().hour(),
            13,
            "same wall time the next day, not twenty-four hours later"
        );
    }

    /// The local date, not the UTC one. A claim learned at 01:00 in Kolkata was learned today.
    #[test]
    fn today_is_local_and_can_differ_from_the_utc_date() {
        let kolkata = TimeZone::get("Asia/Kolkata").expect("zone");
        let clock = FakeClock::new(at("2026-09-01T19:45:00Z"), kolkata);
        assert_eq!(clock.now().to_zoned(TimeZone::UTC).date(), date(2026, 9, 1));
        assert_eq!(clock.today(), date(2026, 9, 2));
    }

    #[test]
    fn the_system_clock_reports_the_system_zone() {
        let clock = SystemClock;
        assert_eq!(clock.zoned().date(), clock.today());
    }
}
