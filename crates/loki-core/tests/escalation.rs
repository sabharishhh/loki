//! When a deeper search of memory fires, who decided, and what the user sees (§10.8, D-062).
//!
//! The unit tests in `memory::runtime` cover the grammar and the two deterministic conditions.
//! This suite is about the wiring: that lane 2 actually runs in a turn, that the navigator's
//! tokens are charged, and that a model asking to search never leaks the asking.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream;
use jiff::civil::date;
use loki_core::adapters::clock::SystemClock;
use loki_core::core::budget::Budget as Spend;
use loki_core::core::cycle::{Loop, TokenSink};
use loki_core::core::event::Event;
use loki_core::core::prompt::Prefix;
use loki_core::core::sink::Broadcast;
use loki_core::core::sink::EventSink;
use loki_core::core::vocab::{Cents, CostModel, Lane, Locality, ModelRole};
use loki_core::memory::claim::Origin;
use loki_core::memory::consolidate::{Candidate, ConsolidateError, Extractor, Unbounded};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::{Memory, Speaker};
use loki_core::memory::index::{Candidate as EntityCandidate, Index};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};
use loki_core::ports::model::{
    Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request, StopReason, ToolSupport, Usage,
};
use loki_core::ports::search::{CancelToken, Discover, Hit, SearchError};
use tokio_util::sync::CancellationToken;

/// One provider serving both roles: the conversation on `Primary`, the navigator on `Utility`.
///
/// Scripted per role, because that is the only way to tell which of the two decided to search.
/// Each list runs down and then repeats its last entry, so a test says what it cares about and
/// nothing more.
struct Scripted {
    answers: Mutex<Vec<String>>,
    steps: Mutex<Vec<String>>,
    requests: Mutex<Vec<Request>>,
    /// When set, every navigator call fails. The store is then unreadable, not empty.
    navigator_fails: bool,
}

impl Scripted {
    fn new(answers: &[&str], steps: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().rev().map(|s| (*s).to_owned()).collect()),
            steps: Mutex::new(steps.iter().rev().map(|s| (*s).to_owned()).collect()),
            requests: Mutex::new(Vec::new()),
            navigator_fails: false,
        })
    }

    fn breaking_the_navigator(answers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().rev().map(|s| (*s).to_owned()).collect()),
            steps: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            navigator_fails: true,
        })
    }

    fn requests(&self, role: ModelRole) -> Vec<Request> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| r.role == role)
            .cloned()
            .collect()
    }
}

fn take(from: &Mutex<Vec<String>>, fallback: &str) -> String {
    let mut left = from.lock().expect("lock");
    match left.len() {
        0 => fallback.to_owned(),
        1 => left[0].clone(),
        _ => left.pop().expect("non-empty"),
    }
}

#[async_trait]
impl ModelProvider for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::Cloud,
            prompt_cache: true,
            max_context: 200_000,
            tools: ToolSupport::None,
            // Priced, so a navigator call that is never charged shows up as a zero.
            cost: CostModel::PerToken {
                input_per_mtok: Cents::new(300),
                output_per_mtok: Cents::new(1_500),
            },
        }
    }

    async fn complete(
        &self,
        req: Request,
        _cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        let reply = match req.role {
            ModelRole::Utility if self.navigator_fails => {
                self.requests.lock().expect("lock").push(req);
                return Err(ModelError::Upstream {
                    status: 503,
                    body: "the index is unavailable".to_owned(),
                });
            }
            ModelRole::Utility => take(&self.steps, "DONE"),
            ModelRole::Primary => take(&self.answers, "Noted."),
        };
        self.requests.lock().expect("lock").push(req);
        // One character at a time, because the hold-back that hides a search request has to work
        // against a real token cadence and not against one chunk carrying the whole reply.
        let mut chunks: Vec<Result<Chunk, ModelError>> = reply
            .chars()
            .map(|c| Ok(Chunk::Text(c.to_string())))
            .collect();
        chunks.push(Ok(Chunk::Usage(Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        })));
        chunks.push(Ok(Chunk::Done(StopReason::EndTurn)));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

/// Everything that reached the interface, in order.
#[derive(Default)]
struct Tape(Mutex<String>);

impl Tape {
    fn shown(&self) -> String {
        self.0.lock().expect("lock").clone()
    }
}

impl TokenSink for Tape {
    fn token(&self, text: &str) {
        self.0.lock().expect("lock").push_str(text);
    }
}

#[derive(Default)]
struct Collector(Mutex<Vec<Event>>);

impl Collector {
    fn all(&self) -> Vec<Event> {
        self.0.lock().expect("lock").clone()
    }
}

impl EventSink for Collector {
    fn emit(&self, event: &Event) {
        self.0.lock().expect("lock").push(event.clone());
    }
}

struct OneFact;

#[async_trait]
impl Extractor for OneFact {
    async fn extract(
        &self,
        _episode: &str,
        text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        if !text.contains("computer science") {
            return Ok(vec![]);
        }
        Ok(vec![Candidate {
            surface: "Sabharish".to_owned(),
            kind: Kind::Person,
            heading: "education".to_owned(),
            attribute: "education".to_owned(),
            text: "Sabharish is a computer science graduate".to_owned(),
            days_ago: None,
            valid_from: Some(date(2026, 1, 1)),
            origin: Origin::Stated,
            tags: vec![],
            aliases: vec![],
            value: None,

            relation: None,
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
        _kind: Kind,
        candidates: &[EntityCandidate],
    ) -> Result<Decision, ResolveError> {
        Ok(if candidates.is_empty() {
            Decision::New
        } else {
            Decision::Existing(0)
        })
    }
}

/// A store with one fact in it, already consolidated, plus a loop pointed at it.
struct Fixture {
    core: Loop,
    provider: Arc<Scripted>,
    tape: Arc<Tape>,
    events: Arc<Collector>,
    dir: std::path::PathBuf,
    /// The same store the loop holds, so a test can ask what the turn left behind.
    memory: Arc<Memory>,
}

impl Fixture {
    async fn open(label: &str, answers: &[&str], steps: &[&str]) -> Self {
        Self::with(label, Scripted::new(answers, steps)).await
    }

    async fn with(label: &str, provider: Arc<Scripted>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-escalation-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let memory = Arc::new(
            Memory::open(
                &dir,
                Index::in_memory().expect("index"),
                label,
                date(2026, 9, 3),
                TierScope::normal(Locality::Cloud),
                Arc::new(Broadcast::new()),
            )
            .await
            .expect("memory"),
        );
        memory
            .record(Speaker::User, "I did computer science")
            .await
            .expect("record");
        memory
            .close(&OneFact, &FirstMatch, &Unbounded, date(2026, 9, 3))
            .await
            .expect("close");

        let tape = Arc::new(Tape::default());
        let events = Arc::new(Collector::default());
        let mut core = Loop::new(
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            Arc::clone(&events) as Arc<dyn EventSink>,
            Arc::clone(&tape) as Arc<dyn TokenSink>,
            Arc::new(SystemClock),
            Prefix::new("You are Loki."),
            Spend::new(Cents::new(10_000)),
        );
        core.attach_memory(Arc::clone(&memory))
            .await
            .expect("attach");

        Self {
            core,
            provider,
            tape,
            events,
            dir,
            memory,
        }
    }

    /// The same fixture with a search engine wired to the one exit, for the turns where memory and
    /// the web both have a claim on the answer.
    async fn with_web(label: &str, answers: &[&str], engine: Arc<FakeEngine>) -> Self {
        let mut app = Self::open(label, answers, &["DONE"]).await;
        app.core
            .attach_web(Arc::new(loki_core::core::websearch::Search {
                engines: vec![engine as Arc<dyn Discover>],
                rungs: Vec::new(),
                clock: Arc::new(SystemClock),
                budget: loki_core::core::attempt::Budget::of_steps(2),
                reads: 1,
                evidence: None,
                egress: None,
            }));
        app
    }

    async fn ask(&mut self, message: &str) {
        self.core
            .turn_with(message, CancellationToken::new())
            .await
            .expect("turn");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn prompt_text(request: &Request) -> String {
    request
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// B-32's other half. The runtime existed and nothing called it, so lane 2 had never run outside
/// a test. The floor fires before the model call, so the answer is written with the search in hand.
#[tokio::test]
async fn the_floor_runs_lane_two_before_the_model_answers() {
    // "study" does not reach "graduate" by keyword, so lane 1 comes back with nothing.
    let mut app = Fixture::open(
        "floor",
        &["You studied computer science."],
        &["SEARCH computer science", "DONE"],
    )
    .await;
    app.ask("what did I study earlier").await;

    let utility = app.provider.requests(ModelRole::Utility);
    assert!(!utility.is_empty(), "the navigator ran");

    let primary = app.provider.requests(ModelRole::Primary);
    assert_eq!(primary.len(), 1, "the floor needs no second call");
    let prompt = prompt_text(&primary[0]);
    assert!(
        prompt.contains("A deeper search of memory returned"),
        "what lane 2 found has to reach the model: {prompt}"
    );
    assert!(prompt.contains("computer science"), "{prompt}");
    assert!(
        !prompt.contains("SEARCH: <what to look for>"),
        "a turn the host already searched must not also arm the model: {prompt}"
    );

    // Charged, and visible as its own role rather than folded into the conversation.
    let charged: Vec<ModelRole> = app
        .events
        .all()
        .into_iter()
        .filter_map(|e| match e {
            Event::ModelCall { role, .. } => Some(role),
            _ => None,
        })
        .collect();
    assert!(charged.contains(&ModelRole::Utility), "{charged:?}");
    assert!(app.core.budget().spent_micros() > 0, "and it costs money");

    assert!(
        app.events.all().iter().any(|e| matches!(
            e,
            Event::MemoryRecalled {
                lane: Lane::Deliberate,
                ..
            }
        )),
        "principle 7: a retrieval is an act and appears in the stream"
    );
}

/// D-062. The model reads its recall, decides it does not answer, and asks. The asking is a
/// protocol message and the user must never see it.
#[tokio::test]
async fn a_search_request_is_never_shown_and_is_answered_once() {
    let mut app = Fixture::open(
        "voice",
        &[
            "SEARCH: what Sabharish studied",
            "You studied computer science.",
        ],
        &["SEARCH computer science", "DONE"],
    )
    .await;
    // Lane 1 answers this one at 0.42, above the floor's threshold, so the host does not
    // escalate. The model still gets to ask, which is the whole point: a good score must not be
    // able to silence it.
    app.ask("did I tell you I am a computer science graduate")
        .await;

    let shown = app.tape.shown();
    assert!(
        !shown.to_uppercase().contains("SEARCH:"),
        "the request leaked to the interface: {shown}"
    );
    assert_eq!(shown, "You studied computer science.");

    let primary = app.provider.requests(ModelRole::Primary);
    assert_eq!(primary.len(), 2, "asked, searched, asked again");
    assert!(
        prompt_text(&primary[0]).contains("SEARCH: <what to look for>"),
        "the first call has to offer it"
    );
    let second = prompt_text(&primary[1]);
    assert!(
        second.contains("A deeper search of memory returned"),
        "{second}"
    );
    assert!(
        !second.contains("SEARCH: <what to look for>"),
        "the retry is never armed, or a model that keeps asking never answers: {second}"
    );

    // And the search ran on the model's own phrasing, not on the raw message.
    let utility = prompt_text(&app.provider.requests(ModelRole::Utility)[0]);
    assert!(utility.contains("what Sabharish studied"), "{utility}");
}

/// Two rules meet. A turn that was never armed still has to survive a model that writes the
/// marker anyway, and the only right answer is to show it: nothing offered it a search.
#[tokio::test]
async fn an_unarmed_turn_treats_the_marker_as_ordinary_text() {
    let mut app = Fixture::open("unarmed", &["SEARCH: for a good haiku"], &["DONE"]).await;
    app.ask("write me a haiku about rain").await;

    assert_eq!(app.tape.shown(), "SEARCH: for a good haiku");
    assert_eq!(app.provider.requests(ModelRole::Primary).len(), 1);
    assert!(
        app.provider.requests(ModelRole::Utility).is_empty(),
        "nothing armed, nothing searched"
    );
}

/// The retry is one, not a loop. A model that asks again gets its second reply used as an answer,
/// marker and all, rather than a third call.
#[tokio::test]
async fn asking_twice_still_costs_one_search() {
    let mut app = Fixture::open(
        "one-retry",
        &["SEARCH: my degree", "SEARCH: my degree again"],
        &["DONE"],
    )
    .await;
    app.ask("did I tell you I am a computer science graduate")
        .await;

    assert_eq!(app.provider.requests(ModelRole::Primary).len(), 2);
    assert_eq!(
        app.provider.requests(ModelRole::Utility).len(),
        1,
        "one search, whatever the model does with it"
    );
}

/// §10.8's honest exhaustion, carried all the way into the prompt. A navigator that finds nothing
/// has to hand the model a miss, or the model fills the silence with "you never told me".
#[tokio::test]
async fn a_miss_reaches_the_model_as_a_miss() {
    let mut app = Fixture::open(
        "miss",
        &["I could not find it."],
        &["GREP nothing-like-this-exists", "DONE"],
    )
    .await;
    app.ask("what did I tell you about Meera earlier").await;

    let prompt = prompt_text(&app.provider.requests(ModelRole::Primary)[0]);
    assert!(prompt.contains("found nothing"), "{prompt}");
    assert!(
        prompt.contains("Never say the user did not tell you"),
        "a miss must not read as an absence: {prompt}"
    );
}

/// B-56, from Sabharish's session. An explicit instruction to search ran no search at all.
///
/// "search the memory to find everything that is listed in it" contains none of the thirteen
/// markers `asks_about_the_past` looks for, so the floor never fired and the model was never armed
/// to ask. It answered out of the working set and headed the answer "here is everything currently
/// listed in memory", which is a false statement assembled from true ones.
#[tokio::test]
async fn an_instruction_to_search_runs_a_search() {
    let mut app = Fixture::open(
        "instructed",
        &["Here is what I found."],
        &["SEARCH computer science", "DONE"],
    )
    .await;
    app.ask("search the memory to find everything that is listed in it")
        .await;

    assert!(
        !app.provider.requests(ModelRole::Utility).is_empty(),
        "the instruction has to reach lane 2"
    );
    let primary = app.provider.requests(ModelRole::Primary);
    assert_eq!(primary.len(), 1, "the floor needs no second call");
    assert!(
        prompt_text(&primary[0]).contains("A deeper search of memory"),
        "{}",
        prompt_text(&primary[0])
    );
}

/// The other half of B-56: an instruction to search ignores the score.
///
/// A good keyword hit does not answer "search your memory and list everything in it", and the
/// floor's `nothing_to_read` gate would otherwise swallow the instruction whenever lane 1 happened
/// to return something.
#[tokio::test]
async fn an_instruction_to_search_is_not_satisfied_by_a_good_recall() {
    let mut app = Fixture::open(
        "instructed-with-recall",
        &["Here is what I found."],
        &["CATALOG", "DONE"],
    )
    .await;
    // "computer science" is in the store and lane 1 will answer it well.
    app.ask("check your memory for everything about computer science")
        .await;

    assert!(
        !app.provider.requests(ModelRole::Utility).is_empty(),
        "a good lane 1 hit does not answer an instruction to search"
    );
}

/// Failure point 91. Found, empty and could-not-run are three answers, and only two reached the
/// model: a search the store refused was reported to the event stream and nowhere else, so the
/// model wrote its answer as though memory had been read and had held nothing.
#[tokio::test]
async fn a_search_that_could_not_run_reaches_the_model_as_a_failure() {
    let mut app = Fixture::with(
        "unreadable",
        Scripted::breaking_the_navigator(&["I could not check just now."]),
    )
    .await;
    app.ask("what did I tell you about Meera earlier").await;

    assert!(
        app.events
            .all()
            .iter()
            .any(|e| matches!(e, Event::Blocked { .. })),
        "the failure is still on the event stream"
    );

    let prompt = prompt_text(&app.provider.requests(ModelRole::Primary)[0]);
    assert!(prompt.contains("could not run"), "{prompt}");
    assert!(
        prompt.contains("Never say the user did not tell you"),
        "a store that refused must not read as an absence: {prompt}"
    );
    assert!(
        !prompt.contains("found nothing"),
        "and it is not a miss either, because nothing was searched: {prompt}"
    );
}

/// B-47, from Sabharish's session. Three ways a turn spent up to eight navigator calls and nine
/// seconds on a search nothing had asked for.
mod not_every_turn {
    use super::{Fixture, ModelRole};

    /// A statement is not a question about the past. "my " was a marker, so nearly every personal
    /// sentence armed a search on the way to being stored.
    #[tokio::test]
    async fn telling_loki_something_does_not_search() {
        let mut app = Fixture::open("statement", &["Got it."], &["DONE"]).await;
        app.ask("my dad is a civil contractor and he studied electronics")
            .await;

        assert!(
            app.provider.requests(ModelRole::Utility).is_empty(),
            "nothing was asked, so nothing should have been searched"
        );
        assert_eq!(app.provider.requests(ModelRole::Primary).len(), 1);
    }

    /// The floor is for a turn with nothing to read. Lane 1 answering a vague question with real
    /// facts scores low, because the question was vague, and that is not a reason to go looking.
    #[tokio::test]
    async fn a_vague_question_lane_one_answered_does_not_search() {
        let mut app = Fixture::open("vague", &["Here is what I know."], &["DONE"]).await;
        app.ask("what all do you know about my computer science degree")
            .await;

        assert!(
            app.provider.requests(ModelRole::Utility).is_empty(),
            "five recalled facts is material to read, not a reason to search"
        );
        // The model still gets the offer, so a good score cannot silence it (D-062).
        let prompt = super::prompt_text(&app.provider.requests(ModelRole::Primary)[0]);
        assert!(prompt.contains("SEARCH: <what to look for>"), "{prompt}");
    }

    /// And the floor still fires when there really is nothing.
    #[tokio::test]
    async fn a_question_with_no_recall_at_all_still_searches() {
        let mut app = Fixture::open("empty", &["I could not find it."], &["DONE"]).await;
        app.ask("what did I tell you about Meera earlier").await;

        assert!(
            !app.provider.requests(ModelRole::Utility).is_empty(),
            "lane 1 has nothing, so the model would only ask for a search anyway"
        );
    }
}

/// The gap that hid a defect for the entire life of the in-play rail (B-73).
///
/// `loki_recalled` read a list nothing ever wrote to. Both halves compiled, both had types, and the
/// read end returned a valid empty answer, so nothing at either end could see it: the gap was
/// between two crates. This is the shape of test that would have caught it, and the reason it sits
/// here rather than in a unit test is that it has to run a whole turn to be worth anything.
mod what_the_rails_read {
    use super::*;

    #[tokio::test]
    async fn a_turn_leaves_behind_what_it_recalled() {
        let mut app = Fixture::open("rails", &["You studied computer science."], &["DONE"]).await;
        assert!(
            app.core.last_recalled().is_empty(),
            "nothing has happened yet"
        );

        app.ask("what all do you know about my computer science degree")
            .await;

        // The rail asks after the turn has ended, so this is the only moment that matters.
        assert!(
            !app.core.last_recalled().is_empty(),
            "the in-play rail has something to draw"
        );
    }

    /// A rail showing the previous turn's sources beside this turn's answer looks like provenance.
    #[tokio::test]
    async fn a_second_turn_does_not_inherit_the_first_ones() {
        let mut app = Fixture::open(
            "rails-cleared",
            &["Here is what I know.", "Nothing to say about that."],
            &["DONE", "DONE"],
        )
        .await;

        app.ask("what all do you know about my computer science degree")
            .await;
        assert!(
            !app.core.last_recalled().is_empty(),
            "the first turn recalled"
        );

        app.ask("zzzzq qqqzz nothing matches this").await;
        assert!(
            app.core.last_recalled().is_empty(),
            "the second turn's rail is its own, not the first turn's"
        );
    }

    /// The web half of the same plumbing. No engine is attached, so nothing is cited, and the
    /// assertion is that this reads as nothing rather than as stale.
    #[tokio::test]
    async fn a_turn_without_the_web_cites_nothing() {
        let mut app = Fixture::open("rails-web", &["Two plus two is four."], &["DONE"]).await;
        app.ask("what is 2 + 2").await;
        assert!(app.core.last_cited().is_empty());
    }
}

/// An engine that answers from a script, so a test says what the web returned and nothing more.
struct FakeEngine {
    hits: Vec<Hit>,
    /// When set, the engine cannot be reached at all. §12.4's failure point, not an empty result.
    unreachable: bool,
}

impl FakeEngine {
    fn returning(hits: &[(&str, &str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            hits: hits
                .iter()
                .map(|(url, title, snippet)| Hit {
                    url: (*url).to_owned(),
                    title: (*title).to_owned(),
                    snippet: (*snippet).to_owned(),
                })
                .collect(),
            unreachable: false,
        })
    }

    fn down() -> Arc<Self> {
        Arc::new(Self {
            hits: Vec::new(),
            unreachable: true,
        })
    }
}

#[async_trait]
impl Discover for FakeEngine {
    fn id(&self) -> &'static str {
        "fake"
    }

    async fn search(&self, _query: &str, _cancel: CancelToken) -> Result<Vec<Hit>, SearchError> {
        if self.unreachable {
            return Err(SearchError::Unreachable("no route".into()));
        }
        Ok(self.hits.clone())
    }
}

/// Lifts the release out of a web-sourced answer, so consolidation has something to carry.
struct RustFact;

#[async_trait]
impl Extractor for RustFact {
    async fn extract(
        &self,
        _episode: &str,
        text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        if !text.contains("1.96") {
            return Ok(vec![]);
        }
        Ok(vec![Candidate {
            surface: "Rust".to_owned(),
            kind: Kind::Project,
            heading: "software".to_owned(),
            attribute: "release".to_owned(),
            text: "The latest rust release is 1.96".to_owned(),
            days_ago: None,
            valid_from: Some(date(2026, 9, 3)),
            origin: Origin::Stated,
            tags: vec![],
            aliases: vec![],
            value: None,
            relation: None,
        }])
    }
}

/// Memory and the web in one turn. B-74 was two subsystems that worked alone and silently
/// cancelled each other when both had something to say, so these run them together on purpose.
mod living_together {
    use super::*;

    /// B-57, arriving from a new direction. A web-sourced answer is Loki paraphrasing pages it
    /// read, so it must reach the transcript and never the claims: filing it would turn whatever a
    /// search engine surfaced into a remembered fact about the user.
    #[tokio::test]
    async fn a_web_sourced_answer_never_becomes_a_claim() {
        let engine = FakeEngine::returning(&[(
            "https://example.com/rust",
            "Rust 1.96",
            "Rust 1.96 was released.",
        )]);
        let mut app =
            Fixture::with_web("web-records", &["Rust 1.96 is the latest [1]."], engine).await;

        app.ask("what is the latest rust release").await;
        app.memory
            .close(&RustFact, &FirstMatch, &Unbounded, date(2026, 9, 3))
            .await
            .expect("close");

        let claims = app
            .memory
            .recall("latest rust release", 5, date(2026, 9, 3))
            .expect("recall");
        assert!(
            claims.is_empty(),
            "the web's answer stayed out of memory, but recall saw {claims:?}"
        );

        // The seeded fact is still there, so this is the filter working rather than the store
        // having been emptied.
        assert!(
            !app.memory
                .recall("computer science degree", 5, date(2026, 9, 3))
                .expect("recall")
                .is_empty(),
            "what the user actually said survives"
        );
    }

    /// Both rails at once, on the turn where the host decides for itself. Neither subsystem gets
    /// to blank the other's output.
    #[tokio::test]
    async fn one_turn_can_fill_both_rails() {
        let engine = FakeEngine::returning(&[(
            "https://example.com/cs",
            "Degrees",
            "A computer science degree covers algorithms.",
        )]);
        let mut app = Fixture::with_web("both-rails", &["Here is what I found [1]."], engine).await;

        app.ask("what all do you know about my computer science degree")
            .await;

        assert!(
            !app.core.last_recalled().is_empty(),
            "memory's rail survived the web turn"
        );
        assert!(
            !app.core.last_cited().is_empty(),
            "the web's rail survived the memory turn"
        );
    }

    /// The other flow: the host was unsure, so the model was offered the marker and used it. What
    /// the user reads is the answer, never the asking.
    #[tokio::test]
    async fn a_model_that_asks_for_the_web_is_answered_not_echoed() {
        let engine = FakeEngine::returning(&[(
            "https://example.com/tickets",
            "Tickets",
            "A ticket costs 400 dollars.",
        )]);
        let mut app = Fixture::with_web(
            "web-offer",
            &["WEB: rust conference ticket price", "Tickets are $400 [1]."],
            engine,
        )
        .await;

        app.ask("how much does a rust conference ticket cost").await;

        let shown = app.tape.shown();
        assert!(!shown.contains("WEB:"), "the asking never reaches the user");
        assert!(shown.contains("$400"), "the answer did, but got {shown:?}");
        assert!(!app.core.last_cited().is_empty(), "with its sources");
    }

    /// §12.4's failure point. An engine that cannot be reached is a missing source, not a failed
    /// turn, and it must not take memory down with it.
    #[tokio::test]
    async fn an_engine_that_is_down_still_leaves_an_answer() {
        let mut app = Fixture::with_web(
            "web-down",
            &["I could not look that up."],
            FakeEngine::down(),
        )
        .await;

        app.ask("what is the latest rust release").await;

        assert!(app.core.last_cited().is_empty(), "nothing was cited");
        assert!(
            app.tape.shown().contains("could not"),
            "the turn still answered"
        );
    }
}
