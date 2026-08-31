//! The core loop.
//!
//! One struct, one method. Not a plugin and not replaceable. Phase 1 covers steps 1, 3, 4, 5, 8
//! of the nine-step cycle. Pre-fetch, tool dispatch and consolidation arrive with memory and the
//! tool registry.

use std::sync::Arc;

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use super::budget::{Budget, Verdict};
use super::checkpoint::Checkpoint;
use super::event::Event;
use super::ids::IdGen;
use super::prompt::{Prefix, Standing, Turn};
use super::sink::EventSink;
use super::vocab::{Cents, ModelRole, ScopeKind, TaskStatus};
use crate::ports::model::{Chunk, Message, ModelError, ModelProvider, StopReason, Usage};

/// Receives response text as it arrives.
///
/// Separate from the event stream because tokens are not events. The bridge carries both, and
/// putting every token through the event stream would drown the trace.
pub trait TokenSink: Send + Sync {
    fn token(&self, text: &str);
}

/// A sink that discards tokens, for tests and for the dev harness.
pub struct NullTokens;

impl TokenSink for NullTokens {
    fn token(&self, _text: &str) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub status: TaskStatus,
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    #[error("model failed: {0}")]
    Model(#[from] ModelError),
    #[error("stopped at the spending limit, {spent:?} of {ceiling:?}")]
    OverBudget { spent: Cents, ceiling: Cents },
}

/// Everything one conversation needs.
pub struct Loop {
    provider: Arc<dyn ModelProvider>,
    events: Arc<dyn EventSink>,
    tokens: Arc<dyn TokenSink>,
    ids: Arc<IdGen>,
    prefix: Prefix,
    turn: Turn,
    budget: Budget,
    checkpoint: Checkpoint,
    max_tokens: u32,
}

impl Loop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        events: Arc<dyn EventSink>,
        tokens: Arc<dyn TokenSink>,
        prefix: Prefix,
        budget: Budget,
    ) -> Self {
        Self {
            provider,
            events,
            tokens,
            ids: Arc::new(IdGen::new()),
            prefix,
            turn: Turn::new(),
            budget,
            checkpoint: Checkpoint::default(),
            max_tokens: 64_000,
        }
    }

    #[must_use]
    pub const fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Adds a standing instruction, which compaction can never remove.
    pub fn add_standing(&mut self, instruction: Standing) {
        self.prefix.add_standing(instruction);
    }

    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    #[must_use]
    pub const fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    #[must_use]
    pub const fn turn(&self) -> &Turn {
        &self.turn
    }

    /// Summarizes older history. The prefix is untouched by construction.
    pub fn compact(&mut self, keep: usize, summary: impl Into<String>) {
        self.turn.compact(keep, summary);
    }

    /// Runs one turn.
    ///
    /// # Errors
    /// Fails if the budget is already spent, or if the provider rejects the request outright.
    /// A failure partway through the stream ends the turn as [`TaskStatus::Failed`] rather than
    /// an error, because partial output is still output.
    pub async fn turn_with(
        &mut self,
        message: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<Outcome, LoopError> {
        let task = self.ids.task();
        let message = message.into();
        self.events.emit(&Event::TaskStarted {
            id: task,
            summary: summarize(&message),
        });
        self.checkpoint = Checkpoint::new(task);
        self.turn.push(Message::user(message));

        match self.budget.check() {
            Verdict::Proceed => {}
            Verdict::Warn { spent, ceiling } => {
                self.events.emit(&Event::BudgetWarning { spent, ceiling });
            }
            Verdict::Stop(reason) => {
                self.events.emit(&Event::Blocked {
                    reason: reason.clone(),
                });
                self.events.emit(&Event::TaskFinished {
                    id: task,
                    status: TaskStatus::Blocked,
                });
                return Err(LoopError::OverBudget {
                    spent: self.budget.spent(),
                    ceiling: self.budget.ceiling(),
                });
            }
        }

        let request = super::prompt::build(
            &self.prefix,
            &self.turn,
            ModelRole::Primary,
            self.max_tokens,
        );

        let scope = self.ids.scope();
        self.events.emit(&Event::ScopeOpened {
            id: scope,
            parent: None,
            kind: ScopeKind::Model,
        });
        self.checkpoint.open_scope(scope);

        let started = std::time::Instant::now();
        let stream = self.provider.complete(request, cancel.clone()).await;
        let outcome = match stream {
            Ok(stream) => self.drain(stream, cancel).await,
            Err(e) => {
                self.close_scope(scope, started);
                self.events.emit(&Event::TaskFinished {
                    id: task,
                    status: TaskStatus::Failed,
                });
                return Err(e.into());
            }
        };

        self.close_scope(scope, started);
        self.record_spend(&outcome.usage);

        if !outcome.text.is_empty() {
            self.turn.push(Message::assistant(&outcome.text));
        }

        match outcome.status {
            TaskStatus::Interrupted => self.events.emit(&Event::Interrupted {
                id: task,
                at_step: self.checkpoint.step,
                kept: self.checkpoint.steps(),
                dropped: Vec::new(),
            }),
            status => self.events.emit(&Event::TaskFinished { id: task, status }),
        }

        Ok(outcome)
    }

    async fn drain(
        &self,
        mut stream: crate::ports::model::ChunkStream,
        cancel: CancellationToken,
    ) -> Outcome {
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut status = TaskStatus::Completed;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(Chunk::Text(piece)) => {
                    self.tokens.token(&piece);
                    text.push_str(&piece);
                }
                Ok(Chunk::Thinking(_)) => {}
                Ok(Chunk::Usage(reported)) => merge(&mut usage, reported),
                Ok(Chunk::Done(reason)) => {
                    status = match reason {
                        StopReason::Cancelled => TaskStatus::Interrupted,
                        StopReason::Refusal => TaskStatus::Failed,
                        _ => TaskStatus::Completed,
                    };
                    break;
                }
                Err(_) => {
                    status = TaskStatus::Failed;
                    break;
                }
            }

            if cancel.is_cancelled() {
                status = TaskStatus::Interrupted;
                break;
            }
        }

        Outcome {
            text,
            status,
            usage,
        }
    }

    fn close_scope(&mut self, scope: super::ids::ScopeId, started: std::time::Instant) {
        self.checkpoint.close_scope(scope);
        self.events.emit(&Event::ScopeClosed {
            id: scope,
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
    }

    fn record_spend(&mut self, usage: &Usage) {
        let caps = self.provider.caps();
        self.budget.record_micros(
            caps.cost
                .charge_micros(usage.input_tokens, usage.output_tokens),
        );

        self.events.emit(&Event::ModelCall {
            provider: self.provider.id().to_owned(),
            role: ModelRole::Primary,
            locality: caps.locality,
            tokens_in: usage.input_tokens,
            tokens_out: usage.output_tokens,
            cost: caps.cost,
        });
    }
}

/// Usage arrives across several chunks, so take the larger of each field rather than the last.
fn merge(into: &mut Usage, reported: Usage) {
    into.input_tokens = into.input_tokens.max(reported.input_tokens);
    into.output_tokens = into.output_tokens.max(reported.output_tokens);
    into.cache_read_tokens = into.cache_read_tokens.max(reported.cache_read_tokens);
    into.cache_write_tokens = into.cache_write_tokens.max(reported.cache_write_tokens);
}

/// A short label for the Activity screen.
fn summarize(message: &str) -> String {
    const LIMIT: usize = 60;
    let line = message.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}...", &line[..cut]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sink::Collector;
    use crate::core::vocab::{CostModel, Locality};
    use crate::ports::model::{Caps, ChunkStream, Request, ToolSupport};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A provider that replays a fixed script. Lets the loop be tested without a network or a key.
    struct Fake {
        script: Vec<Result<Chunk, ModelError>>,
        reject: bool,
        seen: Mutex<Vec<Request>>,
    }

    impl Fake {
        fn replying(text: &str) -> Self {
            Self {
                script: vec![
                    Ok(Chunk::Text(text.to_owned())),
                    Ok(Chunk::Usage(Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 1_000_000,
                        ..Usage::default()
                    })),
                    Ok(Chunk::Done(StopReason::EndTurn)),
                ],
                reject: false,
                seen: Mutex::new(Vec::new()),
            }
        }

        fn rejecting() -> Self {
            Self {
                script: Vec::new(),
                reject: true,
                seen: Mutex::new(Vec::new()),
            }
        }

        fn last_request(&self) -> Request {
            self.seen.lock().unwrap().last().cloned().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for Fake {
        fn id(&self) -> &str {
            "fake"
        }

        fn caps(&self) -> Caps {
            Caps {
                locality: Locality::Cloud,
                prompt_cache: true,
                max_context: 1000,
                tools: ToolSupport::None,
                cost: CostModel::PerToken {
                    input_per_mtok: Cents::new(100),
                    output_per_mtok: Cents::new(200),
                },
            }
        }

        async fn complete(
            &self,
            req: Request,
            _cancel: CancellationToken,
        ) -> Result<ChunkStream, ModelError> {
            self.seen.lock().unwrap().push(req);
            if self.reject {
                return Err(ModelError::Unauthorized);
            }
            let script: Vec<_> = self
                .script
                .iter()
                .map(|c| match c {
                    Ok(chunk) => Ok(chunk.clone()),
                    Err(_) => Err(ModelError::Cancelled),
                })
                .collect();
            Ok(Box::pin(futures_util::stream::iter(script)))
        }
    }

    struct Captured(Mutex<String>);

    impl TokenSink for Captured {
        fn token(&self, text: &str) {
            self.0.lock().unwrap().push_str(text);
        }
    }

    fn harness(provider: Arc<dyn ModelProvider>) -> (Loop, Arc<Collector>, Arc<Captured>) {
        let events = Arc::new(Collector::new());
        let tokens = Arc::new(Captured(Mutex::new(String::new())));
        let core = Loop::new(
            provider,
            Arc::clone(&events) as Arc<dyn EventSink>,
            Arc::clone(&tokens) as Arc<dyn TokenSink>,
            Prefix::new("You are Loki."),
            Budget::new(Cents::new(10_000)),
        );
        (core, events, tokens)
    }

    #[tokio::test]
    async fn a_turn_streams_text_and_finishes() {
        let (mut core, events, tokens) = harness(Arc::new(Fake::replying("Three are open.")));
        let outcome = core
            .turn_with("pull the infra tickets", CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(outcome.text, "Three are open.");
        assert_eq!(outcome.status, TaskStatus::Completed);
        assert_eq!(tokens.0.lock().unwrap().as_str(), "Three are open.");

        let kinds: Vec<_> = events.events();
        assert!(matches!(kinds[0], Event::TaskStarted { .. }));
        assert!(matches!(kinds[1], Event::ScopeOpened { .. }));
        assert!(matches!(kinds[2], Event::ScopeClosed { .. }));
        assert!(matches!(kinds[3], Event::ModelCall { .. }));
        assert!(matches!(kinds[4], Event::TaskFinished { .. }));
    }

    #[tokio::test]
    async fn the_reply_joins_the_history_for_the_next_turn() {
        let provider = Arc::new(Fake::replying("ok"));
        let (mut core, _, _) = harness(Arc::clone(&provider) as Arc<dyn ModelProvider>);

        core.turn_with("first", CancellationToken::new())
            .await
            .unwrap();
        core.turn_with("second", CancellationToken::new())
            .await
            .unwrap();

        let messages = provider.last_request().messages;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "ok");
        assert_eq!(messages[2].content, "second");
    }

    #[tokio::test]
    async fn a_standing_instruction_survives_compaction() {
        let provider = Arc::new(Fake::replying("ok"));
        let mut core = Loop::new(
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            Arc::new(Collector::new()),
            Arc::new(NullTokens),
            Prefix::new("You are Loki."),
            Budget::new(Cents::new(1_000_000)),
        );
        core.add_standing(Standing::session("Do nothing until told."));

        for _ in 0..40 {
            core.turn_with("noise", CancellationToken::new())
                .await
                .unwrap();
            core.compact(4, "Earlier context.");
        }

        let request = provider.last_request();
        let prefix: String = request.system.iter().map(|b| b.text.clone()).collect();
        assert!(prefix.contains("Do nothing until told."));
        assert!(request.messages.len() <= 6);
    }

    #[tokio::test]
    async fn spend_is_recorded_against_the_budget() {
        let (mut core, _, _) = harness(Arc::new(Fake::replying("ok")));
        core.turn_with("hello", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(core.budget().spent(), Cents::new(300));
    }

    #[tokio::test]
    async fn the_ceiling_blocks_before_the_model_is_called() {
        let events = Arc::new(Collector::new());
        let mut core = Loop::new(
            Arc::new(Fake::replying("ok")),
            Arc::clone(&events) as Arc<dyn EventSink>,
            Arc::new(NullTokens),
            Prefix::new("You are Loki."),
            Budget::new(Cents::ZERO),
        );

        let err = core
            .turn_with("hello", CancellationToken::new())
            .await
            .unwrap_err();

        assert!(matches!(err, LoopError::OverBudget { .. }));
        let emitted = events.events();
        assert!(emitted.iter().any(|e| matches!(e, Event::Blocked { .. })));
        assert!(!emitted.iter().any(|e| matches!(e, Event::ModelCall { .. })));
    }

    #[tokio::test]
    async fn a_cancelled_token_ends_the_turn_as_interrupted() {
        let (mut core, events, _) = harness(Arc::new(Fake::replying("partial")));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = core.turn_with("hello", cancel).await.unwrap();

        assert_eq!(outcome.status, TaskStatus::Interrupted);
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, Event::Interrupted { .. }))
        );
    }

    #[tokio::test]
    async fn a_rejected_request_fails_the_task_and_closes_the_scope() {
        let (mut core, events, _) = harness(Arc::new(Fake::rejecting()));
        let err = core
            .turn_with("hello", CancellationToken::new())
            .await
            .unwrap_err();

        assert!(matches!(err, LoopError::Model(ModelError::Unauthorized)));
        let emitted = events.events();
        assert!(
            emitted
                .iter()
                .any(|e| matches!(e, Event::ScopeClosed { .. }))
        );
        assert!(emitted.iter().any(
            |e| matches!(e, Event::TaskFinished { status, .. } if *status == TaskStatus::Failed)
        ));
    }

    #[test]
    fn long_first_lines_are_shortened_for_the_activity_row() {
        assert_eq!(summarize("short one"), "short one");
        assert_eq!(summarize("first\nsecond"), "first");
        assert!(summarize(&"x".repeat(200)).ends_with("..."));
    }
}
