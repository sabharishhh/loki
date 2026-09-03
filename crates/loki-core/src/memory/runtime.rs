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
//! **Nothing here decides to escalate.** §10.8's two conditions are checked by the host, on the
//! absolute score §10.1 already returns. A model deciding whether to search is a step it will
//! sometimes skip, which is the same argument §10.1 makes for pre-fetch not being a tool call.

use async_trait::async_trait;
use jiff::civil::Date;

use super::bundle::{Bundle, BundleError};
use super::gate::TierScope;
use super::index::{Index, IndexError, Lane, Query, Visibility};

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
#[must_use]
pub fn asks_about_the_past(message: &str) -> bool {
    const MARKERS: [&str; 14] = [
        "remember",
        "did i",
        "did we",
        "what did",
        "what do you know",
        "you said",
        "i said",
        "i told you",
        "earlier",
        "last time",
        "before",
        "previously",
        "we discussed",
        "my ",
    ];
    let lower = message.to_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// §10.8's two conditions, both checked here and neither by a model.
///
/// The message has to be asking about the past, *and* lane 1's best hit has to have fallen below
/// the threshold. Either alone escalates far too often: every turn mentions something, and a
/// low score on a question that is not about memory is just a question about something else.
#[must_use]
pub fn should_escalate(message: &str, best: Option<f32>) -> bool {
    asks_about_the_past(message) && best.is_none_or(|score| score < ESCALATION_SCORE)
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

/// What a lane 2 search came back with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub lines: Vec<String>,
    /// How much of §10.5's budget went.
    pub searched: usize,
    /// True when the budget ran out with nothing found, as opposed to finding nothing.
    pub out_of_budget: bool,
}

impl Found {
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
        if self.out_of_budget {
            return "I searched my memory and ran out of searches before finding it. \
                    That is not the same as it not being there."
                .to_owned();
        }
        "I searched my memory and did not find it. That does not mean you never said it, \
         only that I could not find it."
            .to_owned()
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

    while used < SEARCH_BUDGET {
        let Some(op) = navigator.next(question, &seen).await? else {
            break;
        };
        used += 1;
        match runtime.run(&op, today).await {
            Ok(output) if output.trim().is_empty() => seen.push("nothing there".to_owned()),
            Ok(output) => {
                seen.push(output.clone());
                found.push(output);
            }
            // A dead end is an ordinary step in a narrow, look, refine loop: a path that is not
            // there, a file that will not parse. The navigator is told and the loop continues,
            // because aborting the turn over a probe is worse than spending one of eight.
            Err(why) => seen.push(format!("that did not work: {why}")),
        }
    }

    let out_of_budget = used >= SEARCH_BUDGET && found.is_empty();
    Ok(Found {
        lines: found,
        searched: used,
        out_of_budget,
    })
}

/// The lane a search ran on, for §10.6's log.
#[must_use]
pub const fn lane() -> Lane {
    Lane::Deliberate
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
    }

    #[test]
    fn the_threshold_is_the_only_number_in_the_decision() {
        assert!(should_escalate(
            "remember this",
            Some(ESCALATION_SCORE - 0.01)
        ));
        assert!(!should_escalate("remember this", Some(ESCALATION_SCORE)));
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
            out_of_budget: true,
            searched: SEARCH_BUDGET,
            ..Found::default()
        };
        assert!(spent.render().contains("ran out of searches"));
        assert_ne!(
            spent.render(),
            empty.render(),
            "running out and finding nothing are different answers"
        );
    }
}
