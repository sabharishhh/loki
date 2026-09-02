//! The product's claim, tested end to end: say something once, and do not say it again.
//!
//! The provider is scripted and so is extraction, because what is under test is whether the loop
//! carries memory across a session boundary, not whether a model can write a sentence.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream;
use jiff::civil::date;
use loki_core::adapters::clock::SystemClock;
use loki_core::core::budget::Budget as Spend;
use loki_core::core::cycle::{Loop, NullTokens};
use loki_core::core::prompt::Prefix;
use loki_core::core::sink::EventSink;
use loki_core::core::vocab::{Cents, CostModel, Locality};
use loki_core::memory::consolidate::{Budget, Candidate, ConsolidateError, Extractor, Unbounded};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Candidate as EntityCandidate, Index};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};
use loki_core::ports::model::{
    Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request, StopReason, ToolSupport, Usage,
};
use tokio_util::sync::CancellationToken;

/// Answers with a fixed line, and keeps every request so the test can read the prompt.
struct Recorder {
    reply: String,
    requests: Mutex<Vec<Request>>,
}

impl Recorder {
    fn new(reply: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: reply.to_string(),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn last(&self) -> Request {
        self.requests
            .lock()
            .expect("lock")
            .last()
            .cloned()
            .expect("a request")
    }
}

#[async_trait]
impl ModelProvider for Recorder {
    fn id(&self) -> &str {
        "recorder"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::Cloud,
            prompt_cache: true,
            max_context: 200_000,
            tools: ToolSupport::None,
            cost: CostModel::Free,
        }
    }

    async fn complete(
        &self,
        req: Request,
        _cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        self.requests.lock().expect("lock").push(req);
        let reply = self.reply.clone();
        Ok(Box::pin(stream::iter(vec![
            Ok(Chunk::Text(reply)),
            Ok(Chunk::Usage(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            })),
            Ok(Chunk::Done(StopReason::EndTurn)),
        ])))
    }
}

struct Silent;

impl EventSink for Silent {
    fn emit(&self, _event: &loki_core::core::event::Event) {}
}

/// Pulls one fixed fact out of any transcript.
struct OneFact {
    surface: String,
    fact: String,
}

#[async_trait]
impl Extractor for OneFact {
    async fn extract(
        &self,
        _episode: &str,
        _text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        Ok(vec![Candidate {
            surface: self.surface.clone(),
            kind: Kind::Person,
            heading: "role".to_string(),
            attribute: "role".to_string(),
            text: self.fact.clone(),
            days_ago: None,
            valid_from: Some(date(2026, 1, 1)),
            source: loki_core::memory::claim::Source::Stated,
            tags: vec![],
        }])
    }
}

struct FirstMatch;

#[async_trait]
impl Matcher for FirstMatch {
    async fn decide(
        &self,
        _surface: &str,
        _claim: &str,
        candidates: &[EntityCandidate],
    ) -> Result<Decision, ResolveError> {
        Ok(if candidates.is_empty() {
            Decision::New
        } else {
            Decision::Existing(0)
        })
    }
}

fn scratch(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loki-remember-{}-{label}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn memory_at(dir: &std::path::Path, session: &str) -> Arc<Memory> {
    Arc::new(
        Memory::open(
            dir,
            Index::in_memory().expect("index"),
            session,
            date(2026, 9, 1),
            TierScope::normal(Locality::Cloud),
        )
        .await
        .expect("memory"),
    )
}

fn a_loop(provider: Arc<Recorder>) -> Loop {
    Loop::new(
        provider,
        Arc::new(Silent),
        Arc::new(NullTokens),
        Arc::new(SystemClock),
        Prefix::new("You are Loki."),
        Spend::new(Cents::new(10_000)),
    )
}

/// Session one is told something. Session two knows it without being told again.
#[tokio::test]
async fn a_fact_learned_in_one_session_reaches_the_next_sessions_prompt() {
    let dir = scratch("across");

    {
        let provider = Recorder::new("Noted.");
        let memory = memory_at(&dir, "one").await;
        let mut first = a_loop(provider);
        first.attach_memory(memory).await.expect("with memory");
        first
            .turn_with("I moved to the infra team", CancellationToken::new())
            .await
            .expect("turn");
        first
            .end_session(
                &OneFact {
                    surface: "Sabharish".to_string(),
                    fact: "works on the infra team".to_string(),
                },
                &FirstMatch,
                &Unbounded,
            )
            .await
            .expect("close");
    }

    // A second occurrence, so the claim promotes past draft and becomes prompt-eligible.
    {
        let provider = Recorder::new("Noted.");
        let memory = memory_at(&dir, "two").await;
        let mut second = a_loop(provider);
        second.attach_memory(memory).await.expect("memory");
        second
            .turn_with("still on infra", CancellationToken::new())
            .await
            .expect("turn");
        second
            .end_session(
                &OneFact {
                    surface: "Sabharish".to_string(),
                    fact: "works on the infra team".to_string(),
                },
                &FirstMatch,
                &Unbounded,
            )
            .await
            .expect("close");
    }

    let provider = Recorder::new("You work on infra.");
    let memory = memory_at(&dir, "three").await;
    let mut third = a_loop(provider.clone());
    third.attach_memory(memory).await.expect("memory");
    third
        .turn_with("which team am I on?", CancellationToken::new())
        .await
        .expect("turn");

    let request = provider.last();
    let prefix: String = request.system.iter().map(|b| b.text.clone()).collect();
    let turn: String = request
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        prefix.contains("infra team") || turn.contains("infra team"),
        "the fact never reached the prompt.\nprefix: {prefix}\nturn: {turn}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// §8.1. Retrieval lands in the turn, never in the prefix, or the cache misses every turn.
#[tokio::test]
async fn recall_lands_in_the_turn_and_never_in_the_prefix() {
    let dir = scratch("zones");
    let provider = Recorder::new("Sure.");
    let memory = memory_at(&dir, "one").await;
    let mut session = a_loop(provider.clone());
    session.attach_memory(memory).await.expect("memory");

    session
        .turn_with("the deploy window is Thursday", CancellationToken::new())
        .await
        .expect("first");
    // Far enough back that it has left the window.
    for n in 0..21 {
        session
            .turn_with(format!("filler {n}"), CancellationToken::new())
            .await
            .expect("filler");
    }
    session
        .turn_with("when is the deploy window?", CancellationToken::new())
        .await
        .expect("ask");

    let request = provider.last();
    let prefix: String = request.system.iter().map(|b| b.text.clone()).collect();
    assert!(
        !prefix.contains("Thursday"),
        "recall reached the frozen prefix, which misses the cache every turn: {prefix}"
    );

    let recalled = request
        .messages
        .first()
        .map(|m| m.content.clone())
        .unwrap_or_default();
    assert!(
        recalled.contains("Thursday"),
        "the session's own earlier turn was not recalled: {recalled}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A loop without memory has to keep working, and send no recall block at all.
#[tokio::test]
async fn a_loop_without_memory_still_runs() {
    let provider = Recorder::new("Hello.");
    let mut session = a_loop(provider.clone());

    let out = session
        .turn_with("hello", CancellationToken::new())
        .await
        .expect("turn");

    assert_eq!(out.text, "Hello.");
    assert_eq!(
        provider.last().messages.len(),
        1,
        "no recall block belongs here"
    );
    assert!(
        session
            .end_session(
                &OneFact {
                    surface: "x".to_string(),
                    fact: "y".to_string()
                },
                &FirstMatch,
                &Unbounded,
            )
            .await
            .expect("close")
            .is_none()
    );
}

/// The prompt must not carry an empty "what you already know" block on a cold store.
#[tokio::test]
async fn nothing_recalled_means_no_recall_block() {
    let dir = scratch("cold");
    let provider = Recorder::new("Hello.");
    let memory = memory_at(&dir, "one").await;
    let mut session = a_loop(provider.clone());
    session.attach_memory(memory).await.expect("memory");

    session
        .turn_with("hello there", CancellationToken::new())
        .await
        .expect("turn");

    let request = provider.last();
    assert_eq!(request.messages.len(), 1, "{:?}", request.messages);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The episode is written as the session runs, not at close (D-045).
#[tokio::test]
async fn turns_are_written_to_the_episode_as_they_happen() {
    let dir = scratch("episode");
    let provider = Recorder::new("Noted.");
    let memory = memory_at(&dir, "one").await;
    let mut session = a_loop(provider);
    session.attach_memory(memory.clone()).await.expect("memory");

    session
        .turn_with(
            "remember the kickoff is on Tuesday",
            CancellationToken::new(),
        )
        .await
        .expect("turn");

    let reader = memory.bundle().reader().await;
    let episode = reader.read("episodes/2026-09-01.md").expect("episode");
    assert!(episode.contains("kickoff is on Tuesday"), "{episode}");
    assert!(
        episode.contains("Noted."),
        "the reply belongs there too: {episode}"
    );
    drop(reader);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The budget is not a suggestion. A run that cannot afford to continue says so.
#[tokio::test]
async fn a_close_that_runs_out_of_budget_reports_what_is_left() {
    struct Broke;
    impl Budget for Broke {
        fn may_continue(&self) -> bool {
            false
        }
    }

    let dir = scratch("broke");
    let provider = Recorder::new("Noted.");
    let memory = memory_at(&dir, "one").await;
    let mut session = a_loop(provider);
    session.attach_memory(memory).await.expect("memory");
    session
        .turn_with("something", CancellationToken::new())
        .await
        .expect("turn");

    let report = session
        .end_session(
            &OneFact {
                surface: "Dan".to_string(),
                fact: "likes tea".to_string(),
            },
            &FirstMatch,
            &Broke,
        )
        .await
        .expect("close")
        .expect("a report");

    assert_eq!(report.remaining, ["episodes/2026-09-01.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// §8.1's exception: an explicit instruction has to be usable on the very next turn.
#[tokio::test]
async fn what_was_just_learned_reaches_the_next_turns_prompt() {
    let dir = scratch("immediate");
    let provider = Recorder::new("Noted.");
    let memory = memory_at(&dir, "one").await;
    let mut session = a_loop(provider.clone());
    session.attach_memory(memory).await.expect("memory");

    session
        .turn_with(
            "remember that I moved to the infra team",
            CancellationToken::new(),
        )
        .await
        .expect("turn");

    // The capture the app runs when it sees an explicit instruction.
    session
        .end_session(
            &OneFact {
                surface: "Sabharish".to_string(),
                fact: "works on the infra team".to_string(),
            },
            &FirstMatch,
            &Unbounded,
        )
        .await
        .expect("capture");
    session.refresh_working_set().await.expect("refresh");

    session
        .turn_with("which team am I on?", CancellationToken::new())
        .await
        .expect("ask");

    let request = provider.last();
    let prefix: String = request.system.iter().map(|b| b.text.clone()).collect();
    assert!(
        prefix.contains("infra team"),
        "the working set did not regenerate mid-session: {prefix}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
