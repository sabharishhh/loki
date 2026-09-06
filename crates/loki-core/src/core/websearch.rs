//! Search to completion, with evidence (§12.7).
//!
//! ```text
//! discover -> extract the few worth reading -> check coverage
//!          -> refine if a claim has no source -> stop when covered or budget spent
//! ```
//!
//! **A caller of §18's bounded attempt, not a second loop.** Lane 2's memory search is the same
//! shape and the machinery was lifted out of it for exactly this. Two bounded loops with two
//! budgets is two things to keep in step, and the one that drifts is the one nobody is watching.
//!
//! **Snippets first, and this is where most of the saving is.** A search engine already wrote a
//! summary of every result. When those answer the question there is nothing to fetch, which costs
//! one request instead of six and leaves the sources just as citable, because a snippet carries the
//! URL it came from. Fetching first and reading afterwards is the expensive habit this avoids.

use std::sync::Arc;
use std::time::Duration;

use crate::core::attempt::{self, Budget, Ending};
use crate::core::ids::TaskId;
use crate::core::sink::EventSink;
use crate::core::vocab::Verdict;
use crate::memory::evidence::Evidence;
use crate::ports::clock::Clock;
use crate::ports::egress::{Egress, Outbound};
use crate::ports::search::{CancelToken, Discover, Extract, Hit, Page, SearchError};

/// What a search turned up, ready to be cited.
#[derive(Debug, Clone, Default)]
pub struct Found {
    /// Which rung of the discovery ladder answered, for §12.9's ledger.
    pub engine: &'static str,
    /// One entry per source, in the order they were used.
    pub sources: Vec<Cited>,
    /// Whether the budget ran out before the question was covered (§12.7).
    pub complete: bool,
}

/// One source, with what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    pub url: String,
    pub title: String,
    /// The span the answer may quote. A snippet when that sufficed, the article when it did not.
    pub text: String,
    pub icon: Option<String>,
    /// The icon's bytes, once fetched and stored. What the interface actually draws.
    pub icon_hash: Option<String>,
    /// Whether this came from the engine's summary or from reading the page.
    pub read: bool,
}

impl Found {
    /// What the model is shown.
    ///
    /// Numbered, because the model has to be able to say which source a sentence came from and a
    /// URL in prose is not a reference. The numbering is the citation contract.
    #[must_use]
    pub fn brief(&self) -> String {
        if self.sources.is_empty() {
            return "The web was searched and nothing readable came back.".to_owned();
        }
        let mut out = String::from(
            "# From the web\n\nCite a source by its number, like [1]. \
             Every factual sentence needs one. Some entries below are titles a search returned and \
             were never opened: they are marked, they are not evidence, and citing one is worse \
             than citing nothing. If a claim has no source here, say so.\n\n",
        );
        for (at, source) in self.sources.iter().enumerate() {
            // **The numbering runs over every source, opened or not.** The interface lists the same
            // sources in the same order, so skipping one here would shift every marker after it and
            // point the reader's clicks at the wrong page.
            let body = source.text.trim();
            let shown = if body.is_empty() {
                "not opened, nothing read from this page".to_owned()
            } else {
                attempt::clip(body)
            };
            out.push_str(&format!(
                "[{}] {} ({})\n{shown}\n\n",
                at + 1,
                source.title,
                source.url,
            ));
        }
        if !self.complete {
            out.push_str("The search ran out of budget before it was finished. Say so.\n");
        }
        out
    }
}

/// How long a fetched page answers for (§12.7).
///
/// **Short on purpose, because the risk is asymmetric.** A cache hit saves a few seconds; a stale
/// hit answers "what happened today" with yesterday and reads exactly like a fresh answer, which is
/// the failure §21.5 exists to catch and the one B-72 already cost once. Fifteen minutes covers
/// what caching is actually for here, a follow-up question and a refinement loop re-reading the
/// same page, and cannot serve yesterday's news.
const FRESH_FOR: Duration = Duration::from_secs(900);

/// Everything the loop needs from the outside.
pub struct Search {
    /// Cheapest first, exactly like `rungs`. §12.2 puts discovery on `wreq` from the user's own
    /// machine and keeps the browser behind it: a search that needs no browser should not start
    /// one.
    pub engines: Vec<Arc<dyn Discover>>,
    /// Cheapest first. The loop stops at the first rung that answers (§12.2).
    pub rungs: Vec<Arc<dyn Extract>>,
    pub clock: Arc<dyn Clock>,
    pub budget: Budget,
    /// Pages worth reading when the snippets do not answer it.
    pub reads: usize,
    /// Where fetched bytes are cached, when there is a store (§12.7).
    pub evidence: Option<Arc<Evidence>>,
    /// The one exit, for the icons. Never a favicon service (§21.7).
    pub egress: Option<Arc<dyn Egress>>,
    /// Where the search says what it is doing, while it is doing it (§12.9).
    ///
    /// **A sink rather than a report at the end, and the difference is the whole experience.** The
    /// loop used to hand back a `Found` and the caller emitted an event per source afterwards, so
    /// every step of a seventeen second search landed in one frame once it was over and the trace
    /// had nothing to say for the seventeen seconds that mattered.
    pub events: Option<Arc<dyn EventSink>>,
}

impl Search {
    /// Says a page was reached, as it is reached.
    /// Says which engine answered and with how many hits, before any page is read.
    fn discovered(&self, task: TaskId, question: &str, engine: &str, hits: usize) {
        let Some(events) = self.events.as_ref() else {
            return;
        };
        events.emit(&crate::core::event::Event::Searched {
            task,
            query: question.to_owned(),
            provider: engine.to_owned(),
            hits: u32::try_from(hits).unwrap_or(u32::MAX),
            cost: crate::core::vocab::CostModel::Free,
        });
    }

    /// Every engine on the ladder, for the ledger.
    #[must_use]
    pub fn engine_ids(&self) -> Vec<&'static str> {
        self.engines.iter().map(|engine| engine.id()).collect()
    }

    /// Walks the discovery ladder and returns the first rung that found anything.
    ///
    /// **No cleverness on purpose.** A rung either comes back with pages or it does not, and the
    /// one behind it is tried when it does not. There is no classifier deciding in advance which
    /// rung suits a question, because a wrong guess there produces a confident answer about the
    /// wrong thing rather than a slow one.
    ///
    /// # Errors
    /// Fails when no rung could answer, carrying the last real failure rather than a bare empty.
    async fn find(
        &self,
        question: &str,
        cancel: CancelToken,
    ) -> Result<(&'static str, Vec<crate::ports::search::Hit>), SearchError> {
        let mut last: Option<SearchError> = None;
        let mut tried: Vec<&'static str> = Vec::new();

        for engine in &self.engines {
            tried.push(engine.id());
            match engine.search(question, cancel.clone()).await {
                Ok(hits) if !hits.is_empty() => return Ok((engine.id(), hits)),
                Ok(_) => {}
                // Remembered in case nothing below works either, so the caller is told why rather
                // than told the web was empty.
                Err(why) => last = Some(why),
            }
        }

        Err(last.unwrap_or(SearchError::SilentlyEmpty {
            engine: tried.join(", "),
        }))
    }

    /// Runs one search to completion, or to the budget.
    ///
    /// # Errors
    /// Fails only when the engine itself could not be used. A page that could not be read is a
    /// missing source, not a failed search (§12.4).
    pub async fn run(
        &self,
        question: &str,
        task: TaskId,
        cancel: CancelToken,
    ) -> Result<Found, SearchError> {
        let (engine, hits) = self.find(question, cancel.clone()).await?;
        // **Between finding and reading, because reading is what costs.** The budget reads three
        // of these and the engine ordered them for its own reasons, so which three is most of the
        // answer's quality (§12.7).
        let hits = crate::core::rank::best_first(question, hits);
        // Said now, not at the end: the reader is already waiting and this is the first thing
        // there is to tell them.
        self.discovered(task, question, engine, hits.len());

        // The engine's own summaries, which are free and already carry their URLs.
        let mut sources: Vec<Cited> = hits
            .iter()
            .take(self.reads.max(1) * 2)
            .map(|hit| Cited {
                url: hit.url.clone(),
                title: hit.title.clone(),
                text: hit.snippet.clone(),
                icon: None,
                icon_hash: None,
                read: false,
            })
            .collect();

        if answered_by_snippets(question, &sources) {
            self.fetch_icons(&mut sources, cancel.clone()).await;
            return Ok(Found {
                engine,
                sources,
                complete: true,
            });
        }

        let cancel_for_icons = cancel.clone();
        let plan = Reading {
            question: question.to_owned(),
            hits,
            rungs: self.rungs.clone(),
            cancel,
            evidence: self.evidence.clone(),
            events: self.events.clone(),
            task,
        };
        let outcome = attempt::run(&plan, self.budget, self.clock.as_ref())
            .await
            .map_err(|e| SearchError::Unreachable(e.to_string()))?;

        for record in &outcome.found {
            if let Some(cited) = Cited::from_record(record) {
                // A page that was read replaces the snippet that stood in for it.
                sources.retain(|existing| existing.url != cited.url);
                sources.insert(0, cited);
            }
        }
        sources.truncate(self.reads.max(1) * 2);

        self.fetch_icons(&mut sources, cancel_for_icons).await;
        Ok(Found {
            engine,
            sources,
            complete: outcome.ending == Ending::Stopped,
        })
    }

    /// Fetches each site's own icon through the exit and stores it (§12.7).
    ///
    /// **From the page that was already read, and through the same door as everything else.** A
    /// favicon service would be told every site the user reads, and a fetch from the interface
    /// would be a second way out of the process. One icon per host, because a search that returned
    /// four pages from one site should not ask it for the same file four times.
    ///
    /// Never fails a search. An answer with a letter where an icon would be is an answer; an answer
    /// withheld because an icon did not load is not.
    async fn fetch_icons(&self, sources: &mut [Cited], cancel: CancelToken) {
        let (Some(evidence), Some(egress)) = (self.evidence.as_ref(), self.egress.as_ref()) else {
            return;
        };
        let mut seen: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for source in sources.iter_mut() {
            let Some(url) = source.icon.clone() else {
                continue;
            };
            if let Some(known) = seen.get(&url) {
                source.icon_hash.clone_from(known);
                continue;
            }
            let hash = fetch_icon(egress.as_ref(), evidence, &url, cancel.clone()).await;
            seen.insert(url, hash.clone());
            source.icon_hash = hash;
        }
    }
}

/// One icon, fetched and stored, or `None` if anything went wrong.
async fn fetch_icon(
    egress: &dyn Egress,
    evidence: &Evidence,
    url: &str,
    cancel: CancelToken,
) -> Option<String> {
    use futures_util::StreamExt as _;
    let mut landed = egress
        .send(Outbound::get(url).as_browser(), cancel)
        .await
        .ok()?;
    if !(200..300).contains(&landed.status) {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(Ok(chunk)) = landed.body.next().await {
        bytes.extend_from_slice(&chunk);
        // An icon is small. A host answering a favicon request with a megabyte is either wrong or
        // hostile, and either way it is not going in the store.
        if bytes.len() > 256 * 1024 {
            return None;
        }
    }
    (!bytes.is_empty())
        .then(|| evidence.put(&bytes).ok())
        .flatten()
        .map(|hash| hash.as_str().to_owned())
}

/// Whether the engine's summaries already answer the question.
///
/// **Deliberately shallow.** A model deciding this would cost a call to save a call, and the
/// judgement worth making cheaply is only whether there is enough text to work with at all. Getting
/// it wrong costs one fetch, which is the cheapest mistake in this file.
fn answered_by_snippets(question: &str, sources: &[Cited]) -> bool {
    // Interrogatives are dropped, because they are the one class of word that reliably does *not*
    // appear in the answer: nothing that tells you when Rust reached 1.0 contains the word "when".
    // Requiring them made this permanently false, which the test caught before use did.
    const ASKING: [&str; 14] = [
        "when", "what", "where", "which", "whose", "does", "did", "is", "are", "was", "were",
        "how", "why", "who",
    ];
    let words: Vec<String> = question
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| word.len() > 3 && !ASKING.contains(&word.as_str()))
        .collect();
    if words.is_empty() || sources.len() < 2 {
        return false;
    }
    let haystack = sources
        .iter()
        .map(|source| source.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    // Every content word of the question appears in the summaries, and there is enough text that
    // it is not appearing by accident.
    haystack.len() > 240 && words.iter().all(|word| haystack.contains(word))
}

/// The reading half, as steps the bounded attempt can run.
/// The text of a record, without the url, title and icon `record` writes above it.
fn body_of(record: &str) -> &str {
    record.splitn(4, '\n').nth(3).unwrap_or("")
}

/// Why the reading loop stopped, in §12.2's shape: a reason, never a count.
///
/// **A number cannot be matched on and cannot say why.** `reads: 3` reports nothing about whether
/// three was too few or five too many, and no caller can act on it. This can, and the ledger can
/// record it (§12.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gathered {
    /// Every subject is covered, and more than one page said something about them.
    Answered,
    /// Something the question asked about is still missing.
    Wanting,
}

/// Whether what has been read covers what was asked.
///
/// **Depth falls out of the question rather than being guessed at.** The stopping condition is
/// that the question's own subjects are present in the evidence, so a question with one subject is
/// covered by one page and a comparison of three keeps going until all three are there. Nothing
/// classifies the question's complexity; the complexity is read off its structure, which is the
/// same move the compiler makes deriving a lifetime from the code rather than asking for it.
///
/// **One page is never the answer, and that is not a counter (B-88).** The subjects are the words
/// the engine matched on, so every result it returns contains them by construction and a check
/// built on them alone cannot fail: CoinMarketCap's evergreen article says "bitcoin" and "price" on
/// every line and carries no price, and the loop stopped on it. Corroboration is the missing half.
/// A second page that also speaks to the subjects is what makes the first one evidence rather than
/// a hit.
///
/// Lexical and local, because a model call per page is the cost this exists to avoid.
fn gathered(question: &str, read: &[String]) -> Gathered {
    // **What the question asks *for* is not what it asks *about*.** "compare X with Y" wants X and
    // Y; "compare" itself appears in no article about either, so requiring it meant a comparison
    // could never be satisfied and always spent the whole budget. The list is closed and short
    // because these are the verbs a request is made with, not the things requests are about.
    const INSTRUCTIONS: [&str; 18] = [
        "compare",
        "contrast",
        "explain",
        "describe",
        "summarise",
        "summarize",
        "list",
        "tell",
        "give",
        "show",
        "write",
        "find",
        "search",
        "look",
        "check",
        "refer",
        "verify",
        "answer",
    ];
    let wanted: Vec<String> = crate::core::rank::content_words(question)
        .into_iter()
        .filter(|word| !INSTRUCTIONS.contains(&word.as_str()))
        .collect();
    if wanted.is_empty() || read.is_empty() {
        return Gathered::Wanting;
    }
    // A page counts towards corroboration when it says something about what was asked, not merely
    // when the fetch returned bytes: two blocked pages are not two sources.
    let speaking = read
        .iter()
        .filter(|record| {
            let body = body_of(record).to_lowercase();
            wanted.iter().any(|word| body.contains(word.as_str()))
        })
        .count();
    if speaking < 2 {
        return Gathered::Wanting;
    }
    // **The page's text, never its title or its address.** A record opens with the url and the
    // title, and a search result's title echoes the query almost by construction: "kerala news
    // today" is covered by a page called "Kerala News Today" before a word of it has been read.
    // Judging on the title is judging on the search engine's own paraphrase of the question.
    let haystack = read
        .iter()
        .map(|record| body_of(record))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    if wanted.iter().all(|word| haystack.contains(word.as_str())) {
        Gathered::Answered
    } else {
        Gathered::Wanting
    }
}

struct Reading {
    /// What was asked, so the loop can tell when it has been answered.
    question: String,
    hits: Vec<Hit>,
    rungs: Vec<Arc<dyn Extract>>,
    cancel: CancelToken,
    /// Where a page read lately is found again (§12.7).
    evidence: Option<Arc<Evidence>>,
    events: Option<Arc<dyn EventSink>>,
    task: TaskId,
}

impl Reading {
    /// Says a page was reached, from inside the loop that reached it.
    fn reached(&self, url: &str, rung: crate::core::vocab::Rung, verdict: Verdict) {
        let Some(events) = self.events.as_ref() else {
            return;
        };
        events.emit(&crate::core::event::Event::Fetched {
            task: self.task,
            url: url.to_owned(),
            hash: crate::core::ids::ContentHash::new(""),
            rung,
            verdict,
            cost: crate::core::vocab::CostModel::Free,
        });
    }
}

#[async_trait::async_trait]
impl attempt::Steps for Reading {
    type Step = Hit;
    type Error = SearchError;

    async fn next(&self, seen: &[String]) -> Result<Option<Hit>, SearchError> {
        // **Stop when the question is answered, not when a counter runs out (§12.7).** `reads` was
        // a target, so "who is the president of India" cost the same six pages as a question that
        // genuinely needs a survey. It is a ceiling now, and the floor is sufficiency.
        if let Gathered::Answered = gathered(&self.question, seen) {
            return Ok(None);
        }
        Ok(self
            .hits
            .iter()
            .find(|hit| !seen.iter().any(|record| record.contains(&hit.url)))
            .cloned())
    }

    fn describe(&self, step: &Hit) -> String {
        format!("read {}", step.url)
    }

    async fn run(&self, step: &Hit) -> Result<String, SearchError> {
        // **Before the ladder, not inside it.** A page read minutes ago is the cheapest rung there
        // is, and the expensive part of a search is reading pages rather than finding them.
        if let Some(evidence) = &self.evidence
            && let Some(held) = evidence.recall(&step.url, FRESH_FOR)
            && let Ok(record) = String::from_utf8(held)
        {
            // A page answered from the cache still happened, and a trace that skipped it would
            // show a search reading fewer pages than it used.
            self.reached(&step.url, crate::core::vocab::Rung::Direct, Verdict::Ok);
            return Ok(record);
        }

        let mut last = None;
        for rung in &self.rungs {
            let page = rung.read(&step.url, self.cancel.clone()).await?;
            // The ladder stops climbing on a page that does not exist or a host asking to be left
            // alone. Trying harder there is what turns a soft flag into a ban (§12.2).
            self.reached(&step.url, rung.rung(), page.verdict);
            if page.verdict == Verdict::Ok {
                let record = record(&page);
                // Never fails a search. A page that could not be cached is a page that will be
                // fetched again, which is slower and not wrong.
                if let Some(evidence) = &self.evidence {
                    let _ = evidence.remember(&step.url, record.as_bytes());
                }
                return Ok(record);
            }
            if !page.verdict.should_escalate() {
                return Err(SearchError::Unreadable(format!(
                    "{} said {:?}",
                    step.url, page.verdict
                )));
            }
            last = Some(page);
        }
        Err(SearchError::Unreadable(format!(
            "{} was not readable by any rung, last said {:?}",
            step.url,
            last.map(|page| page.verdict)
        )))
    }
}

/// A page as one record, so the attempt can carry it in `seen` without knowing what it is.
///
/// The attempt is deliberately stringly at its boundary so it can serve memory search and web
/// search without knowing either. The cost is this pair of functions, which is the smallest place
/// to put the knowledge back.
fn record(page: &Page) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        page.url,
        page.title,
        page.icon.clone().unwrap_or_default(),
        page.text
    )
}

impl Cited {
    fn from_record(record: &str) -> Option<Self> {
        let mut lines = record.splitn(4, '\n');
        let url = lines.next()?.to_owned();
        if !url.starts_with("http") {
            return None;
        }
        let title = lines.next().unwrap_or_default().to_owned();
        let icon = lines.next().unwrap_or_default();
        Some(Self {
            url,
            title,
            text: lines.next().unwrap_or_default().to_owned(),
            icon: (!icon.is_empty()).then(|| icon.to_owned()),
            icon_hash: None,
            read: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(url: &str, text: &str) -> Cited {
        Cited {
            url: url.to_owned(),
            title: "t".to_owned(),
            text: text.to_owned(),
            icon: None,
            icon_hash: None,
            read: false,
        }
    }

    /// The saving: when the engine already answered, nothing is fetched.
    #[test]
    fn summaries_that_cover_the_question_stop_the_search() {
        let long = "The Rust programming language reached version 1.0 in 2015 and is maintained by \
                    the Rust Foundation, with a release every six weeks on a train model that has \
                    not slipped since it began. ";
        let sources = vec![
            snippet("https://a.test", long),
            snippet("https://b.test", long),
        ];
        assert!(answered_by_snippets("when did rust reach 1.0", &sources));
    }

    /// One summary is not corroboration, and a question whose words are missing is not answered.
    #[test]
    fn thin_or_unrelated_summaries_do_not() {
        let long = "A page about gardening in the spring, at some length, covering soil and \
                    drainage and the sort of thing that fills a summary without saying anything \
                    about the question that was asked of it at all. ";
        let two = vec![
            snippet("https://a.test", long),
            snippet("https://b.test", long),
        ];
        assert!(!answered_by_snippets("when did rust reach 1.0", &two));
        // A single source never counts, however complete it looks.
        assert!(!answered_by_snippets("gardening soil drainage", &two[..1]));
        // Nor does a pile of near-empty ones.
        let thin = vec![
            snippet("https://a.test", "rust 1.0"),
            snippet("https://b.test", "rust 1.0"),
        ];
        assert!(!answered_by_snippets("when did rust reach 1.0", &thin));
    }

    #[test]
    fn a_page_survives_the_round_trip_through_the_attempt() {
        let page = Page {
            url: "https://example.com/a".to_owned(),
            title: "A Title".to_owned(),
            text: "Line one\nLine two".to_owned(),
            icon: Some("https://example.com/f.ico".to_owned()),
            rung: crate::core::vocab::Rung::Direct,
            verdict: Verdict::Ok,
        };
        let back = Cited::from_record(&record(&page)).expect("a record");
        assert_eq!(back.url, page.url);
        assert_eq!(back.title, page.title);
        assert_eq!(back.icon, page.icon);
        // Multi-line bodies must not be truncated by the record format.
        assert_eq!(back.text, page.text);
        assert!(back.read);
    }

    /// The attempt carries dead ends too, and a dead end is not a source.
    #[test]
    fn a_record_that_is_not_a_page_is_not_cited() {
        for not_a_page in ["", "read https://example.com", "no such thing"] {
            assert!(Cited::from_record(not_a_page).is_none(), "{not_a_page}");
        }
    }

    #[test]
    fn a_brief_numbers_its_sources_and_says_when_it_is_short() {
        let found = Found {
            engine: "test",
            sources: vec![
                snippet("https://a.test", "first"),
                snippet("https://b.test", "second"),
            ],
            complete: false,
        };
        let brief = found.brief();
        assert!(brief.contains("[1] t (https://a.test)"));
        assert!(brief.contains("[2] t (https://b.test)"));
        // §12.7: an answer that ran out of budget says so rather than reading as finished.
        assert!(brief.contains("ran out of budget"));

        let nothing = Found::default();
        assert!(nothing.brief().contains("nothing readable"));
    }
}

#[cfg(test)]
mod sufficiency {
    use super::*;

    /// A record as `record` writes one: url, title, icon, then the text.
    fn page(url: &str, title: &str, text: &str) -> String {
        format!("{url}\n{title}\n\n{text}")
    }

    /// **A title is the engine's paraphrase of the question, not an answer to it.** Judging on the
    /// record whole let a page called "Kerala News Today" satisfy "kerala news today" before a word
    /// of it had been read, which would stop every search on its first result.
    #[test]
    fn a_title_that_echoes_the_question_is_not_coverage() {
        let read = vec![page(
            "https://example.com/kerala-monsoon-rainfall",
            "Kerala Monsoon Rainfall",
            "This page could not be displayed.",
        )];
        assert_eq!(
            gathered("kerala monsoon rainfall", &read),
            Gathered::Wanting
        );
    }

    /// One subject, two pages. This is the "who is the president of India" case, which cost six.
    ///
    /// **The first version of this test asserted one page was enough, and it was wrong.** The rule
    /// that satisfied it also satisfied a page with no answer on it (B-88), because a result the
    /// engine returned contains the question's words either way. Two pages that both speak to the
    /// subject is the cheapest thing that is actually evidence.
    #[test]
    fn a_single_subject_is_covered_once_a_second_page_agrees() {
        let first = page(
            "https://presidentofindia.gov.in/",
            "The President of India",
            "Droupadi Murmu is the president of India.",
        );
        assert_eq!(
            gathered(
                "who is the president of india",
                std::slice::from_ref(&first)
            ),
            Gathered::Wanting
        );

        let second = page(
            "https://en.wikipedia.org/wiki/President_of_India",
            "President of India",
            "The president of India is Droupadi Murmu, in office since 2022.",
        );
        assert_eq!(
            gathered("who is the president of india", &[first, second]),
            Gathered::Answered
        );
    }

    /// **Depth comes from the question, not from a setting.** Three subjects keep the loop going
    /// until all three are there, without anything having classified the question as complex.
    #[test]
    fn a_comparison_keeps_reading_until_every_subject_is_there() {
        let question = "compare weathernext with graphcast and fourcastnet";
        let one = vec![page(
            "https://a.example",
            "Models",
            "WeatherNext 3 produces hourly global forecasts.",
        )];
        assert_eq!(gathered(question, &one), Gathered::Wanting);

        let two = vec![
            one[0].clone(),
            page(
                "https://b.example",
                "More",
                "GraphCast is a DeepMind model.",
            ),
        ];
        assert_eq!(gathered(question, &two), Gathered::Wanting);

        let all = vec![
            two[0].clone(),
            two[1].clone(),
            page("https://c.example", "More", "FourCastNet came from NVIDIA."),
        ];
        assert_eq!(gathered(question, &all), Gathered::Answered);
    }

    /// Nothing read yet is never enough, however the question is worded.
    #[test]
    fn an_empty_reading_is_never_answered() {
        assert_eq!(gathered("anything at all", &[]), Gathered::Wanting);
    }

    /// A question with no content words cannot be judged covered, so the budget decides instead of
    /// a vacuous truth stopping the loop on the first page.
    #[test]
    fn a_question_with_no_subject_does_not_stop_the_loop() {
        let read = vec![page("https://a.example", "Title", "Something was read.")];
        assert_eq!(gathered("what is it", &read), Gathered::Wanting);
    }

    /// B-88, taken from the log verbatim. This is the page Loki read and stopped on.
    ///
    /// It is CoinMarketCap's evergreen "what is Bitcoin" article, and it says "price" and "bitcoin"
    /// on every line without carrying a price at all. The engine returned it *because* it says
    /// those words, so the question's own words cannot be the test of whether it was answered.
    #[test]
    fn a_page_full_of_the_questions_words_is_not_an_answer() {
        let read = vec![page(
            "https://coinmarketcap.com/currencies/bitcoin/",
            "Bitcoin price today, BTC to USD live price",
            "Bitcoin price was $0 when first introduced, and most Bitcoins were obtained via \
             mining. In July 2010, Bitcoin first started trading, with the Bitcoin price ranging \
             from $0.0008 to $0.08 at that time. Mining Bitcoins can be very profitable for \
             miners, depending on the current hash rate and the price of Bitcoin.",
        )];
        assert_eq!(
            gathered("what is the price of bitcoin", &read),
            Gathered::Wanting,
            "one page echoing the question is not an answer to it"
        );
    }

    /// A fetch that returned bytes is not a source. Two blocked pages are not corroboration.
    #[test]
    fn a_page_that_says_nothing_does_not_corroborate() {
        let read = vec![
            page(
                "https://a.example",
                "The President of India",
                "Droupadi Murmu is the president of India.",
            ),
            page(
                "https://b.example",
                "Just a moment",
                "Enable JavaScript to continue.",
            ),
        ];
        assert_eq!(
            gathered("who is the president of india", &read),
            Gathered::Wanting
        );
    }

    /// A page about something else does not count as covering the question.
    #[test]
    fn reading_the_wrong_thing_is_not_coverage() {
        let read = vec![page(
            "https://a.example",
            "Sport",
            "Cricket scores from the weekend.",
        )];
        assert_eq!(
            gathered("kerala monsoon rainfall", &read),
            Gathered::Wanting
        );
    }
}
