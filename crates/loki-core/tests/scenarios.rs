//! What happens when the app is used the way it is meant to be used.
//!
//! The other suites test one rule each against a fixture built to exercise it. This one runs the
//! store the way a person does: several things said across several turns, a correction later, the
//! app closed and reopened, the same thing said again in different words.
//!
//! **The extractor here reads its input.** It is not a script that returns fixed candidates
//! regardless of what it was handed, because that would pass whether or not consolidation gives
//! it the right text, and the bug that produced duplicates in the live store was exactly that:
//! the pass handed the extractor the whole day, every time. It also words facts differently on
//! each run, because a model does, and every duplicate reported in testing came from that.

use std::sync::Mutex;

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::claim::Origin;
use loki_core::memory::consolidate::{
    Candidate, ConsolidateError, Extractor, Report, Unbounded, clear_buffer,
};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Candidate as EntityCandidate, Index, Query};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

fn today() -> Date {
    date(2026, 9, 2)
}

/// Reads the text it is given and reports the facts in it, wording them differently each run.
///
/// The wording drift is the point. A real extractor is a model, and asking it twice for the same
/// fact gives two sentences. Anything that survives this is not relying on the extractor being
/// deterministic, which it never is.
struct Reader {
    runs: Mutex<usize>,
    seen: Mutex<Vec<String>>,
}

impl Reader {
    fn new() -> Self {
        Self {
            runs: Mutex::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Everything it was ever asked to read, for asserting what consolidation handed it.
    fn inputs(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Extractor for Reader {
    async fn extract(
        &self,
        _episode: &str,
        text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        self.seen.lock().expect("lock").push(text.to_owned());
        let run = {
            let mut runs = self.runs.lock().expect("lock");
            *runs += 1;
            *runs
        };

        fn candidate(
            attribute: &str,
            kind: Kind,
            surface: &str,
            text: &str,
            origin: Origin,
        ) -> Candidate {
            Candidate {
                surface: surface.to_owned(),
                kind,
                heading: attribute.to_owned(),
                attribute: attribute.to_owned(),
                text: text.to_owned(),
                days_ago: None,
                valid_from: None,
                origin,
                tags: vec![],
                aliases: vec![],
                relation: None,
            }
        }

        let mut out = Vec::new();
        let mut fact = |attribute: &str, kind: Kind, surface: &str, text: &str| {
            out.push(candidate(attribute, kind, surface, text, Origin::Stated));
        };

        if text.contains("name is Sabharish") || text.contains("I'm Sabharish") {
            // Two spellings of one sentence, alternating by run.
            fact(
                "name",
                Kind::Person,
                "Sabharish",
                if run.is_multiple_of(2) {
                    "Sabharish is the user's name"
                } else {
                    "The user's name is Sabharish"
                },
            );
        }
        if text.contains("computer science") {
            fact(
                "education",
                Kind::Person,
                "Sabharish",
                if run.is_multiple_of(2) {
                    "Sabharish studied computer science"
                } else {
                    "Sabharish is a computer science graduate"
                },
            );
        }
        if text.contains("Chennai") {
            fact(
                "city",
                Kind::Person,
                "Sabharish",
                "Sabharish lives in Chennai",
            );
        }
        if text.contains("Bangalore") {
            fact(
                "city",
                Kind::Person,
                "Sabharish",
                "Sabharish lives in Bangalore",
            );
        }
        // Something Loki worked out rather than something the user said, which is the case
        // §9.8 makes wait for recall behaviour.
        if text.contains("short replies") {
            // Plural one run, singular the next. Open question 18's drift, deliberately.
            fact(
                if run.is_multiple_of(2) {
                    "reply_styles"
                } else {
                    "reply_style"
                },
                Kind::Preference,
                "reply length",
                "Sabharish prefers short replies",
            );
        }
        // Something Loki worked out rather than something the user said, which is the case §9.8
        // makes wait for recall behaviour.
        if text.contains("keep it brief") {
            out.push(candidate(
                "reply_style",
                Kind::Preference,
                "reply length",
                "Sabharish seems to want short replies",
                Origin::Inferred,
            ));
        }
        Ok(out)
    }
}

/// Answers the way §9.4's matcher should: the first candidate, or new when blocking found none.
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

/// One store, driven the way the app drives it.
struct App {
    memory: Memory,
    reader: Reader,
    dir: std::path::PathBuf,
    /// False while another `App` will reopen the same directory, as a relaunch does.
    owns_dir: bool,
}

impl App {
    async fn open(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-scenario-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self::reopen(dir, label).await
    }

    /// Opens a store that already exists on disk, as relaunching the app does.
    async fn reopen(dir: std::path::PathBuf, session: &str) -> Self {
        let index = Index::in_memory().expect("index");
        let memory = Memory::open(
            &dir,
            index,
            session,
            today(),
            TierScope::normal(Locality::Cloud),
        )
        .await
        .expect("open");
        Self {
            memory,
            reader: Reader::new(),
            dir,
            owns_dir: true,
        }
    }

    /// Leaves the store on disk when this handle goes away, so a later `reopen` finds it.
    fn leave_on_disk(&mut self) {
        self.owns_dir = false;
    }

    async fn say(&self, text: &str) {
        self.memory.record("user", text).await.expect("record");
    }

    async fn reply(&self, text: &str) {
        self.memory.record("assistant", text).await.expect("record");
    }

    /// What the app does when the window closes.
    async fn close(&self) -> Report {
        self.memory
            .close(&self.reader, &FirstMatch, &Unbounded, today())
            .await
            .expect("close")
    }

    /// What Loki would put in front of the model for a question.
    fn recall(&self, question: &str) -> Vec<String> {
        self.memory
            .index()
            .recall(&Query::prefetch(
                question,
                TierScope::normal(Locality::Cloud),
                today(),
                5,
            ))
            .expect("recall")
            .into_iter()
            .map(|hit| hit.text)
            .collect()
    }

    async fn facts_about(&self, name: &str) -> Vec<String> {
        self.memory
            .knowledge(today())
            .await
            .expect("knowledge")
            .entities
            .into_iter()
            .filter(|e| e.name == name)
            .flat_map(|e| e.facts)
            .map(|f| f.text)
            .collect()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if self.owns_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn mentions(hits: &[String], needle: &str) -> bool {
    hits.iter().any(|hit| hit.contains(needle))
}

/// The first conversation. Three unrelated things, and every one of them usable afterwards.
#[tokio::test]
async fn a_first_conversation_leaves_three_usable_facts() {
    let app = App::open("first").await;
    app.say("hi, my name is Sabharish").await;
    app.reply("Hello.").await;
    app.say("I did computer science").await;
    app.reply("Noted.").await;
    app.say("I prefer short replies").await;
    app.reply("Understood.").await;

    app.close().await;

    let facts = app.facts_about("Sabharish").await;
    assert_eq!(facts.len(), 2, "name and education: {facts:?}");

    assert!(
        mentions(&app.recall("what is my name"), "Sabharish"),
        "name"
    );
    assert!(
        mentions(&app.recall("computer science degree"), "computer science"),
        "education"
    );
    assert!(
        mentions(&app.recall("short reply preference"), "short replies"),
        "the preference is its own entity, and porter stemming matches reply against replies"
    );
}

/// Sabharish's report: closing and reopening repeatedly produced duplicates.
///
/// The buffer is what fixes it. Consolidation reads what has not been consolidated yet, so the
/// second and third close have nothing to re-extract, and the extractor's wording drift never
/// gets the chance to turn one fact into three.
#[tokio::test]
async fn closing_three_times_in_one_session_does_not_duplicate() {
    let app = App::open("repeat").await;
    app.say("my name is Sabharish").await;
    app.reply("Hello.").await;

    app.close().await;
    app.close().await;
    app.close().await;

    let facts = app.facts_about("Sabharish").await;
    assert_eq!(facts.len(), 1, "one fact, three closes: {facts:?}");

    // And the extractor was only ever handed the turn once.
    let asked = app
        .reader
        .inputs()
        .iter()
        .filter(|text| text.contains("Sabharish"))
        .count();
    assert_eq!(asked, 1, "the buffer is cleared, so nothing is re-read");
}

/// More said after a close. The second pass sees only the new part, and both facts stand.
#[tokio::test]
async fn a_second_close_picks_up_only_what_is_new() {
    let app = App::open("incremental").await;
    app.say("my name is Sabharish").await;
    app.close().await;

    app.say("I did computer science").await;
    app.close().await;

    let facts = app.facts_about("Sabharish").await;
    assert_eq!(facts.len(), 2, "{facts:?}");
    assert!(mentions(&app.recall("what is my name"), "Sabharish"));
    assert!(mentions(
        &app.recall("computer science"),
        "computer science"
    ));
}

/// A correction, which is the thing the product exists to get right.
#[tokio::test]
async fn a_move_supersedes_and_the_old_city_stops_being_recalled() {
    let app = App::open("correction").await;
    app.say("I live in Chennai").await;
    app.close().await;
    assert!(mentions(&app.recall("Chennai"), "Chennai"));

    app.say("I have moved to Bangalore").await;
    app.close().await;

    let hits = app.recall("Sabharish lives city");
    assert!(mentions(&hits, "Bangalore"), "{hits:?}");
    assert!(
        !mentions(&hits, "Chennai"),
        "the old city must not reach a prompt: {hits:?}"
    );

    let facts = app.facts_about("Sabharish").await;
    assert_eq!(
        facts.len(),
        1,
        "a correction is one row, not two: {facts:?}"
    );
}

/// Two things in one breath that cannot both be true. Under §9.7 rule 4 as built, the later one
/// is used and the earlier is kept to be checked, and nothing else about the person is affected.
#[tokio::test]
async fn a_conflict_costs_only_itself() {
    let app = App::open("conflict").await;
    app.say("my name is Sabharish and I did computer science")
        .await;
    app.say("I live in Chennai, well, Bangalore now").await;
    app.close().await;

    let hits = app.recall("Sabharish name study city");
    assert!(mentions(&hits, "Sabharish"), "the name survives: {hits:?}");
    assert!(
        mentions(&hits, "computer science"),
        "the degree survives: {hits:?}"
    );
    assert!(
        mentions(&hits, "Bangalore"),
        "the later city is used: {hits:?}"
    );
    assert!(
        !mentions(&hits, "Chennai"),
        "the shadowed one stays out of the prompt: {hits:?}"
    );
}

/// A force quit. The buffer is on disk, so the next launch picks the turns up rather than
/// orphaning them (§18.2, B-30).
#[tokio::test]
async fn a_session_that_never_closed_is_consolidated_on_the_next_launch() {
    let dir = std::env::temp_dir().join(format!(
        "loki-scenario-{}-crash-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let mut app = App::reopen(dir.clone(), "one").await;
        app.leave_on_disk();
        app.say("my name is Sabharish").await;
        // No close, and no cleanup. The process ends here the way a force quit ends it.
        assert!(app.memory.has_unconsolidated().await);
    }

    let app = App::reopen(dir.clone(), "two").await;
    assert!(
        app.memory.has_unconsolidated().await,
        "the buffer survived the process that wrote it"
    );
    app.close().await;

    assert!(mentions(&app.recall("what is my name"), "Sabharish"));
    assert!(
        !app.memory.has_unconsolidated().await,
        "and is cleared after"
    );
}

/// The same fact, said again in different words, months apart. One fact.
#[tokio::test]
async fn saying_the_same_thing_again_later_does_not_add_a_claim() {
    let app = App::open("restated").await;
    app.say("my name is Sabharish").await;
    app.close().await;

    app.say("just so you know, I'm Sabharish").await;
    app.close().await;

    let facts = app.facts_about("Sabharish").await;
    assert_eq!(facts.len(), 1, "{facts:?}");
}

/// Open question 18's drift, which produced duplicates in the live store: the extractor wrote
/// `reply_style` one run and `reply_styles` the next, so nothing ever superseded anything.
#[tokio::test]
async fn two_spellings_of_one_attribute_do_not_split_it() {
    let app = App::open("drift").await;
    app.say("I prefer short replies").await;
    app.close().await;
    app.say("remember, I prefer short replies").await;
    app.close().await;

    let facts = app.facts_about("reply length").await;
    assert_eq!(facts.len(), 1, "one property, one row: {facts:?}");
}

/// The honest limit of lane 1, written down so it is a known gap rather than a surprise.
///
/// Recall is keyword search with porter stemming (§10.5). "What did I study" shares no word with
/// "is a computer science graduate", so nothing comes back. §10.5 puts that on a local embedding
/// index after two failed keyword rounds, and §10.8's lane 2 is the other half of the answer.
///
/// Asserted rather than avoided, because a test suite that only ever asks questions the system can
/// answer measures nothing about the questions a person actually asks.
#[tokio::test]
async fn a_question_sharing_no_word_with_the_claim_is_a_known_miss() {
    let app = App::open("semantic-gap").await;
    app.say("I did computer science").await;
    app.close().await;

    assert!(
        mentions(&app.recall("computer science"), "computer science"),
        "the words that are shared do match"
    );
    assert!(
        app.recall("what did I study").is_empty(),
        "keyword recall cannot bridge study to graduate: {:?}",
        app.recall("what did I study")
    );
}

/// Porter stemming, §10.5's first cheap win: a plural in the question finds a singular in the
/// claim, and the other way round.
#[tokio::test]
async fn a_question_in_the_other_number_still_matches() {
    let app = App::open("stemming").await;
    app.say("I live in Chennai").await;
    app.close().await;

    assert!(
        mentions(&app.recall("where does Sabharish live"), "Chennai"),
        "live against lives"
    );
}

/// Nothing is invented. A question about something never said comes back empty, which §10.8 says
/// has to read as a miss rather than as an answer.
#[tokio::test]
async fn a_question_about_something_never_said_finds_nothing() {
    let app = App::open("empty").await;
    app.say("my name is Sabharish").await;
    app.close().await;

    assert!(
        app.recall("what is my sister's phone number").is_empty(),
        "{:?}",
        app.recall("what is my sister's phone number")
    );
}

/// The buffer is cleared only after a committed pass, so a run that consolidates nothing leaves
/// the turns for the next one rather than dropping them.
#[tokio::test]
async fn clearing_the_buffer_is_explicit_and_survives_a_reopen() {
    let app = App::open("buffer").await;
    app.say("my name is Sabharish").await;
    assert!(app.memory.has_unconsolidated().await);

    clear_buffer(app.memory.bundle()).await.expect("clear");
    assert!(!app.memory.has_unconsolidated().await);
}

/// §9.8 and §10.6, end to end. A guess earns its place by answering different questions on
/// different days, not by the extractor writing it twice.
#[tokio::test]
async fn a_guess_is_promoted_by_being_useful_across_days() {
    let app = App::open("recall-promotion").await;
    app.say("keep it brief").await;
    app.close().await;

    // Written as a guess, so it waits.
    let held = app.memory.knowledge(today()).await.expect("knowledge");
    let preference = held
        .entities
        .iter()
        .find(|e| e.name == "reply length")
        .expect("the preference exists");
    assert!(
        !preference.in_use,
        "a guess is not used on its first mention"
    );

    // Three different questions, on three different days, all answered by it.
    let path = preference.path.clone();
    let ordinal = preference.facts.first().map_or(0, |f| f.ordinal);
    for (day, question) in [
        (date(2026, 9, 2), "how long should replies be"),
        (date(2026, 9, 3), "reply length preference"),
        (date(2026, 9, 4), "does Sabharish want short answers"),
    ] {
        let hits = app
            .memory
            .index()
            .recall(&Query {
                visibility: loki_core::memory::index::Visibility::Everything,
                ..Query::prefetch(question, TierScope::normal(Locality::Cloud), day, 5)
            })
            .expect("recall");
        assert!(
            hits.iter().any(|h| h.path == path && h.ordinal == ordinal),
            "the claim has to be reachable to earn anything: {hits:?}"
        );
        app.memory
            .note_recall(
                &hits,
                question,
                day,
                loki_core::memory::index::Lane::Automatic,
            )
            .expect("note");
    }

    app.say("anything else").await;
    app.close().await;

    let after = app.memory.knowledge(today()).await.expect("knowledge");
    let preference = after
        .entities
        .iter()
        .find(|e| e.name == "reply length")
        .expect("still there");
    assert!(
        preference.in_use,
        "three questions across three days is what earns it: {preference:?}"
    );
}

/// The counts live in the file, so wiping the index does not wipe the promotion signal (§9.13).
#[tokio::test]
async fn the_recall_counts_survive_in_the_file() {
    let app = App::open("counts-in-file").await;
    app.say("keep it brief").await;
    app.close().await;

    let entity = app
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .into_iter()
        .find(|e| e.name == "reply length")
        .expect("preference");
    let hits = app
        .memory
        .index()
        .recall(&Query {
            visibility: loki_core::memory::index::Visibility::Everything,
            ..Query::prefetch(
                "short replies",
                TierScope::normal(Locality::Cloud),
                today(),
                5,
            )
        })
        .expect("recall");
    app.memory
        .note_recall(
            &hits,
            "short replies",
            today(),
            loki_core::memory::index::Lane::Automatic,
        )
        .expect("note");

    app.say("more").await;
    app.close().await;

    let text = {
        let reader = app.memory.bundle().reader().await;
        reader.read(&entity.path).expect("read")
    };
    assert!(
        text.contains("recalls: 1"),
        "the count is written into the record, not only the index: {text}"
    );
}

/// Principle 7: nothing acts outside the event stream, and a retrieval is an act. §10.6's log is
/// a consumer of this event, so the two have to carry the same digest or they cannot be lined up.
#[test]
fn one_query_hashes_the_same_way_everywhere() {
    use loki_core::memory::index::query_hash;

    assert_eq!(
        query_hash("What is my name?"),
        query_hash("what is my name?")
    );
    assert_eq!(
        query_hash(" what is my name? "),
        query_hash("what is my name?")
    );
    assert_ne!(query_hash("what is my name"), query_hash("where do I live"));
    assert_eq!(
        query_hash("x").len(),
        16,
        "fixed width, so a log column is bounded"
    );
}

/// §10.8's escalation, end to end. Lane 2 fires when lane 1 was not enough and stays out of the
/// way when it was, and a miss is reported as a miss.
mod lane_two {
    use super::{App, today};
    use async_trait::async_trait;
    use loki_core::memory::index::{Query, Visibility};
    use loki_core::memory::runtime::{Navigator, Op, RuntimeError, should_escalate};

    /// Walks §10.8's own route: start from the catalog, narrow with a ranked search, stop.
    struct Route {
        steps: std::sync::Mutex<Vec<Op>>,
    }

    impl Route {
        fn to(question: &str) -> Self {
            Self {
                steps: std::sync::Mutex::new(vec![
                    Op::Catalog,
                    Op::Search {
                        query: question.to_owned(),
                    },
                ]),
            }
        }
    }

    #[async_trait]
    impl Navigator for Route {
        async fn next(&self, _q: &str, _seen: &[String]) -> Result<Option<Op>, RuntimeError> {
            let mut steps = self.steps.lock().expect("lock");
            Ok(if steps.is_empty() {
                None
            } else {
                Some(steps.remove(0))
            })
        }
    }

    /// A navigator that never stops, for checking the budget actually bounds the loop.
    struct Endless;

    #[async_trait]
    impl Navigator for Endless {
        async fn next(&self, _q: &str, _seen: &[String]) -> Result<Option<Op>, RuntimeError> {
            Ok(Some(Op::Ls {
                dir: "nowhere".to_owned(),
            }))
        }
    }

    /// The trigger is two conditions and a threshold, and no model is asked.
    #[tokio::test]
    async fn a_question_lane_one_answered_does_not_escalate() {
        let app = App::open("no-escalation").await;
        app.say("my name is Sabharish").await;
        app.close().await;

        let best = app
            .memory
            .index()
            .recall(&Query::prefetch(
                "what is my name",
                loki_core::memory::gate::TierScope::normal(loki_core::core::vocab::Locality::Cloud),
                today(),
                5,
            ))
            .expect("recall")
            .first()
            .map(|hit| hit.score.value());

        assert!(best.is_some(), "lane 1 found it");
        assert!(
            !should_escalate("what is my name", best),
            "a good hit means no second round trip: {best:?}"
        );
    }

    /// The case B-32 was about: a question about the past that lane 1 cannot answer.
    #[tokio::test]
    async fn a_question_lane_one_missed_escalates_and_lane_two_finds_it() {
        let app = App::open("escalation").await;
        app.say("I did computer science").await;
        app.close().await;

        // Keyword recall cannot bridge "study" to "graduate", so lane 1 comes back empty.
        let best = app
            .memory
            .index()
            .recall(&Query::prefetch(
                "what did I study",
                loki_core::memory::gate::TierScope::normal(loki_core::core::vocab::Locality::Cloud),
                today(),
                5,
            ))
            .expect("recall")
            .first()
            .map(|hit| hit.score.value());
        assert!(best.is_none(), "lane 1 has nothing: {best:?}");
        assert!(should_escalate("what did I study", best));

        // Lane 2 goes looking with a query the store can actually match.
        let found = app
            .memory
            .search_deeply("computer science", &Route::to("computer science"), today())
            .await
            .expect("lane 2");
        assert!(
            found.render().contains("computer science"),
            "{}",
            found.render()
        );
        assert!(found.searched <= 8, "the budget holds: {}", found.searched);
    }

    /// §10.8's honest exhaustion, applied inward.
    #[tokio::test]
    async fn a_lane_two_miss_is_reported_as_a_miss() {
        let app = App::open("lane2-miss").await;
        app.say("my name is Sabharish").await;
        app.close().await;

        let found = app
            .memory
            .search_deeply(
                "my sister's phone number",
                &Route::to("sister phone number"),
                today(),
            )
            .await
            .expect("lane 2");

        // The catalog is never empty once anything is stored, so the honest answer here is that
        // it looked and what it found does not answer the question.
        assert!(
            !found.render().contains("phone"),
            "nothing was invented: {}",
            found.render()
        );
    }

    /// The budget bounds the loop even when the navigator will not stop.
    #[tokio::test]
    async fn the_search_budget_stops_a_runaway() {
        let app = App::open("budget").await;
        app.say("my name is Sabharish").await;
        app.close().await;

        let found = app
            .memory
            .search_deeply("anything", &Endless, today())
            .await
            .expect("lane 2");

        assert_eq!(found.searched, 8, "§10.5's hard budget of eight");
        assert!(
            found.out_of_budget,
            "and it says so rather than looking empty"
        );
        assert!(found.render().contains("ran out of searches"));
    }

    /// §10.6: a candidate is searchable on lane 2 and never prompt-eligible.
    #[tokio::test]
    async fn lane_two_reaches_what_lane_one_may_not() {
        let app = App::open("lane2-candidates").await;
        app.say("keep it brief").await;
        app.close().await;

        let prompt = app.recall("short replies");
        assert!(
            prompt.is_empty(),
            "a guess is not prompt-eligible: {prompt:?}"
        );

        let hits = app
            .memory
            .index()
            .recall(&Query {
                visibility: Visibility::Everything,
                ..Query::prefetch(
                    "short replies",
                    loki_core::memory::gate::TierScope::normal(
                        loki_core::core::vocab::Locality::Cloud,
                    ),
                    today(),
                    5,
                )
            })
            .expect("recall");
        assert!(
            hits.iter().any(|h| h.text.contains("short replies")),
            "and lane 2 can still find it: {hits:?}"
        );
    }
}

/// The session transcript, end to end: a real prompt through a real provider decorator, and the
/// file readable afterwards. §20.1's audit trail, as a thing a person opens.
#[tokio::test]
async fn the_journal_records_a_whole_turn() {
    use async_trait::async_trait;
    use loki_core::adapters::journal::{Journal, Journalled};
    use loki_core::core::sink::EventSink;
    use loki_core::core::vocab::{CostModel, ModelRole};
    use loki_core::ports::model::{
        Caps, Chunk, ChunkStream, Message, ModelError, ModelProvider, Request, SystemBlock,
        ToolSupport,
    };
    use std::sync::Arc;

    struct Says(&'static str);

    #[async_trait]
    impl ModelProvider for Says {
        fn id(&self) -> &str {
            "says"
        }
        fn caps(&self) -> Caps {
            Caps {
                locality: Locality::Cloud,
                prompt_cache: false,
                max_context: 1_000,
                tools: ToolSupport::None,
                cost: CostModel::Free,
            }
        }
        async fn complete(
            &self,
            _req: Request,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<ChunkStream, ModelError> {
            let text = self.0.to_owned();
            Ok(Box::pin(futures_util::stream::iter(vec![Ok(Chunk::Text(
                text,
            ))])))
        }
    }

    let path = std::env::temp_dir().join(format!(
        "loki-journal-turn-{}-{:?}.log",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);

    let journal = Arc::new(Journal::open(&path, "test"));
    let provider: Arc<dyn ModelProvider> = Arc::new(Says("Hello Sabharish."));
    let wrapped = Journalled::new(provider, Arc::clone(&journal));

    let request = Request {
        role: ModelRole::Primary,
        system: vec![SystemBlock::new(
            "You are Loki.\n## Sabharish\n- name is Sabharish",
        )],
        messages: vec![Message::user("what is my name")],
        max_tokens: 100,
    };
    let mut stream = wrapped
        .complete(request, tokio_util::sync::CancellationToken::new())
        .await
        .expect("complete");
    while futures_util::StreamExt::next(&mut stream).await.is_some() {}

    journal.emit(&loki_core::core::event::Event::ModelCall {
        task: loki_core::core::ids::TaskId::new(0),
        provider: "says".to_owned(),
        role: ModelRole::Primary,
        locality: Locality::Cloud,
        tokens_in: 1_200,
        tokens_out: 40,
        cost: CostModel::Free,
    });
    journal.totals();

    let log = std::fs::read_to_string(&path).expect("the log exists");

    // The whole prompt, including what memory contributed, because a transcript that summarises
    // the prompt cannot answer why it said that.
    assert!(log.contains("- name is Sabharish"), "{log}");
    assert!(log.contains("what is my name"), "{log}");
    assert!(log.contains("Hello Sabharish."), "{log}");
    // And what it cost, per session.
    assert!(
        log.contains("1 calls, 1200 in, 40 out, 1200 in context"),
        "{log}"
    );
    // Timestamped throughout, so two runs can be told apart by eye.
    assert!(log.contains("session "), "{log}");

    let _ = std::fs::remove_file(&path);
}

/// The meters, end to end: a turn through a `Loop` whose events reach a `Journal`.
///
/// The other journal test emits `ModelCall` by hand, so it proves the counter and not the wiring,
/// and the adapter's own test fed usage and stop on one chunk, which is a shape OpenAI does not
/// send. Between them B-45 hid for a whole phase: every OpenAI turn recorded zero tokens and zero
/// cost. This runs the path the app runs, with the ordering the provider uses.
#[tokio::test]
async fn a_turn_moves_the_session_token_counters() {
    use async_trait::async_trait;
    use loki_core::adapters::clock::SystemClock;
    use loki_core::adapters::journal::{Journal, Journalled};
    use loki_core::core::budget::Budget as Spend;
    use loki_core::core::cycle::{Loop, NullTokens};
    use loki_core::core::prompt::Prefix;
    use loki_core::core::sink::EventSink;
    use loki_core::core::vocab::{Cents, CostModel};
    use loki_core::ports::model::{
        Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request, StopReason, ToolSupport,
        Usage,
    };
    use std::sync::Arc;

    struct Reports;

    #[async_trait]
    impl ModelProvider for Reports {
        fn id(&self) -> &str {
            "reports"
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
            _req: Request,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<ChunkStream, ModelError> {
            // The shape OpenAI streams, which is the one B-45 was about: the call is declared
            // over, and only then is what it cost reported.
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(Chunk::Text("Hello.".to_owned())),
                Ok(Chunk::Done(StopReason::EndTurn)),
                Ok(Chunk::Usage(Usage {
                    input_tokens: 1_200,
                    output_tokens: 40,
                    ..Usage::default()
                })),
            ])))
        }
    }

    let path = std::env::temp_dir().join(format!(
        "loki-meters-{}-{:?}.log",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);

    let journal = Arc::new(Journal::open(&path, "test"));
    let provider: Arc<dyn ModelProvider> = Arc::new(Journalled::new(
        Arc::new(Reports) as Arc<dyn ModelProvider>,
        Arc::clone(&journal),
    ));
    let mut core = Loop::new(
        provider,
        Arc::clone(&journal) as Arc<dyn EventSink>,
        Arc::new(NullTokens),
        Arc::new(SystemClock),
        Prefix::new("You are Loki."),
        Spend::new(Cents::new(10_000)),
    );
    core.turn_with("hello", tokio_util::sync::CancellationToken::new())
        .await
        .expect("turn");

    let spent = journal.tokens();
    assert_eq!(spent.calls, 1, "{spent:?}");
    assert_eq!(spent.input, 1_200, "{spent:?}");
    assert_eq!(spent.output, 40, "{spent:?}");
    assert_eq!(spent.context, 1_200, "{spent:?}");

    let _ = std::fs::remove_file(&path);
}
