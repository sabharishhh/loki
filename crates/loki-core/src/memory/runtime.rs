//! The memory runtime (§10.3) and lane 2 (§10.8).
//!
//! Eight ordinary file operations, the same shapes used on your documents, plus the gate. Until
//! now every retrieval was the automatic path: the primitives existed on `Bundle` and nothing
//! exposed them, so the agent could not search its own memory at all. That was B-32, and §23's
//! failure point 81.
//!
//! **Lane 2 is not a better lane 1.** §10.2 measures both: over a well-structured store, lexical
//! retrieval is the large win and the agentic reader adds a real but smaller amount on top. So
//! this fires when lane 1 was not enough, and never instead of it.
//!
//! **The host's trigger is a floor, not the only vote.** §10.8's two conditions are checked here,
//! deterministically and with no model, and they fire before any model call. What they cannot see
//! is a hit that scores well and answers nothing: [`Score`](super::index::Score) measures how
//! strongly a line matches the words, not whether it answers the question, so a confidently wrong
//! hit would otherwise silence escalation. The caller may therefore also let the model ask. See
//! D-062.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use async_trait::async_trait;
use jiff::civil::Date;
use tokio_util::sync::CancellationToken;

use super::bundle::{Bundle, BundleError};
use super::gate::TierScope;
use super::index::{Index, IndexError, Lane, Query, Visibility};
use crate::core::vocab::ModelRole;
use crate::ports::model::{Chunk, Message, ModelProvider, Request, SystemBlock, Usage};

/// Searches one turn may run (§10.5). A hard budget, from the first day.
///
/// The narrow, look, refine loop is only bounded by something like this. Eight is enough to find
/// anything in a personal store and few enough that a runaway costs a turn rather than a bill.
pub const SEARCH_BUDGET: usize = 8;

/// The score below which lane 1 counts as not having answered (§10.8, §26 question 23).
///
/// This is why §10.1 returns an absolute score rather than only an order: a corpus-relative rank
/// cannot answer "was the best hit good enough at all". Open question 23, so it is one named
/// number, and it trades latency against completeness.
pub const ESCALATION_SCORE: f32 = 0.35;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("could not read or write the bundle: {0}")]
    Bundle(#[from] BundleError),
    #[error("could not read the index: {0}")]
    Index(#[from] IndexError),
    #[error("the search budget of {SEARCH_BUDGET} is spent")]
    OutOfBudget,
    #[error("navigation failed: {0}")]
    Navigate(String),
}

/// One call into the memory runtime (§10.3).
///
/// One tool surface, two scopes: the same operations work on the bundle as on your documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Ripgrep over the bundle.
    Grep {
        pattern: String,
        path: Option<String>,
    },
    /// Cat, or a line range.
    ///
    /// The range is most of lane 2's value: a hit can be expanded into the block around it rather
    /// than returned as an isolated line, and the block is what makes the answer checkable.
    Read {
        path: String,
        range: Option<(usize, usize)>,
    },
    Write {
        path: String,
        content: String,
    },
    Append {
        path: String,
        content: String,
    },
    Edit {
        path: String,
        old: String,
        new: String,
    },
    Ls {
        dir: String,
    },
    /// Ranked, for when grep is too literal.
    Search {
        query: String,
    },
    /// Concept ids and one-line summaries (§10.3).
    ///
    /// An entry point for a search, never the retrieval mechanism. Using summaries *as* retrieval
    /// measures 41.7 percent in §10.2's ablation; using them as a starting point is what the
    /// agentic reader beats the hybrid with. Lane 1 never sees it.
    Catalog,
}

impl Op {
    /// The one-line form a navigator writes, and what the search log records against its result.
    ///
    /// Only the six read forms round-trip through [`Op::parse`]. The writes have no surface in the
    /// navigator's grammar on purpose: a search that can edit the store is a different feature,
    /// with §14's confirms attached.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Catalog => "CATALOG".to_owned(),
            Self::Ls { dir } => format!("LS {dir}"),
            Self::Grep {
                pattern,
                path: None,
            } => format!("GREP {pattern}"),
            Self::Grep {
                pattern,
                path: Some(path),
            } => format!("GREP {pattern} under {path}"),
            Self::Search { query } => format!("SEARCH {query}"),
            Self::Read { path, range: None } => format!("READ {path}"),
            Self::Read {
                path,
                range: Some((from, to)),
            } => format!("READ {path} {from}-{to}"),
            Self::Write { path, .. } => format!("WRITE {path}"),
            Self::Append { path, .. } => format!("APPEND {path}"),
            Self::Edit { path, .. } => format!("EDIT {path}"),
        }
    }

    /// Reads one navigator line. `None` for `DONE`, and for anything unreadable.
    ///
    /// Tolerant, like [`super::consolidate::parse_candidates`] and for the same reason: a line it
    /// cannot read costs one step of the budget, and an error would cost the whole turn.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        // A bullet or a fence around the line is formatting, not part of the op. Only backticks
        // are stripped from the end: a trailing hyphen belongs to whatever pattern it is in.
        let line = line.trim().trim_start_matches(['-', '*', '`']).trim();
        let line = line.trim_end_matches('`').trim();
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim().trim_matches('`').trim();
        match verb.to_ascii_uppercase().as_str() {
            "CATALOG" => Some(Self::Catalog),
            "LS" => Some(Self::Ls {
                dir: if rest.is_empty() {
                    ".".to_owned()
                } else {
                    rest.to_owned()
                },
            }),
            "GREP" if !rest.is_empty() => Some(Self::Grep {
                pattern: rest.to_owned(),
                path: None,
            }),
            "SEARCH" if !rest.is_empty() => Some(Self::Search {
                query: rest.to_owned(),
            }),
            "READ" if !rest.is_empty() => Some(read_line(rest)),
            _ => None,
        }
    }
}

/// Splits `READ <path> <from>-<to>` from `READ <path>`, treating an unreadable range as absent.
fn read_line(rest: &str) -> Op {
    if let Some((path, tail)) = rest.rsplit_once(char::is_whitespace)
        && let Some((from, to)) = tail.split_once('-')
        && let (Ok(from), Ok(to)) = (from.trim().parse(), to.trim().parse())
    {
        return Op::Read {
            path: path.trim().to_owned(),
            range: Some((from, to)),
        };
    }
    Op::Read {
        path: rest.to_owned(),
        range: None,
    }
}

/// The memory runtime. Reads and writes the bundle, and never builds a prompt.
///
/// Getting text into a prompt is [`super::gate::Active`]'s job and only its job, which is why
/// `load` is not one of these: it returns a type, not a string.
pub struct Runtime<'a> {
    bundle: &'a Bundle,
    index: &'a Index,
    scope: TierScope,
}

impl<'a> Runtime<'a> {
    #[must_use]
    pub const fn new(bundle: &'a Bundle, index: &'a Index, scope: TierScope) -> Self {
        Self {
            bundle,
            index,
            scope,
        }
    }

    /// Runs one operation and returns what a model would read.
    ///
    /// # Errors
    /// Fails if the bundle or index cannot be reached.
    pub async fn run(&self, op: &Op, today: Date) -> Result<String, RuntimeError> {
        match op {
            Op::Grep { pattern, path } => {
                let reader = self.bundle.reader().await;
                let hits = reader.search(pattern)?;
                Ok(hits
                    .into_iter()
                    .filter(|hit| path.as_ref().is_none_or(|want| hit.path.starts_with(want)))
                    .map(|hit| format!("{}:{}: {}", hit.path, hit.line, hit.text))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            Op::Read { path, range } => {
                let reader = self.bundle.reader().await;
                let text = reader.read(path)?;
                Ok(range.map_or(text.clone(), |(from, to)| {
                    // Numbered from one and returned with the numbers, so a model reading a block
                    // can ask for the next one without counting.
                    text.lines()
                        .zip(1usize..)
                        .filter(|(_, line_no)| (from..=to).contains(line_no))
                        .map(|(line, line_no)| format!("{line_no}: {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }))
            }
            Op::Write { path, content } => {
                let writer = self.bundle.writer().await;
                writer.write(path, content)?;
                Ok(format!("wrote {path}"))
            }
            Op::Append { path, content } => {
                let writer = self.bundle.writer().await;
                writer.append(path, content)?;
                Ok(format!("appended to {path}"))
            }
            Op::Edit { path, old, new } => {
                let writer = self.bundle.writer().await;
                writer.edit(path, old, new)?;
                Ok(format!("edited {path}"))
            }
            Op::Ls { dir } => {
                let reader = self.bundle.reader().await;
                Ok(reader.ls(dir)?.join("\n"))
            }
            Op::Search { query } => {
                // Everything, not just what a prompt may carry. §10.6: a candidate is searchable
                // on lane 2 and visible on the review screen, and only lane 1 is restricted.
                let hits = self.index.recall(&Query {
                    visibility: Visibility::Everything,
                    ..Query::prefetch(query, self.scope, today, SEARCH_BUDGET)
                })?;
                Ok(hits
                    .into_iter()
                    .map(|hit| format!("{}#{}: {}", hit.path, hit.ordinal, hit.text))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            Op::Catalog => {
                let reader = self.bundle.reader().await;
                Ok(reader.read(super::bundle::INDEX).unwrap_or_default())
            }
        }
    }
}

/// Whether the message is asking about the past at all (§10.8's first condition).
///
/// Keyword matching, deliberately. A model deciding whether to search is a step it will sometimes
/// skip, and this is the cheap half of a test whose expensive half is already computed.
///
/// **Questions only.** `my ` was on this list and matched nearly every personal statement, so
/// telling Loki something armed a search on the way to storing it (B-47). A marker earns its place
/// by appearing in questions about what was said before, and in little else.
#[must_use]
pub fn asks_about_the_past(message: &str) -> bool {
    const MARKERS: [&str; 13] = [
        "remember",
        "did i",
        "did we",
        "what did",
        "do you know",
        "you said",
        "i said",
        "i told you",
        "earlier",
        "last time",
        "before",
        "previously",
        "we discussed",
    ];
    let lower = message.to_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// An explicit instruction to search memory (§10.8).
///
/// **Separate from [`asks_about_the_past`], and it ignores the score.** That one reads a question
/// for markers of the past; this reads an instruction. "Search your memory and tell me everything
/// in it" contains none of the thirteen markers, so lane 1 came back empty, the floor never fired,
/// the model was never armed to ask, and the answer was composed out of the working set and headed
/// "here is everything currently listed in memory". A subset presented as the whole store, which
/// is §10.8's silence-as-fact wearing the opposite face. B-56.
///
/// A verb **followed closely** by a memory word, never either alone and never in either order.
/// "Find me a restaurant" names no memory; "my memory is bad, I should check my calendar" has both
/// words and asks for nothing. B-47 is the record of what a loose marker list costs, and this rule
/// is the narrowest one that still catches every phrasing of the instruction.
#[must_use]
pub fn asks_to_search(message: &str) -> bool {
    const VERBS: [&str; 8] = [
        "search",
        "look",
        "find",
        "check",
        "list",
        "recall",
        "dig",
        "go through",
    ];
    const SUBJECTS: [&str; 8] = [
        "memory",
        "memories",
        "remember",
        "stored",
        "notes",
        "you know",
        "know about",
        "you have",
    ];
    /// Characters the object may sit behind the verb. Wide enough for "look *through your* notes",
    /// narrow enough that two unrelated clauses never pair up.
    const REACH: usize = 24;

    let lower = message.to_lowercase();
    VERBS.iter().any(|verb| {
        lower.match_indices(verb).any(|(at, _)| {
            let after = &lower[at + verb.len()..];
            // By characters, not bytes. A message in another script must not split a code point.
            let end = after
                .char_indices()
                .nth(REACH)
                .map_or(after.len(), |(boundary, _)| boundary);
            SUBJECTS
                .iter()
                .any(|subject| after[..end].contains(subject))
        })
    })
}

/// §10.8's two conditions, both checked here and neither by a model.
///
/// The message has to be asking about the past, *and* lane 1's best hit has to have fallen below
/// the threshold. Either alone escalates far too often: every turn mentions something, and a
/// low score on a question that is not about memory is just a question about something else.
///
/// This is the floor. It cannot see a hit that scores well and answers nothing, which is what
/// [`missed_the_subject`] and the model's own request are for.
#[must_use]
pub fn should_escalate(message: &str, best: Option<f32>) -> bool {
    asks_about_the_past(message) && best.is_none_or(|score| score < ESCALATION_SCORE)
}

/// Words too common to say anything about what a question is about.
///
/// The quantifiers matter as much as the interrogatives. "What all do you know about me" reduces
/// to `all`, which appears in nothing, so the question read as being about a subject the store had
/// never heard of and escalated a search over five perfectly good recalled facts (B-47).
const NOISE: [&str; 46] = [
    "what",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "whose",
    "why",
    "how",
    "did",
    "does",
    "do",
    "the",
    "a",
    "an",
    "is",
    "are",
    "was",
    "were",
    "my",
    "me",
    "i",
    "you",
    "your",
    "we",
    "us",
    "about",
    "tell",
    "know",
    "remember",
    "again",
    "said",
    "told",
    "that",
    "all",
    "any",
    "some",
    "anything",
    "everything",
    "something",
    "else",
    "more",
    "much",
    "many",
    "and",
    "but",
];

/// Whether lane 1 came back about something else entirely (D-062).
///
/// The score answers "how strongly did this line match", never "did it answer the question", so a
/// confident hit about the wrong subject reads exactly like a good one. This is the cheap half of
/// the difference: if not one content word of the question appears anywhere in what came back,
/// the hits are about something else and the score saying otherwise is the problem.
///
/// Deliberately one-sided. Overlap present proves nothing, so this only ever adds escalations.
#[must_use]
pub fn missed_the_subject(message: &str, hits: &[String]) -> bool {
    let content: Vec<String> = message
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 2)
        .map(str::to_lowercase)
        .filter(|word| !NOISE.contains(&word.as_str()))
        .collect();
    if content.is_empty() {
        return false;
    }
    let haystack = hits.join("\n").to_lowercase();
    !content.iter().any(|word| haystack.contains(word.as_str()))
}

/// Picks the next operation, or stops. A model in production, a script in tests.
///
/// A trait for the same reason [`super::consolidate::Extractor`] is one: a test whose setup is a
/// model call measures two things and fails for the wrong reasons.
#[async_trait]
pub trait Navigator: Send + Sync {
    /// # Errors
    /// Fails if the underlying call fails.
    async fn next(&self, question: &str, seen: &[String]) -> Result<Option<Op>, RuntimeError>;
}

/// Why a lane 2 search stopped (§10.8).
///
/// Found, empty and could-not-run are three answers rather than two, and a budget that drained
/// and a loop that went nowhere stopped for different reasons. Collapsing any of them into
/// "found nothing" is the silence-as-fact §12.4 forbids.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ending {
    /// The navigator stopped on its own.
    #[default]
    Stopped,
    /// §10.5's budget of eight went.
    OutOfBudget,
    /// Two identical progress signatures in a row (§10.5).
    Stalled,
    /// The store could not be read, so no search happened.
    Failed,
}

/// What a lane 2 search came back with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub lines: Vec<String>,
    /// How much of §10.5's budget went.
    pub searched: usize,
    /// Why the loop stopped. Only changes what is said when `lines` is empty.
    pub ending: Ending,
}

impl Found {
    /// The outcome of a search that could not run at all (§10.8).
    ///
    /// A store that refused to be read is not a store that holds nothing, and the caller has to
    /// be able to say which without inventing the distinction itself.
    #[must_use]
    pub fn failed() -> Self {
        Self {
            lines: Vec::new(),
            searched: 0,
            ending: Ending::Failed,
        }
    }

    /// What the turn carries, or why it carries nothing (§10.8).
    ///
    /// **Honest exhaustion, applied inward.** §12.4 says a page the ladder could not read is
    /// reported as unread rather than returned as an empty page. A memory search that finds
    /// nothing says so, and never lets a miss read as "you never told me that", which is the same
    /// failure pointed at the user instead of at the web.
    #[must_use]
    pub fn render(&self) -> String {
        if !self.lines.is_empty() {
            return format!("I searched my memory and found:\n{}", self.lines.join("\n"));
        }
        match self.ending {
            Ending::Stopped => {
                "I searched my memory and did not find it. That does not mean you never said it, \
                 only that I could not find it."
            }
            Ending::OutOfBudget => {
                "I searched my memory and ran out of searches before finding it. That is not the \
                 same as it not being there."
            }
            Ending::Stalled => {
                "I searched my memory and the search stopped getting anywhere before it found it. \
                 That is not the same as it not being there."
            }
            Ending::Failed => {
                "I could not read my memory just now, so I have not searched it. That is not the \
                 same as it not being there."
            }
        }
        .to_owned()
    }

    /// The same three outcomes, addressed to the model rather than to the user.
    ///
    /// Two renderings because there are two audiences and [`Found::render`] is written in Loki's
    /// own voice. Feeding that voice back in as turn content would have the model reading its own
    /// first person as something the user said.
    #[must_use]
    pub fn brief(&self) -> String {
        if !self.lines.is_empty() {
            return format!(
                "A deeper search of memory returned:\n{}",
                self.lines.join("\n")
            );
        }
        if self.ending == Ending::Failed {
            return "A deeper search of memory could not run, because the store could not be \
                    read. Say that you could not check just now. Never say the user did not tell \
                    you."
                .to_owned();
        }
        let how = match self.ending {
            Ending::OutOfBudget => {
                format!("spent all {SEARCH_BUDGET} of its searches without finding anything")
            }
            Ending::Stalled => "stopped getting anywhere and found nothing".to_owned(),
            Ending::Stopped | Ending::Failed => "found nothing".to_owned(),
        };
        format!(
            "A deeper search of memory {how}. Say that you could not find it. Never say the user \
             did not tell you."
        )
    }
}

/// Lane 2: the agent searching memory directly, under §10.5's budget (§10.8).
///
/// Starts wherever the navigator starts, which §10.8 expects to be the catalog, narrows with grep
/// and search, and reads with a line range so a hit expands into the block around it.
///
/// # Errors
/// Fails if the bundle or index cannot be reached. A navigator that stops early is not an error.
pub async fn search(
    question: &str,
    runtime: &Runtime<'_>,
    navigator: &dyn Navigator,
    today: Date,
) -> Result<Found, RuntimeError> {
    // What the navigator sees, dead ends included, and what actually answered. Separate on
    // purpose: a model narrowing a search needs to know a path was empty, and the turn does not.
    let mut seen: Vec<String> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut previous: Option<u64> = None;
    let mut ending = Ending::Stopped;

    while used < SEARCH_BUDGET {
        let Some(op) = navigator.next(question, &seen).await? else {
            break;
        };
        used += 1;
        // The step is recorded beside its result, not just the result. A navigator that cannot
        // see what it already ran repeats it, and repeating a step costs one of eight.
        let step = op.line();
        let outcome = runtime.run(&op, today).await;
        let now = signature(&step, &outcome);
        match outcome {
            Ok(output) if output.trim().is_empty() => seen.push(format!("{step}\nnothing there")),
            Ok(output) => {
                let output = clip(&output);
                seen.push(format!("{step}\n{output}"));
                found.push(output);
            }
            // A dead end is an ordinary step in a narrow, look, refine loop: a path that is not
            // there, a file that will not parse. The navigator is told and the loop continues,
            // because aborting the turn over a probe is worse than spending one of eight.
            Err(why) => seen.push(format!("{step}\nthat did not work: {why}")),
        }
        // §10.5: the budget bounds a runaway, and notices nothing about a loop going nowhere.
        // Eight searches spent re-running one grep cost the same as eight spent narrowing, and
        // only one of those is a search.
        if previous == Some(now) {
            ending = Ending::Stalled;
            break;
        }
        previous = Some(now);
    }

    // A stall is the more specific reason, so a loop that repeats itself on the last step of the
    // budget is reported as stuck rather than as thorough.
    if ending == Ending::Stopped && used >= SEARCH_BUDGET && found.is_empty() {
        ending = Ending::OutOfBudget;
    }
    Ok(Found {
        lines: found,
        searched: used,
        ending,
    })
}

/// One step's progress signature: the operation, how it ended, and how much it produced (§10.5).
///
/// **Over the raw byte count, never the rendered slice.** [`clip`] truncates, so a step producing
/// steadily more output looks stationary through the slice, and a genuinely stuck loop looks alive
/// the moment a byte of formatting changes. An error carries no output: the same operation failing
/// twice is no progress however the message is worded.
fn signature(step: &str, outcome: &Result<String, RuntimeError>) -> u64 {
    let (status, bytes) = match outcome {
        Ok(output) if output.trim().is_empty() => (0u8, 0),
        Ok(output) => (1u8, output.len()),
        Err(_) => (2u8, 0),
    };
    let mut hasher = DefaultHasher::new();
    step.hash(&mut hasher);
    status.hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Lines one observation may carry into a prompt.
///
/// A grep over a store with a busy word in it returns hundreds. Unbounded, eight of those is the
/// context window, which is the failure §10.5 avoids by putting tool output in a file and taking
/// only the slice.
const OBSERVATION_LINES: usize = 40;

fn clip(output: &str) -> String {
    let mut lines: Vec<&str> = output.lines().take(OBSERVATION_LINES).collect();
    let over = output.lines().count().saturating_sub(OBSERVATION_LINES);
    if over > 0 {
        lines.push("(more lines, not shown. narrow the search)");
    }
    lines.join("\n")
}

/// The lane a search ran on, for §10.6's log.
#[must_use]
pub const fn lane() -> Lane {
    Lane::Deliberate
}

/// What a navigator may ask for, as the model reads it.
const NAVIGATE_INSTRUCTIONS: &str = "\
You are searching one person's memory store to answer one question. The store is markdown files.

Reply with exactly one line, and nothing else:

  CATALOG                  every concept id with its one-line summary
  LS <dir>                 list a directory
  GREP <pattern>           literal search across the whole store
  SEARCH <query>           ranked search, for when grep is too literal
  READ <path> <from>-<to>  a file, or a range of its lines
  DONE                     stop

Rules:
- Start with CATALOG when you do not yet know the store's shape.
- Narrow before you read. A GREP hit gives a path and a line number, and READ with a range around
  that line gives the surrounding block, which is what makes an answer checkable.
- Never repeat a step already listed below. Its result is there, and repeating it wastes a search.
- Say DONE the moment what has been found answers the question.
- Say DONE when two different searches have both come back empty. The store does not hold it, and
  saying so is a better answer than a third guess.
- Output the line and nothing else. No explanation, no quotes, no code fence.";

/// A navigator with a model behind it (§10.8).
///
/// One bounded [`ModelRole::Utility`] call per step, over the six-word grammar in [`Op::parse`].
/// Utility rather than `Primary` because §8.1's cached conversation prefix belongs to the
/// conversation, and eight utility calls sharing it would break the cache eight times.
///
/// Usage accumulates so the caller can charge it. A search that spends money without appearing in
/// §20's ledger is exactly the silent cost the budget exists to prevent.
pub struct ModelNavigator<'a> {
    provider: &'a dyn ModelProvider,
    cancel: CancellationToken,
    usage: std::sync::Mutex<Usage>,
}

impl<'a> ModelNavigator<'a> {
    #[must_use]
    pub fn new(provider: &'a dyn ModelProvider, cancel: CancellationToken) -> Self {
        Self {
            provider,
            cancel,
            usage: std::sync::Mutex::new(Usage::default()),
        }
    }

    /// What every step of this search cost, for the caller to record.
    #[must_use]
    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl Navigator for ModelNavigator<'_> {
    async fn next(&self, question: &str, seen: &[String]) -> Result<Option<Op>, RuntimeError> {
        use futures_util::StreamExt as _;

        let mut prompt = format!("Question: {question}\n");
        if seen.is_empty() {
            prompt.push_str("\nNothing has been tried yet.");
        } else {
            prompt.push_str("\nSteps already run, and what each returned:\n\n");
            for step in seen {
                prompt.push_str(step);
                prompt.push_str("\n\n");
            }
        }

        let request = Request {
            role: ModelRole::Utility,
            system: vec![SystemBlock::new(NAVIGATE_INSTRUCTIONS)],
            messages: vec![Message::user(prompt)],
            // One line. A ceiling this low is also what stops a model that wants to explain.
            max_tokens: 64,
        };
        let mut stream = self
            .provider
            .complete(request, self.cancel.clone())
            .await
            .map_err(|e| RuntimeError::Navigate(e.to_string()))?;

        let mut answer = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk.map_err(|e| RuntimeError::Navigate(e.to_string()))? {
                Chunk::Text(piece) => answer.push_str(&piece),
                Chunk::Usage(reported) => {
                    let mut total = self.usage.lock().unwrap_or_else(|e| e.into_inner());
                    total.input_tokens += reported.input_tokens;
                    total.output_tokens += reported.output_tokens;
                    total.cache_read_tokens += reported.cache_read_tokens;
                    total.cache_write_tokens += reported.cache_write_tokens;
                }
                _ => {}
            }
        }
        // The first non-empty line only. A model that adds a sentence after the op still gets its
        // op run, and one that answers with prose stops the search rather than failing it.
        Ok(answer
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(Op::parse))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_conditions_are_needed_to_escalate() {
        // Asking about the past, and lane 1 had nothing good.
        assert!(should_escalate(
            "what did I tell you about Meera",
            Some(0.1)
        ));
        assert!(should_escalate("do you remember my degree", None));

        // Asking about the past, but lane 1 answered it well. No round trip.
        assert!(!should_escalate(
            "what did I tell you about Meera",
            Some(0.9)
        ));

        // A poor score on a question that is not about memory is a question about something else.
        assert!(!should_escalate("write me a haiku about rain", Some(0.0)));

        // A statement is not a question about the past. `my ` used to be a marker, so nearly every
        // personal sentence armed a search: "my dad is a civil contractor" spent five navigator
        // calls and nine seconds on a turn that was telling Loki something (B-47).
        assert!(!asks_about_the_past(
            "my dad is a civil contractor, he studied electronics"
        ));
        assert!(asks_about_the_past("did I tell you about my dad"));
        // The user's own phrasing, which a narrower marker missed.
        assert!(asks_about_the_past("what all do you know about my degree"));
    }

    /// B-56, from Sabharish's session. An instruction to search is not a question about the past,
    /// and none of the thirteen markers is in it, so nothing armed and nothing searched.
    #[test]
    fn an_instruction_to_search_memory_is_recognised_as_one() {
        for message in [
            "search the memory to find everything that is listed in it",
            "check your memory for what I said about my father",
            "look through your notes and tell me",
            "list everything you know about me",
            "search your memory",
            "can you recall what is stored about my degree",
        ] {
            assert!(asks_to_search(message), "should search: {message:?}");
        }
    }

    /// The half that matters more. A loose list is what B-47 cost, so both words together in the
    /// wrong order or the wrong clause must stay quiet.
    #[test]
    fn a_sentence_that_merely_contains_both_words_does_not_search() {
        for message in [
            // A verb with nothing to search.
            "find me a restaurant near Kochi",
            "check the weather",
            "list the steps for setting up a rust project",
            // A memory word nowhere near the verb, in a different clause.
            "my memory is bad, I should check my calendar",
            "I have notes everywhere and I cannot find my keys",
            // Neither.
            "my dad is a civil contractor",
            "",
        ] {
            assert!(!asks_to_search(message), "should not search: {message:?}");
        }
    }

    /// A message in another script must not panic the window, whatever it decides.
    #[test]
    fn the_window_does_not_split_a_character() {
        for message in [
            "search the நினைவகம் for my name",
            "check मेरी memory के लिए",
            "找一下 memory 里面有什么",
            "search 🧠🧠🧠🧠🧠🧠🧠🧠🧠🧠 memory",
        ] {
            let _ = asks_to_search(message);
        }
    }

    #[test]
    fn the_threshold_is_the_only_number_in_the_decision() {
        assert!(should_escalate(
            "remember this",
            Some(ESCALATION_SCORE - 0.01)
        ));
        assert!(!should_escalate("remember this", Some(ESCALATION_SCORE)));
    }

    /// The six read forms survive the trip a navigator makes them take: rendered into the search
    /// log, read back by the next step.
    #[test]
    fn the_navigable_grammar_round_trips() {
        let ops = [
            Op::Catalog,
            Op::Ls {
                dir: "people".to_owned(),
            },
            Op::Grep {
                pattern: "computer science".to_owned(),
                path: None,
            },
            Op::Search {
                query: "what I studied".to_owned(),
            },
            Op::Read {
                path: "people/sabharish.md".to_owned(),
                range: None,
            },
            Op::Read {
                path: "people/sabharish.md".to_owned(),
                range: Some((4, 20)),
            },
        ];
        for op in ops {
            assert_eq!(Op::parse(&op.line()), Some(op.clone()), "{}", op.line());
        }
    }

    /// A model does not answer in a clean line. It bullets, it fences, it lower-cases, and it
    /// explains afterwards. Every one of those still has to run, or the budget goes on formatting.
    #[test]
    fn a_line_dressed_the_way_a_model_dresses_it_still_parses() {
        assert_eq!(Op::parse("- CATALOG"), Some(Op::Catalog));
        assert_eq!(Op::parse("`catalog`"), Some(Op::Catalog));
        assert_eq!(Op::parse("  grep  Lakshmi  "), Op::parse("GREP Lakshmi"));
        assert_eq!(
            Op::parse("READ people/meera.md 3-9"),
            Some(Op::Read {
                path: "people/meera.md".to_owned(),
                range: Some((3, 9)),
            })
        );
        // A path with a number in it is a path, not a range.
        assert_eq!(
            Op::parse("READ episodes/2026-09-03.md"),
            Some(Op::Read {
                path: "episodes/2026-09-03.md".to_owned(),
                range: None,
            })
        );

        // Stopping and failing to parse are the same outcome: the search ends, nothing errors.
        assert_eq!(Op::parse("DONE"), None);
        assert_eq!(Op::parse("I think the answer is Chennai."), None);
        assert_eq!(Op::parse(""), None);
        assert_eq!(
            Op::parse("GREP"),
            None,
            "a verb with no argument is not an op"
        );
    }

    /// D-062: the score says how strongly a line matched, never whether it answered. A confident
    /// hit about someone else has to escalate anyway, or the failure is silent.
    #[test]
    fn a_confident_hit_about_the_wrong_subject_still_escalates() {
        let wrong = vec!["Meera works on the design team".to_owned()];
        assert!(
            missed_the_subject("what did Ravi tell me about the launch", &wrong),
            "nothing in the answer is about Ravi or the launch"
        );
        // The floor alone would have let this through, which is the bug.
        assert!(!should_escalate(
            "what did Ravi tell me about the launch",
            Some(0.9)
        ));

        // One shared content word is enough to stay quiet. This only ever adds escalations.
        let right = vec!["Ravi moved to the launch team".to_owned()];
        assert!(!missed_the_subject(
            "what did Ravi tell me about the launch",
            &right
        ));

        // A question with nothing but common words says nothing about its subject, so overlap
        // has no opinion and must not manufacture one. Quantifiers count as common words: a
        // vague question is vague, not about something the store has never heard of.
        assert!(!missed_the_subject("what did you know", &wrong));
        assert!(!missed_the_subject("what all do you know about me", &wrong));
        assert!(!missed_the_subject("tell me everything", &wrong));
        assert!(!missed_the_subject("", &wrong));
    }

    /// The enormous case. One grep over a store with a busy word in it must not become the
    /// context window, and what was cut has to say so or a model reads a truncation as an ending.
    #[test]
    fn an_enormous_result_is_bounded_and_admits_it() {
        let huge = (0..500)
            .map(|n| format!("people/sabharish.md:{n}: a line"))
            .collect::<Vec<_>>()
            .join("\n");
        let clipped = clip(&huge);
        assert_eq!(clipped.lines().count(), OBSERVATION_LINES + 1);
        assert!(clipped.contains("not shown"), "{clipped}");

        // Anything that fits is left exactly as it was.
        assert_eq!(clip("one\ntwo"), "one\ntwo");
    }

    /// §10.8: a miss is reported as a miss, and never as an absence.
    #[test]
    fn finding_nothing_says_so_rather_than_saying_nothing() {
        let empty = Found::default();
        assert!(empty.render().contains("did not find it"));
        assert!(
            empty.render().contains("does not mean you never said it"),
            "a miss must not read as the user never having said it"
        );

        let spent = Found {
            ending: Ending::OutOfBudget,
            searched: SEARCH_BUDGET,
            ..Found::default()
        };
        assert!(spent.render().contains("ran out of searches"));
        assert_ne!(
            spent.render(),
            empty.render(),
            "running out and finding nothing are different answers"
        );

        // §10.8's third answer. A store that refused is not a store that holds nothing, and the
        // model has to be told which before it writes a sentence about the user's memory.
        let broken = Found::failed();
        assert!(broken.render().contains("could not read my memory"));
        assert!(broken.brief().contains("could not run"));
        for other in [&empty, &spent] {
            assert_ne!(broken.brief(), other.brief());
        }
        for text in [broken.render(), broken.brief()] {
            assert!(
                !text.contains("found nothing") && !text.contains("did not find it"),
                "a search that never ran must not report a miss: {text}"
            );
        }
    }

    /// §10.5: the signature is over the raw byte count, never the slice that reaches the prompt.
    ///
    /// The interesting case is two outputs long enough to clip identically. Through the rendered
    /// slice they are the same observation, so a task producing steadily more output would look
    /// stationary and be cut off as stuck.
    #[test]
    fn a_growing_output_is_not_stagnation_even_when_the_slice_stops_growing() {
        let step = "mem_grep pattern=x path=.";
        let short = (0..OBSERVATION_LINES * 2)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let long = (0..OBSERVATION_LINES * 4)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            clip(&short),
            clip(&long),
            "the premise: both clip to the same slice"
        );
        assert_ne!(
            signature(step, &Ok(short.clone())),
            signature(step, &Ok(long)),
            "so the signature must read the raw output, not the slice"
        );

        // And the same step producing the same bytes twice is stagnation, which is the point.
        assert_eq!(
            signature(step, &Ok(short.clone())),
            signature(step, &Ok(short))
        );

        // Empty, non-empty and failed are three statuses, not two.
        let failed = signature(step, &Err(RuntimeError::Navigate("no".to_owned())));
        assert_ne!(failed, signature(step, &Ok(String::new())));
        assert_ne!(
            failed,
            signature(
                "mem_ls dir=.",
                &Err(RuntimeError::Navigate("no".to_owned()))
            )
        );
    }
}
