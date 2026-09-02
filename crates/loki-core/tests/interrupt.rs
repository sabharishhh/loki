//! The interrupt latency budget.
//!
//! Section 18.5 commits to under 150ms from signal to the interface visibly stopping. That is a
//! promise about how the product feels, and an untested promise is a wish.
//!
//! What is measured here is the core's half: from cancelling the token to the `Interrupted` event
//! reaching a sink. The interface then renders from that event, so this is the budget the Swift
//! side spends the rest of.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use loki_core::adapters::clock::SystemClock;
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, NullTokens, TokenSink};
use loki_core::core::event::Event;
use loki_core::core::prompt::Prefix;
use loki_core::core::sink::{Collector, EventSink};
use loki_core::core::vocab::{Cents, CostModel, Locality};
use loki_core::ports::model::{
    Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request, ToolSupport,
};
use tokio_util::sync::CancellationToken;

/// A provider that never stops talking, so only the interrupt can end the turn.
struct Endless;

#[async_trait]
impl ModelProvider for Endless {
    fn id(&self) -> &str {
        "endless"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::Cloud,
            prompt_cache: false,
            max_context: 1000,
            tools: ToolSupport::None,
            cost: CostModel::Free,
        }
    }

    async fn complete(
        &self,
        _req: Request,
        _cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        Ok(Box::pin(async_stream::stream! {
            loop {
                // A realistic token cadence. A tight loop would flatter the result.
                tokio::time::sleep(Duration::from_millis(5)).await;
                yield Ok(Chunk::Text("word ".to_owned()));
            }
        }))
    }
}

fn budget() -> Budget {
    Budget::new(Cents::new(1_000_000))
}

/// One run. Returns how long the `Interrupted` event took to arrive.
async fn measure() -> Duration {
    let events = Arc::new(Collector::new());
    let mut core = Loop::new(
        Arc::new(Endless),
        Arc::clone(&events) as Arc<dyn EventSink>,
        Arc::new(NullTokens) as Arc<dyn TokenSink>,
        Arc::new(SystemClock),
        Prefix::new("system"),
        budget(),
    );

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();

    // Let the stream get going, so this measures an interrupt mid-flight rather than at rest.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });

    let started = Instant::now();
    let outcome = core.turn_with("go", cancel).await.expect("turn");
    let elapsed = started.elapsed().saturating_sub(Duration::from_millis(50));

    assert_eq!(
        outcome.status,
        loki_core::core::vocab::TaskStatus::Interrupted,
        "the turn should end interrupted, not completed"
    );
    assert!(
        events
            .events()
            .iter()
            .any(|e| matches!(e, Event::Interrupted { .. })),
        "no Interrupted event reached the sink"
    );
    elapsed
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_lands_inside_the_budget() {
    // Warm up, so the first allocation and task spawn are not counted as latency.
    let _ = measure().await;

    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    const RUNS: u32 = 20;

    for _ in 0..RUNS {
        let taken = measure().await;
        worst = worst.max(taken);
        total += taken;
    }

    let mean = total / RUNS;
    println!("interrupt latency over {RUNS} runs: mean {mean:?}, worst {worst:?}");

    assert!(
        worst < Duration::from_millis(150),
        "section 18.5 budgets 150ms and the worst run took {worst:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_before_the_turn_starts_is_still_honoured() {
    let events = Arc::new(Collector::new());
    let mut core = Loop::new(
        Arc::new(Endless),
        Arc::clone(&events) as Arc<dyn EventSink>,
        Arc::new(NullTokens) as Arc<dyn TokenSink>,
        Arc::new(SystemClock),
        Prefix::new("system"),
        budget(),
    );

    let cancel = CancellationToken::new();
    cancel.cancel();

    let started = Instant::now();
    let outcome = core.turn_with("go", cancel).await.expect("turn");
    let elapsed = started.elapsed();

    assert_eq!(
        outcome.status,
        loki_core::core::vocab::TaskStatus::Interrupted
    );
    assert!(
        elapsed < Duration::from_millis(150),
        "an already-cancelled turn took {elapsed:?}"
    );
}

/// Interrupting must not discard what already streamed. Redoing work is acceptable, losing it is
/// not.
#[tokio::test(flavor = "multi_thread")]
async fn partial_output_survives_the_interrupt() {
    let events = Arc::new(Collector::new());
    let mut core = Loop::new(
        Arc::new(Endless),
        Arc::clone(&events) as Arc<dyn EventSink>,
        Arc::new(NullTokens) as Arc<dyn TokenSink>,
        Arc::new(SystemClock),
        Prefix::new("system"),
        budget(),
    );

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        trigger.cancel();
    });

    let outcome = core.turn_with("go", cancel).await.expect("turn");
    assert!(
        !outcome.text.is_empty(),
        "text streamed before the interrupt was thrown away"
    );
}
