//! The core loop.
//!
//! One struct, one method. Not a plugin and not replaceable. Phase 1 covers steps 1, 3, 4, 5, 8
//! of the nine-step cycle. Pre-fetch, tool dispatch and consolidation arrive with memory and the
//! tool registry.

use std::sync::Arc;

use jiff::Timestamp;
use jiff::civil::Date;

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use super::budget::{Budget, Verdict};
use super::checkpoint::Checkpoint;
use super::event::Event;
use super::ids::{ClaimId, ConceptId, ContentHash, IdGen, QueryHash, TaskId};
use super::prompt::{Prefix, Standing, Turn};
use super::sink::EventSink;
use super::temporal;
use super::trigger;
use super::vocab::{self, BlockReason, Cents, Lane, ModelRole, ScopeKind, TaskStatus};
use crate::memory::handle::{self, Memory, Speaker};
use crate::memory::runtime;
use crate::ports::clock::Clock;
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
    #[error("memory failed: {0}")]
    Memory(#[from] crate::memory::handle::MemoryError),
}

/// How many recent messages stay verbatim in the prompt before recall may reach for them.
const DEFAULT_WINDOW_KEEPS: u32 = 20;

/// What the model writes to ask for a deeper search of memory (§10.8, D-062).
const SEARCH_MARKER: &str = "SEARCH:";

/// The line added to the recall block on a turn where the model may ask.
///
/// Deliberately not "search if you are unsure". A model told to judge its own confidence will
/// answer from a wrong recall as readily as from a right one, because it cannot tell them apart
/// either. Told to check whether the lines in front of it are *about the thing asked*, it can.
const ASK_TO_SEARCH: &str = "\
If the above does not answer what was asked, or is about someone or something else, reply with \
exactly one line and nothing else:

SEARCH: <what to look for>

You will be given the results and asked again. Do not explain, do not apologise, and do not answer \
partially first. If the above does answer it, ignore this and reply normally.";

/// The same offer, for the web (§12.6's last row).
///
/// A separate marker from memory's, because the two searches cost different things and reach
/// different places, and a model that could not say which it wanted would have the host guessing.
const ASK_TO_SEARCH_WEB: &str = "\
If answering this needs something current, or something you would only know by looking, reply with \
exactly one line and nothing else:

WEB: <what to look for>

You will be given what was found, with numbered sources, and asked again. If you can answer without \
it, ignore this and reply normally.";

/// Reads a search request out of a reply, or `None` if it is an ordinary answer.
///
/// The marker has to open the reply. A model that answers and then mentions searching has already
/// answered, and re-asking would throw away a reply the user is entitled to.
fn search_request(text: &str) -> Option<String> {
    let text = text.trim_start();
    let opening: String = text.chars().take(SEARCH_MARKER.len()).collect();
    if !opening.eq_ignore_ascii_case(SEARCH_MARKER) {
        return None;
    }
    let want = text[opening.len()..]
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    (!want.is_empty()).then_some(want)
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
    /// Memory, when there is any. `None` is a working assistant with no recall, which is what the
    /// dev harness and every test that is not about memory want.
    memory: Option<Arc<Memory>>,
    /// Turns kept verbatim in the prompt. Recall never returns anything inside this, because it
    /// is already there.
    window_keeps: u32,
    /// Read once per turn, never per use (§8.3). Two reads inside one turn is how the three lines
    /// of the temporal frame end up disagreeing with each other.
    clock: Arc<dyn Clock>,
    /// When this session began. One of §8.3's three lines is measured from it.
    session_started: Timestamp,
    /// The last day the user said anything before this session, for the third line.
    last_spoke: Option<Date>,
    /// The web, when it is configured. `None` is a working assistant that cannot reach it, which
    /// is what every test that is not about search wants, and what a build with no engine has.
    web: Option<Arc<crate::core::websearch::Search>>,
    /// What the last turn put in play and what it cited, for the rails.
    ///
    /// **Kept here because the rails ask after the turn has ended.** Both are computed inside a
    /// turn and neither survived it, so `loki_recalled` has been returning an empty list since it
    /// was written and the in-play rail has never had anything to draw (B-73).
    last_recalled: Vec<crate::memory::index::Recalled>,
    last_cited: Vec<crate::core::websearch::Cited>,
}

impl Loop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        events: Arc<dyn EventSink>,
        tokens: Arc<dyn TokenSink>,
        clock: Arc<dyn Clock>,
        mut prefix: Prefix,
        budget: Budget,
    ) -> Self {
        // The session anchor belongs to the session, not to memory, so it is set here rather than
        // in `attach_memory`: a loop with no store still knows when it started.
        let session_started = clock.now();
        prefix.set_session_start(&session_started.to_zoned(clock.zone()));
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
            memory: None,
            window_keeps: DEFAULT_WINDOW_KEEPS,
            session_started,
            clock,
            last_spoke: None,
            web: None,
            last_recalled: Vec::new(),
            last_cited: Vec::new(),
        }
    }

    /// Gives the loop the web (§12).
    ///
    /// Same shape as `attach_memory` and for the same reason: a build with no engine configured is
    /// an assistant that cannot search, not one that will not start.
    pub fn attach_web(&mut self, web: Arc<crate::core::websearch::Search>) {
        self.web = Some(web);
    }

    /// What the last turn put in play (§9.2's rail).
    #[must_use]
    pub fn last_recalled(&self) -> &[crate::memory::index::Recalled] {
        &self.last_recalled
    }

    /// What the last turn cited (§12.7).
    #[must_use]
    pub fn last_cited(&self) -> &[crate::core::websearch::Cited] {
        &self.last_cited
    }

    /// The clock this loop reads. Principle 9: callers resolve time, the model never does.
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Gives the loop memory. The working set goes into the frozen prefix once, here, because it
    /// changes per session and not per turn (§8.1).
    ///
    /// Takes `&mut self` rather than consuming: a store that will not open must leave a working
    /// assistant with no recall, not no assistant.
    ///
    /// # Errors
    /// Fails if the working set cannot be read.
    pub async fn attach_memory(&mut self, memory: Arc<Memory>) -> Result<(), LoopError> {
        let working_set = memory.working_set().await.map_err(LoopError::Memory)?;
        if !working_set.is_empty() {
            self.prefix.set_working_set(working_set);
        }
        // Read before this session records anything, or today's own episode answers the question.
        self.last_spoke = memory.last_spoke_on().await.unwrap_or(None);
        self.memory = Some(memory);
        Ok(())
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

    /// The provider, for the Utility-role calls consolidation makes at session close.
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&self.provider)
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

    /// Re-reads the working set into the frozen prefix.
    ///
    /// §8.1: an explicit instruction to remember something "regenerates the working set
    /// immediately and accepts one cache miss". This is that cache miss, taken deliberately and
    /// only when the user asked for it, rather than on every turn.
    ///
    /// # Errors
    /// Fails if the working set cannot be read.
    pub async fn refresh_working_set(&mut self) -> Result<(), LoopError> {
        let Some(memory) = self.memory.clone() else {
            return Ok(());
        };
        let working_set = memory.working_set().await.map_err(LoopError::Memory)?;
        if !working_set.is_empty() {
            self.prefix.set_working_set(working_set);
        }
        Ok(())
    }

    /// Closes the session: consolidate, regenerate the working set, forget the raw turns.
    ///
    /// Runs at session close because the app is already awake, and every session, so the cost
    /// compounds rather than arriving as one bill (§9.8). Silent when there is no memory.
    ///
    /// # Errors
    /// Fails if consolidation or the working set does.
    pub async fn end_session(
        &mut self,
        extractor: &dyn crate::memory::consolidate::Extractor,
        matcher: &dyn crate::memory::resolve::Matcher,
        budget: &dyn crate::memory::consolidate::Budget,
    ) -> Result<Option<crate::memory::consolidate::Report>, LoopError> {
        let Some(memory) = self.memory.clone() else {
            return Ok(None);
        };
        let report = memory
            .close(extractor, matcher, budget, self.clock.today())
            .await?;
        self.prefix.end_session();
        Ok(Some(report))
    }

    /// Consolidates whatever a previous session left behind (§18.2).
    ///
    /// A session ended by a crash or a force quit never ran its close, so its buffer is still on
    /// disk and its turns are claims nobody has extracted. B-30: without this they are orphaned,
    /// because the live corpus that covered them belonged to the process that died.
    ///
    /// Separate from `attach_memory` because it needs an extractor, and the caller owns that.
    /// Silent when there is nothing outstanding, which is the common case.
    ///
    /// # Errors
    /// Fails if the store cannot be read or written.
    pub async fn catch_up(
        &mut self,
        extractor: &dyn crate::memory::consolidate::Extractor,
        matcher: &dyn crate::memory::resolve::Matcher,
        budget: &dyn crate::memory::consolidate::Budget,
    ) -> Result<Option<crate::memory::consolidate::Report>, LoopError> {
        let Some(memory) = self.memory.clone() else {
            return Ok(None);
        };
        if !memory.has_unconsolidated().await {
            return Ok(None);
        }
        let report = memory
            .close(extractor, matcher, budget, self.clock.today())
            .await?;
        // The working set changed, so the prefix has to catch up before the first turn.
        self.refresh_working_set().await?;
        Ok(Some(report))
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
        // Cleared per turn. A rail showing the previous turn's sources beside this turn's answer
        // is worse than an empty one, because it looks like provenance.
        self.last_recalled.clear();
        self.last_cited.clear();

        // Pre-fetch runs on the user's message, before the model call, not as a tool call after
        // it (§10.1). A round trip on every turn where memory matters is most of where the sense
        // of already knowing you goes.
        // One clock read for the whole turn (§8.3). Two would let the frame and the distances on
        // the recalled claims disagree about what "now" is.
        let now = self.clock.zoned();
        let today = now.date();
        self.turn
            .set_frame(temporal::Frame::new(now, self.session_started, self.last_spoke).render());
        // Derived, and it answers one message. Left standing it would answer the next one too.
        self.turn.set_search("");

        // Whether the model may ask for a deeper search on this turn. Set below, once lane 1 has
        // run and the floor has had its chance.
        let mut armed = false;
        // Read inside the memory block and used after it, because §12.6's first row is "memory
        // already answered" and the trigger cannot ask that question without the score.
        let mut best: Option<f32> = None;

        if let Some(memory) = self.memory.clone() {
            memory.record(Speaker::User, &message).await?;
            let recalled = memory.recall(&message, self.window_keeps, today)?;
            best = recalled.first().map(|hit| hit.score.value());
            self.last_recalled.clone_from(&recalled);
            let texts: Vec<String> = recalled.iter().map(|hit| hit.text.clone()).collect();
            if recalled.is_empty() {
                self.turn.set_recall("");
            } else {
                memory.mark_used(&recalled)?;
                // One row per returned claim, for §10.6's three counted signals. Lane 1, because
                // this is automatic recall; lane 2 records its own.
                memory.note_recall(&recalled, &message, today, Lane::Automatic)?;
                // Principle 7: nothing acts outside the event stream, and a retrieval is an act.
                // The log and the event carry the same digest, so the two can be lined up.
                self.events.emit(&Event::MemoryRecalled {
                    claim_ids: recalled
                        .iter()
                        .filter(|r| r.layer == crate::memory::index::Layer::Consolidated)
                        .map(|r| ClaimId::new(ConceptId::new(&r.path), r.ordinal))
                        .collect(),
                    lane: Lane::Automatic,
                    query_hash: QueryHash::new(crate::memory::index::query_hash(&message)),
                });
                // Marked in the buffer so the next pass does not read Loki's own words back as a
                // fresh statement from the user (§9.8).
                memory.note_recalled(&recalled).await?;
                self.turn.set_recall(handle::render(&recalled, today));
            }

            // The floor is for a turn where the model has nothing to read. When lane 1 came back
            // with claims, the model judges them itself, and judging them from the text beats
            // judging them from a score: a low score on a vague question means the question was
            // vague, not that the five claims it returned are useless. Without this, "my dad is a
            // civil contractor" recalled five facts and then spent five navigator calls and nine
            // seconds finding nothing (B-47).
            let nothing_to_read =
                recalled.is_empty() || runtime::missed_the_subject(&message, &texts);
            // An instruction to search is not a question the score can answer. "Search my memory
            // and list everything in it" is not satisfied by a good keyword hit, and it is not
            // satisfied by the working set either, so it escalates whatever lane 1 returned. B-56.
            let floor = runtime::asks_to_search(&message)
                || (nothing_to_read && runtime::should_escalate(&message, best));
            if floor {
                self.escalate(task, &memory, &message, today, cancel.clone())
                    .await;
            } else {
                // Armed on the question alone. Tying this to the score as well would hand the
                // decision back to the number that cannot tell a confident hit from a right one,
                // which is the whole reason the model gets a voice (D-062).
                armed = runtime::asks_about_the_past(&message);
            }
        }

        // §12.6, read after memory because memory is free by this point and its answer outranks a
        // search. The trigger never sees the model: it reads the question and lane 1's score.
        if let Some(web) = self.web.clone() {
            let reach = trigger::decide(
                &message,
                trigger::Situation {
                    recall: best,
                    asked: false,
                },
            );
            match reach {
                trigger::Reach::Yes => {
                    self.search_the_web(task, &web, &message, cancel.clone())
                        .await
                }
                // The host is not sure, so it does not decide. The model is told it may ask, in
                // the same slot and the same shape memory search uses (§10.8).
                trigger::Reach::Offer => self.turn.set_offer(ASK_TO_SEARCH_WEB),
                trigger::Reach::No => {}
            }
        }

        if armed {
            self.turn.set_offer(ASK_TO_SEARCH);
        }

        self.turn.push(Message::user(message.clone()));

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

        let mut outcome = self.call(task, cancel.clone(), armed).await?;

        // The model read its recall and said it was not enough. One retry, ever: the second call
        // is never armed, so a model that keeps asking gets one search and then has to answer.
        if let Some(want) = armed.then(|| search_request(&outcome.text)).flatten()
            && let Some(memory) = self.memory.clone()
        {
            self.escalate(task, &memory, &want, today, cancel.clone())
                .await;
            self.turn.set_offer("");
            outcome = self.call(task, cancel.clone(), false).await?;
        }

        if !outcome.text.is_empty() {
            self.turn.push(Message::assistant(&outcome.text));
            if let Some(memory) = self.memory.clone() {
                memory.record(Speaker::Loki, &outcome.text).await?;
            }
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

    /// One model call: the scope, the stream, the spend.
    ///
    /// `armed` holds the opening characters back until they are known not to be a search request,
    /// so a request the user was never meant to see is never streamed.
    async fn call(
        &mut self,
        task: TaskId,
        cancel: CancellationToken,
        armed: bool,
    ) -> Result<Outcome, LoopError> {
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
            Ok(stream) => self.drain(stream, cancel, armed).await,
            Err(e) => {
                self.close_scope(scope, started);
                // Say why. A bare "failed" leaves the user with nothing to act on.
                self.events.emit(&Event::Blocked {
                    reason: BlockReason::ProviderFailed {
                        provider: self.provider.id().to_owned(),
                        detail: explain(&e),
                    },
                });
                self.events.emit(&Event::TaskFinished {
                    id: task,
                    status: TaskStatus::Failed,
                });
                return Err(e.into());
            }
        };

        self.close_scope(scope, started);
        self.record_spend(task, &outcome.usage, ModelRole::Primary);
        Ok(outcome)
    }

    /// Runs lane 2 and puts what it found into the turn (§10.8).
    ///
    /// Never fails the turn. A search that cannot run leaves the answer where it would have been
    /// without one, and losing the answer as well would make the failure twice as expensive.
    async fn escalate(
        &mut self,
        task: TaskId,
        memory: &Arc<Memory>,
        question: &str,
        today: Date,
        cancel: CancellationToken,
    ) {
        let provider = Arc::clone(&self.provider);
        let navigator = runtime::ModelNavigator::new(provider.as_ref(), cancel);
        let found = memory
            .search_deeply(question, &navigator, today, self.clock.as_ref())
            .await;
        // Charged whether or not the search worked. Tokens spent on a failed search are still
        // spent, and a ledger that only counts successes is not a ledger. A navigator that never
        // ran is a different thing from one that ran and found nothing, and only the second is a
        // call worth recording.
        let spent = navigator.usage();
        if spent != Usage::default() {
            self.record_spend(task, &spent, ModelRole::Utility);
        }

        match found {
            Ok(found) => {
                self.events.emit(&Event::MemoryRecalled {
                    claim_ids: Vec::new(),
                    lane: Lane::Deliberate,
                    query_hash: QueryHash::new(crate::memory::index::query_hash(question)),
                });
                self.turn.set_search(found.brief());
            }
            // §10.8: found, empty and could-not-run are three answers. Reporting the failure only
            // to the event stream left the model answering as though the store held nothing, which
            // is the silence-as-fact the section forbids, arriving through the code that
            // implements it. Failure point 91.
            Err(why) => {
                self.events.emit(&Event::Blocked {
                    reason: BlockReason::ProviderFailed {
                        provider: self.provider.id().to_owned(),
                        detail: why.to_string(),
                    },
                });
                self.turn.set_search(runtime::Found::failed().brief());
            }
        }
    }

    /// Runs one web search and puts what it found into the turn (§12.7).
    ///
    /// Never fails the turn. A search that cannot run leaves the answer where it would have been
    /// without one, and losing the answer as well would make the failure twice as expensive. The
    /// same rule `escalate` follows for memory, for the same reason.
    async fn search_the_web(
        &mut self,
        task: TaskId,
        web: &crate::core::websearch::Search,
        question: &str,
        cancel: CancellationToken,
    ) {
        match web.run(question, cancel).await {
            Ok(found) => {
                self.events.emit(&Event::Searched {
                    task,
                    query: question.to_owned(),
                    provider: web.discover.id().to_owned(),
                    hits: u32::try_from(found.sources.len()).unwrap_or(u32::MAX),
                    cost: vocab::CostModel::Free,
                });
                for source in &found.sources {
                    // One event per source, whether it was read or came from a summary: §12.9's
                    // ledger counts what answered, and a snippet that answered is a rung that did
                    // not have to run.
                    self.events.emit(&Event::Fetched {
                        task,
                        url: source.url.clone(),
                        // Empty until S2 content-addresses what was fetched. Stated rather than
                        // faked: a hash invented here would be a hash of nothing, and §12.7's
                        // point is that the cached page can be checked against it.
                        hash: ContentHash::new(""),
                        rung: vocab::Rung::Direct,
                        verdict: if source.read {
                            vocab::Verdict::Ok
                        } else {
                            vocab::Verdict::JsRequired
                        },
                        cost: vocab::CostModel::Free,
                    });
                }
                self.last_cited.clone_from(&found.sources);
                self.turn.set_web(found.brief());
            }
            // §12.4: an unreadable web is reported, never returned as an empty one. The model is
            // told, so it can say so rather than answering as though nothing was out there.
            Err(why) => {
                self.events.emit(&Event::Blocked {
                    reason: BlockReason::ProviderFailed {
                        provider: web.discover.id().to_owned(),
                        detail: why.to_string(),
                    },
                });
                self.turn
                    .set_web("The web could not be reached this turn. Say so rather than answering as though it had been.");
            }
        }
    }

    async fn drain(
        &self,
        mut stream: crate::ports::model::ChunkStream,
        cancel: CancellationToken,
        armed: bool,
    ) -> Outcome {
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut status = TaskStatus::Completed;
        let mut held = String::new();
        let mut deciding = armed;
        let mut suppress = false;
        // Set by `Done`, and the stream keeps being read afterwards.
        //
        // **A provider may report what a call cost after it has said the call is over.** OpenAI
        // does exactly that: the chunk carrying `finish_reason` has no usage on it, and a final
        // chunk with an empty `choices` array carries it. Breaking on `Done` meant every OpenAI
        // turn was recorded as zero tokens and zero cost, in the ledger and in the meters. B-45.
        let mut finished = false;

        while let Some(chunk) = stream.next().await {
            // Nothing but the trailing usage report is expected once a provider has stopped. Any
            // other chunk means the stream is not behaving, and reading on would be unbounded.
            if finished && !matches!(chunk, Ok(Chunk::Usage(_))) {
                break;
            }
            match chunk {
                Ok(Chunk::Text(piece)) => {
                    text.push_str(&piece);
                    if suppress {
                    } else if deciding {
                        held.push_str(&piece);
                        let probe = held.trim_start();
                        if probe.chars().count() >= SEARCH_MARKER.len() {
                            deciding = false;
                            let opening: String = probe.chars().take(SEARCH_MARKER.len()).collect();
                            if opening.eq_ignore_ascii_case(SEARCH_MARKER) {
                                suppress = true;
                            } else {
                                self.tokens.token(&held);
                            }
                            held.clear();
                        }
                    } else {
                        self.tokens.token(&piece);
                    }
                }
                Ok(Chunk::Thinking(_)) => {}
                Ok(Chunk::Usage(reported)) => merge(&mut usage, reported),
                Ok(Chunk::Done(reason)) => {
                    status = match reason {
                        StopReason::Cancelled => TaskStatus::Interrupted,
                        StopReason::Refusal => TaskStatus::Failed,
                        _ => TaskStatus::Completed,
                    };
                    finished = true;
                }
                Err(e) => {
                    // A failure partway through the stream needs a reason as much as one before
                    // it. Partial text already streamed stays; only the ending changes.
                    self.events.emit(&Event::Blocked {
                        reason: BlockReason::ProviderFailed {
                            provider: self.provider.id().to_owned(),
                            detail: explain(&e),
                        },
                    });
                    status = TaskStatus::Failed;
                    break;
                }
            }

            if cancel.is_cancelled() {
                status = TaskStatus::Interrupted;
                break;
            }
        }

        // A reply shorter than the marker never got its verdict. It is not a search request.
        if deciding && !held.is_empty() {
            self.tokens.token(&held);
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

    fn record_spend(&mut self, task: TaskId, usage: &Usage, role: ModelRole) {
        // Recorded even when the provider reported nothing. Principle 7: a call that happened is
        // an act, and suppressing the event because the numbers were zero is how B-45 stayed
        // invisible for a phase, with no `cost` line in the transcript to notice was missing.
        let caps = self.provider.caps();
        self.budget.record_micros(
            caps.cost
                .charge_micros(usage.input_tokens, usage.output_tokens),
        );

        self.events.emit(&Event::ModelCall {
            task,
            provider: self.provider.id().to_owned(),
            role,
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

/// Turns a provider failure into something a person can act on.
fn explain(error: &ModelError) -> String {
    match error {
        ModelError::Unauthorized(body) => {
            format!("the key was rejected. {}", detail(body))
        }
        ModelError::RateLimited(Some(after)) => {
            format!("rate limited, try again in {}s", after.as_secs())
        }
        ModelError::RateLimited(None) => "rate limited, try again shortly".to_owned(),
        ModelError::BadRequest(body) => format!("the request was rejected. {}", detail(body)),
        ModelError::Upstream { status, body } => {
            format!("returned {status}. {}", detail(body))
        }
        ModelError::Transport(detail) => format!("could not reach the provider: {detail}"),
        ModelError::Protocol(detail) => format!("could not read the response: {detail}"),
        ModelError::Cancelled => "cancelled".to_owned(),
    }
}

/// Pulls the human-readable part out of a provider error body.
///
/// Both providers wrap the useful sentence in JSON. Showing the raw envelope buries it.
fn detail(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/message"))
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| first_line(body));

    if message.is_empty() {
        "No detail given.".to_owned()
    } else {
        message
    }
}

/// Provider error bodies are often a wall of JSON. One line is enough to act on.
fn first_line(body: &str) -> String {
    const LIMIT: usize = 200;
    let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    match line.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}...", &line[..cut]),
        None => line.to_owned(),
    }
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

    /// A clock stopped at one instant.
    ///
    /// Local rather than the `FakeClock` adapter, because Ring 0 may not import Ring 2 and a
    /// `#[cfg(test)]` block is still Ring 0. Nothing here depends on time moving.
    struct Stopped;

    impl Clock for Stopped {
        fn now(&self) -> jiff::Timestamp {
            "2026-09-02T14:20:00Z".parse().expect("timestamp")
        }

        fn zone(&self) -> jiff::tz::TimeZone {
            jiff::tz::TimeZone::UTC
        }
    }

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
                return Err(ModelError::Unauthorized("test".into()));
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
            Arc::new(Stopped),
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
        // The frame leads every request (§8.3) and is not history, so the conversation starts
        // after it. Asserting on the content rather than the count keeps this test about the
        // history and not about how many things precede it.
        let history: Vec<&str> = messages
            .iter()
            .skip_while(|m| m.content.starts_with("Now: "))
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(history, ["first", "ok", "second"]);
    }

    /// The frame is rebuilt each turn and never accumulates. One per request, always.
    #[tokio::test]
    async fn the_frame_leads_the_request_and_appears_once() {
        let provider = Arc::new(Fake::replying("ok"));
        let (mut core, _, _) = harness(Arc::clone(&provider) as Arc<dyn ModelProvider>);

        for message in ["first", "second", "third"] {
            core.turn_with(message, CancellationToken::new())
                .await
                .unwrap();
        }

        let messages = provider.last_request().messages;
        let frames = messages
            .iter()
            .filter(|m| m.content.starts_with("Now: "))
            .count();
        assert_eq!(frames, 1, "{messages:?}");
        assert!(messages[0].content.starts_with("Now: "));
        assert_eq!(
            messages[0].content.trim().lines().count(),
            3,
            "capped and stable in shape"
        );

        let prefix: String = provider
            .last_request()
            .system
            .iter()
            .map(|b| b.text.clone())
            .collect();
        assert!(
            !prefix.contains("Now: "),
            "the frame is turn content: in the prefix it would break the cache every turn"
        );
    }

    #[tokio::test]
    async fn a_standing_instruction_survives_compaction() {
        let provider = Arc::new(Fake::replying("ok"));
        let mut core = Loop::new(
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            Arc::new(Collector::new()),
            Arc::new(NullTokens),
            Arc::new(Stopped),
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
        // Four kept turns plus the leading frame.
        assert!(request.messages.len() <= 7, "{}", request.messages.len());
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
            Arc::new(Stopped),
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

        assert!(matches!(err, LoopError::Model(ModelError::Unauthorized(_))));
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

    /// A call that happened is an act, whatever the provider said it cost (principle 7).
    ///
    /// The event used to be suppressed when usage came back all zeros, which is exactly the state
    /// B-45 left every OpenAI turn in. So the transcript had no `cost` line to notice was missing
    /// and the ledger had no row, and the one symptom anybody could see was a meter reading zero.
    #[tokio::test]
    async fn a_call_the_provider_priced_at_nothing_is_still_recorded() {
        let silent = Fake {
            script: vec![
                Ok(Chunk::Text("Hello.".to_owned())),
                Ok(Chunk::Done(StopReason::EndTurn)),
            ],
            reject: false,
            seen: Mutex::new(Vec::new()),
        };
        let (mut core, events, _) = harness(Arc::new(silent));
        core.turn_with("hello", CancellationToken::new())
            .await
            .expect("turn");

        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, Event::ModelCall { .. })),
            "a turn with no reported usage still made a call"
        );
    }

    /// The marker opens the reply or it is not a request. Two rules meet here: a model that
    /// answers and then talks about searching has already answered, and taking that away to run a
    /// search would cost the user a reply they were owed.
    #[test]
    fn a_search_request_has_to_be_the_whole_opening() {
        assert_eq!(
            search_request("SEARCH: my degree"),
            Some("my degree".to_owned())
        );
        assert_eq!(
            search_request("  search: what Meera said\nand then some"),
            Some("what Meera said".to_owned())
        );

        // Answered first. Not a request.
        assert_eq!(
            search_request("You studied computer science. I could SEARCH: for more."),
            None
        );
        // The marker with nothing after it asks for nothing.
        assert_eq!(search_request("SEARCH:"), None);
        assert_eq!(search_request("SEARCH:   "), None);
        assert_eq!(search_request(""), None);
        // A word that merely starts the same way.
        assert_eq!(search_request("Searching my memory now."), None);
    }
}
