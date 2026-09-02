//! The `Clock` port doing the job it exists for (§6.1, §9.14).
//!
//! §21.2's corpus scores a job change, a move and a preference reversal, and every one of those
//! needs weeks to pass between two sessions. These tests are the proof that passing weeks is now a
//! call rather than a wait. If they ever need `Timestamp::now()`, the port has stopped working.

use std::sync::Arc;

use jiff::Span;
use jiff::civil::date;
use jiff::tz::TimeZone;
use loki_core::adapters::clock::{FakeClock, SystemClock};
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, TokenSink};
use loki_core::core::prompt::Prefix;
use loki_core::core::sink::EventSink;
use loki_core::core::vocab::{Cents, CostModel, Locality};
use loki_core::memory::claim::Claim;
use loki_core::memory::concept::{Frontmatter, RawConcept, Status};
use loki_core::memory::gate::{Active, GateError, TierScope};
use loki_core::ports::clock::Clock;
use loki_core::ports::model::{Caps, ChunkStream, ModelError, ModelProvider, Request, ToolSupport};
use tokio_util::sync::CancellationToken;

/// The default prompt scope: normal claims, nothing foreign.
fn cloud() -> TierScope {
    TierScope::normal(Locality::Cloud)
}

fn at(text: &str) -> jiff::Timestamp {
    text.parse().expect("timestamp")
}

/// A trip note that stops being worth carrying once the trip is over.
fn expiring_concept() -> RawConcept {
    let mut front = Frontmatter::new("Chennai trip", date(2026, 7, 15));
    front.status = Status::Stable;
    front.stale_after = Some(date(2026, 8, 20));
    let mut concept = RawConcept::new(front);
    concept.add(
        "plan",
        Claim::stated("Sabharish is in Chennai until 20 August", date(2026, 7, 15)).about("plan"),
    );
    concept
}

/// The whole reason the port exists: one store, one code path, two answers, and the only thing
/// that changed is the clock.
#[test]
fn six_weeks_of_world_time_changes_what_the_gate_admits() {
    let clock = FakeClock::utc(at("2026-07-15T09:00:00Z"));

    let admitted = Active::try_from(expiring_concept(), clock.today(), cloud());
    assert!(
        admitted.is_ok(),
        "a live trip note belongs in a prompt: {:?}",
        admitted.err()
    );

    clock.advance(Span::new().weeks(6));

    let refused = Active::try_from(expiring_concept(), clock.today(), cloud());
    assert!(
        matches!(refused, Err(GateError::Stale)),
        "past its own expiry the gate has to refuse it, got {refused:?}"
    );
}

/// Principle 9 is only real if the value the gate reads is the one the host resolved. A gate that
/// called the clock itself could not be tested at a point on a timeline at all.
#[test]
fn the_gate_is_told_the_day_rather_than_asking_for_it() {
    let then = FakeClock::utc(at("2026-08-19T23:00:00Z"));
    let after = FakeClock::utc(at("2026-08-21T01:00:00Z"));

    assert!(Active::try_from(expiring_concept(), then.today(), cloud()).is_ok());
    assert!(Active::try_from(expiring_concept(), after.today(), cloud()).is_err());
}

/// The zone is not decoration. A claim expiring on the 20th is live at 23:00 on the 19th in
/// Kolkata and expired at the same instant in UTC, because the local date has already turned.
#[test]
fn the_zone_decides_which_day_it_is() {
    let instant = at("2026-08-19T19:00:00Z");
    let utc = FakeClock::new(instant, TimeZone::UTC);
    let kolkata = FakeClock::new(instant, TimeZone::get("Asia/Kolkata").expect("zone"));

    assert_eq!(utc.today(), date(2026, 8, 19));
    assert_eq!(kolkata.today(), date(2026, 8, 20));

    assert!(Active::try_from(expiring_concept(), utc.today(), cloud()).is_ok());
    assert!(
        Active::try_from(expiring_concept(), kolkata.today(), cloud()).is_err(),
        "in Kolkata it is already the 20th, so the note has expired"
    );
}

/// Never called. The loop needs a provider to exist, not to answer.
struct Mute;

#[async_trait::async_trait]
impl ModelProvider for Mute {
    fn id(&self) -> &str {
        "mute"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::OnDevice,
            prompt_cache: false,
            max_context: 1_000,
            tools: ToolSupport::None,
            cost: CostModel::Free,
        }
    }

    async fn complete(
        &self,
        _req: Request,
        _cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

struct Silent;
impl EventSink for Silent {
    fn emit(&self, _: &loki_core::core::event::Event) {}
}

struct NullTokens;
impl TokenSink for NullTokens {
    fn token(&self, _: &str) {}
}

/// The loop reads the clock it was handed, not the wall clock. Without this the port is a type
/// that nothing consults.
#[test]
fn the_loop_reads_the_clock_it_was_given() {
    let clock = Arc::new(FakeClock::utc(at("2026-07-15T09:00:00Z")));
    let core = Loop::new(
        Arc::new(Mute),
        Arc::new(Silent),
        Arc::new(NullTokens),
        Arc::clone(&clock) as Arc<dyn Clock>,
        Prefix::new("You are Loki."),
        Budget::new(Cents::new(1_000)),
    );

    assert_eq!(core.clock().today(), date(2026, 7, 15));
    clock.advance(Span::new().weeks(6));
    assert_eq!(
        core.clock().today(),
        date(2026, 8, 26),
        "the loop holds the clock, so advancing it moves the loop's today"
    );
}

/// The real clock still works, and it is what ships.
#[test]
fn the_system_clock_agrees_with_itself() {
    let clock = SystemClock;
    assert_eq!(clock.today(), clock.zoned().date());
}
