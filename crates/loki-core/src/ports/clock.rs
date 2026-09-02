//! Ring 1. Time, as something the system reads rather than something it assumes (§6.1, §9.14).
//!
//! Principle 9: time is computed, never recalled. The host resolves every temporal fact before the
//! model sees it, so the model never derives elapsed time, orders events by comparing dates,
//! decides what is stale, or turns "two weeks ago" into an instant.
//!
//! **Why this is a port and not a function.** §21.2's corpus scores a job change, a move and a
//! preference reversal, and doing that means moving weeks of world time between sessions. With
//! `Timestamp::now()` called from the code under test, that corpus cannot be written at all, and
//! principle 9 stays aspirational because nothing can check it. Same argument as `Locality` being
//! a capability rather than a config flag: something the system has to reason about cannot be a
//! convention.
//!
//! **What this is not.** Measuring how long a call took is a monotonic duration, not a temporal
//! fact, so the `ms` fields on the event stream keep using [`std::time::Instant`]. §9.14 names
//! three clocks and says Loki has two of them: a knowledge clock for what was true when, and an
//! agent clock for how much time has passed. An interaction clock, the last few hundred
//! milliseconds, does not exist here because the system is turn-based.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// The current time, and the zone to render it in.
///
/// Read once per turn and passed down. Reading it twice inside one turn is how the three lines of
/// §8.3's frame end up disagreeing with each other.
pub trait Clock: Send + Sync {
    /// The current instant, UTC.
    ///
    /// UTC because a record has to be unambiguous years later (§9.14). Rendering is the caller's
    /// job, against [`Clock::zone`].
    fn now(&self) -> Timestamp;

    /// The zone local times are rendered in.
    ///
    /// Needed because "yesterday" and "this morning" are local, and a person who travels expects
    /// the assistant to follow them.
    fn zone(&self) -> TimeZone;

    /// The current instant in the local zone.
    fn zoned(&self) -> Zoned {
        self.now().to_zoned(self.zone())
    }

    /// Today's date, local.
    ///
    /// The civil date, not the UTC one. A claim learned at 01:00 in Asia/Kolkata was learned
    /// today, not yesterday, and the timeline has to agree with the person reading it.
    fn today(&self) -> Date {
        self.zoned().date()
    }
}
