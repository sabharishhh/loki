//! Different users, used differently. Written against the risk that the other suites overfit.
//!
//! `scenarios.rs` runs one user, saying one family of things, in one voice. Everything in it
//! passing says the store works for that person. It says nothing about a user who never states a
//! durable fact, or who talks mostly about other people, or who changes their mind three times in
//! a paragraph, or who writes in a script the parser was not thought about with.
//!
//! **What is being optimised for here is common sense, not cleverness.** Most of these assert that
//! Loki does the boring, obvious thing: does not remember a haiku request, does not file a
//! colleague's preference under the user, does not fall over on an empty message. A store that is
//! brilliant at corrections and stores junk from every turn is worse than one that does neither.
//!
//! Nothing here reuses `scenarios.rs`'s fixture, deliberately.

use std::sync::Mutex;

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::claim::Origin;
use loki_core::memory::consolidate::{Candidate, ConsolidateError, Extractor, Unbounded};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Candidate as EntityCandidate, Index, Query};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

fn today() -> Date {
    date(2026, 9, 2)
}

/// Facts a turn states, as a careful extractor would read them.
///
/// Deliberately literal: it only reports what a sentence actually asserts, and it attributes to
/// whoever the sentence is about. Getting attribution wrong is the most ordinary way a memory
/// system becomes annoying, and no amount of retrieval quality recovers from it.
struct Careful {
    seen: Mutex<Vec<String>>,
}

impl Careful {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
        }
    }
}

fn claim(attribute: &str, kind: Kind, surface: &str, text: &str, origin: Origin) -> Candidate {
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
        value: None,

        relation: None,
    }
}

#[async_trait]
impl Extractor for Careful {
    async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
        self.seen.lock().expect("lock").push(text.to_owned());
        let mut out = Vec::new();

        // A colleague's preference belongs to the colleague. §9.8: the entity is what the fact is
        // about, not who mentioned it.
        if text.contains("Meera hates meetings") {
            out.push(claim(
                "meetings",
                Kind::Person,
                "Meera",
                "Meera hates meetings before ten",
                Origin::Stated,
            ));
        }
        if text.contains("Meera Shah is on design") {
            out.push(claim(
                "team",
                Kind::Person,
                "Meera Shah",
                "Meera Shah is on the design team",
                Origin::Stated,
            ));
        }
        if text.contains("my sister Priya") {
            out.push(claim(
                "relation",
                Kind::Person,
                "Priya",
                "Priya is the user's sister",
                Origin::Stated,
            ));
        }
        if text.contains("Bengaluru") {
            out.push(claim(
                "city",
                Kind::Person,
                "Arjun",
                "Arjun lives in Bengaluru",
                Origin::Stated,
            ));
        }
        if text.contains("Zoë") {
            out.push(claim(
                "name",
                Kind::Person,
                "Zoë",
                "Zoë is the user's manager",
                Origin::Stated,
            ));
        }
        // A subject whose *name* is not Latin at all, so the slug and the file path are too.
        if text.contains("台北") {
            out.push(claim(
                "city",
                Kind::Person,
                "陳美玲",
                "陳美玲 is moving to 台北",
                Origin::Stated,
            ));
        }
        // Someone changing their mind inside one sentence produces three competing claims in the
        // order they were said, which is what a real extractor would report.
        if text.contains("ships on") {
            for date in ["the 30th", "the 20th", "the 30th"] {
                if text.contains(date) {
                    out.push(claim(
                        "deadline",
                        Kind::Project,
                        "Atlas",
                        &format!("Atlas ships on {date}"),
                        Origin::Stated,
                    ));
                }
            }
        }
        Ok(out)
    }
}

struct FirstMatch;

#[async_trait]
impl Matcher for FirstMatch {
    async fn decide(
        &self,
        _s: &str,
        _c: &str,
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

/// A matcher that cannot tell two same-named people apart, which is §9.4's known failure.
struct Confused;

#[async_trait]
impl Matcher for Confused {
    async fn decide(
        &self,
        _s: &str,
        _c: &str,
        _kind: Kind,
        candidates: &[EntityCandidate],
    ) -> Result<Decision, ResolveError> {
        Ok(if candidates.len() > 1 {
            Decision::Tie((0..candidates.len()).collect())
        } else if candidates.is_empty() {
            Decision::New
        } else {
            Decision::Existing(0)
        })
    }
}

struct Store {
    memory: Memory,
    extractor: Careful,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-other-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let memory = Memory::open(
            &dir,
            Index::in_memory().expect("index"),
            label,
            today(),
            TierScope::normal(Locality::Cloud),
        )
        .await
        .expect("open");
        Self {
            memory,
            extractor: Careful::new(),
            dir,
        }
    }

    async fn say(&self, text: &str) {
        self.memory.record("user", text).await.expect("record");
    }

    async fn close(&self) {
        self.close_with(&FirstMatch).await;
    }

    async fn close_with(&self, matcher: &dyn Matcher) {
        self.memory
            .close(&self.extractor, matcher, &Unbounded, today())
            .await
            .expect("close");
    }

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

    async fn entities(&self) -> Vec<String> {
        self.memory
            .knowledge(today())
            .await
            .expect("knowledge")
            .entities
            .into_iter()
            .map(|e| e.name)
            .collect()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The most common way to use an assistant is to give it work, not to tell it about yourself.
///
/// A store that fills up with "summarise this" is worse than one that never learns anything: it
/// pays the token cost of memory on every turn and returns noise for it.
#[tokio::test]
async fn a_session_of_pure_task_work_stores_nothing() {
    let store = Store::new("tasks").await;
    for message in [
        "write me a haiku about rain",
        "summarise this paragraph",
        "what is 17 times 23",
        "rewrite that more formally",
        "thanks",
    ] {
        store.say(message).await;
    }
    store.close().await;

    assert!(
        store.entities().await.is_empty(),
        "nothing here was a durable fact: {:?}",
        store.entities().await
    );
}

/// A colleague's preference belongs to the colleague. Filing it under the user is the most
/// ordinary way a memory system starts being wrong about you.
#[tokio::test]
async fn a_fact_about_someone_else_is_filed_under_them() {
    let store = Store::new("attribution").await;
    store.say("Meera hates meetings before ten").await;
    store.say("my sister Priya is visiting").await;
    store.close().await;

    let names = store.entities().await;
    assert!(names.contains(&"Meera".to_owned()), "{names:?}");
    assert!(names.contains(&"Priya".to_owned()), "{names:?}");

    let hits = store.recall("who hates meetings");
    assert!(
        hits.iter().any(|h| h.starts_with("Meera")),
        "the claim names its subject: {hits:?}"
    );
}

/// §9.4's known failure, and the one case where creating nothing is correct.
#[tokio::test]
async fn two_people_with_the_same_name_create_neither() {
    let store = Store::new("same-name").await;
    store.say("Meera hates meetings before ten").await;
    store.close().await;
    store.say("Meera Shah is on design").await;
    store.close().await;

    // Now a third mention that blocking cannot separate.
    store.say("Meera hates meetings before ten").await;
    store.close_with(&Confused).await;

    let names = store.entities().await;
    assert!(
        names.iter().filter(|n| n.starts_with("Meera")).count() <= 2,
        "a tie must not invent a third Meera: {names:?}"
    );
}

/// Names are not ASCII and never were. A store that mangles them is unusable for most of the world.
#[tokio::test]
async fn names_outside_ascii_survive_the_round_trip() {
    let store = Store::new("unicode").await;
    store.say("Zoë is my manager").await;
    store.say("陳美玲 is moving to 台北 next month").await;
    store.close().await;

    let names = store.entities().await;
    assert!(names.contains(&"Zoë".to_owned()), "{names:?}");
    assert!(names.contains(&"陳美玲".to_owned()), "{names:?}");

    let hits = store.recall("台北");
    assert!(
        hits.iter().any(|h| h.contains("台北")),
        "a non-Latin place name has to be searchable: {hits:?}"
    );
}

/// A slug has to come out of a name that has no ASCII in it at all, and it has to be a path.
#[tokio::test]
async fn a_name_with_no_ascii_still_produces_a_usable_path() {
    let store = Store::new("slug").await;
    store.say("陳美玲 is moving to 台北 next month").await;
    store.close().await;

    let entity = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .into_iter()
        .find(|e| e.name == "陳美玲")
        .expect("a name with no Latin letters is still a name");

    assert!(entity.path.ends_with(".md"), "{}", entity.path);
    assert!(
        !entity.path.contains(' '),
        "a path with a space is a path that breaks: {}",
        entity.path
    );

    // And it round-trips: the file is really there under that path and parses back.
    let text = {
        let reader = store.memory.bundle().reader().await;
        reader.read(&entity.path).expect("the file exists")
    };
    assert!(text.contains("陳美玲"), "{text}");
}

/// Nothing said is nothing learned, and it must not be an error either.
#[tokio::test]
async fn empty_and_whitespace_turns_are_harmless() {
    let store = Store::new("empty").await;
    store.say("").await;
    store.say("   ").await;
    store.say("\n\n").await;
    store.close().await;

    assert!(store.entities().await.is_empty());
}

/// A pasted document is one turn and can be enormous. It must not break anything, and it must not
/// become a fact about the user.
#[tokio::test]
async fn a_very_long_turn_is_handled_without_storing_it() {
    let store = Store::new("long").await;
    let wall = "lorem ipsum dolor sit amet ".repeat(4_000);
    store.say(&wall).await;
    store.close().await;

    assert!(store.entities().await.is_empty());
    assert!(
        store.recall("lorem").is_empty(),
        "a pasted wall of text is not something Loki knows about you"
    );
}

/// Someone who changes their mind inside one message. The store has to end up with one answer,
/// not three, and it must not need a person to sort it out.
#[tokio::test]
async fn changing_your_mind_twice_in_one_breath_leaves_one_answer() {
    let store = Store::new("indecisive").await;
    store
        .say("Atlas ships on the 30th, no the 20th, actually it ships on the 30th")
        .await;
    store.close().await;

    let hits = store.recall("Atlas ships");
    assert_eq!(hits.len(), 1, "one answer, not three: {hits:?}");
    assert!(
        hits[0].contains("30th"),
        "and it is the one they landed on: {hits:?}"
    );

    // Nothing needed a person to sort out. Rule 4 decides and keeps the loser to be checked.
    let entity = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .into_iter()
        .find(|e| e.name == "Atlas")
        .expect("Atlas");
    assert_eq!(entity.facts.len(), 1, "{:?}", entity.facts);
    assert!(
        entity.in_use,
        "an indecisive sentence must not take the project out of use"
    );
}

/// Two people talked about in one session, neither of them the user. A store that assumes every
/// fact is about its owner gets this wrong.
#[tokio::test]
async fn a_session_entirely_about_other_people_still_works() {
    let store = Store::new("third-party").await;
    store.say("Meera hates meetings before ten").await;
    store.say("陳美玲 is moving to 台北 next month").await;
    store.say("Atlas ships on the 30th").await;
    store.close().await;

    let names = store.entities().await;
    assert_eq!(names.len(), 3, "three subjects, three entities: {names:?}");
    assert!(
        store.recall("Atlas").iter().any(|h| h.contains("30th")),
        "a project is an entity like any other"
    );
}

/// Two sessions on one day, which is the shape a real day has: a burst in the morning, another
/// after lunch. Neither should duplicate the other.
#[tokio::test]
async fn two_sessions_on_one_day_do_not_duplicate() {
    let store = Store::new("two-sessions").await;
    store.say("Meera hates meetings before ten").await;
    store.close().await;

    store.say("Meera hates meetings before ten").await;
    store.close().await;

    let facts: Vec<String> = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .into_iter()
        .filter(|e| e.name == "Meera")
        .flat_map(|e| e.facts)
        .map(|f| f.text)
        .collect();
    assert_eq!(facts.len(), 1, "{facts:?}");
}

/// The same store opened twice at once, which happens the moment there is a second window.
#[tokio::test]
async fn two_readers_on_one_store_do_not_corrupt_it() {
    let store = Store::new("concurrent").await;
    store.say("Meera hates meetings before ten").await;
    store.close().await;

    let second = Index::in_memory().expect("index");
    let other = Memory::open(
        &store.dir,
        second,
        "second",
        today(),
        TierScope::normal(Locality::Cloud),
    )
    .await
    .expect("open twice");

    let (mine, theirs) = tokio::join!(other.knowledge(today()), store.memory.knowledge(today()));
    assert_eq!(
        mine.expect("knowledge").entities.len(),
        theirs.expect("knowledge").entities.len(),
        "two readers see the same store"
    );
}

/// Punctuation and casing are not identity. Someone typing quickly should find what they stored.
#[tokio::test]
async fn a_question_typed_carelessly_still_matches() {
    let store = Store::new("careless").await;
    store.say("Meera hates meetings before ten").await;
    store.close().await;

    for question in ["MEERA", "meera?", "  meera  ", "Meera!!"] {
        assert!(
            !store.recall(question).is_empty(),
            "recall should not care about case or punctuation: {question:?}"
        );
    }
}
