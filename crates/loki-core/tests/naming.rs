//! What something is called is knowledge, not bookkeeping (S-26).
//!
//! Sabharish told Loki three times, across three sessions, that his father is called Ashok. Every
//! time it was stored, and every time the trust surface showed nothing, because 2r put names and
//! edges in frontmatter and §17.3 renders claims. A person cannot tell "you did not hear me" from
//! "I heard you and will not say so", so they say it again.
//!
//! The same session produced the other half: two sessions worded one name two ways, `name` is
//! single-valued, and the store called that a contradiction. It showed
//! "Vaidyanathan's official name is Vaidyanathan" against "The user's father's name is
//! Vaidyanathan" and asked which was right.
//!
//! **The class is one thing said two ways.** Once as a claim and once as a name or an edge, or
//! twice as claims worded differently. Both halves come from the store comparing sentences where
//! it should compare what they assert, and from routing half the answer somewhere nobody looks.
//! The cases below are that class across families, workplaces, projects, scripts and medicine.

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::claim::Origin;
use loki_core::memory::consolidate::{
    Candidate, ConsolidateError, Extractor, RelationTo, Unbounded,
};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Candidate as EntityCandidate, Index, Query};
use loki_core::memory::knowledge::Knowledge;
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

fn today() -> Date {
    date(2026, 9, 3)
}

/// One turn, and what a good extractor takes from it under the current instructions.
struct Said {
    turn: &'static str,
    subject: &'static str,
    kind: Kind,
    attribute: &'static str,
    fact: &'static str,
    /// The bare value, for a single-valued attribute.
    value: Option<&'static str>,
    aliases: &'static [&'static str],
    relation: Option<(&'static str, &'static str)>,
}

const fn said(
    turn: &'static str,
    subject: &'static str,
    attribute: &'static str,
    fact: &'static str,
) -> Said {
    Said {
        turn,
        subject,
        kind: Kind::Person,
        attribute,
        fact,
        value: None,
        aliases: &[],
        relation: None,
    }
}

const fn valued(mut s: Said, value: &'static str) -> Said {
    s.value = Some(value);
    s
}

const fn named(mut s: Said, aliases: &'static [&'static str]) -> Said {
    s.aliases = aliases;
    s
}

const fn tied(mut s: Said, label: &'static str, of: &'static str) -> Said {
    s.relation = Some((label, of));
    s
}

const fn thing(mut s: Said) -> Said {
    s.kind = Kind::Project;
    s
}

struct Reads(Vec<Said>);

#[async_trait]
impl Extractor for Reads {
    async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
        Ok(self
            .0
            .iter()
            .filter(|s| text.contains(s.turn))
            .map(|s| Candidate {
                surface: s.subject.to_owned(),
                kind: s.kind,
                heading: s.attribute.to_owned(),
                attribute: s.attribute.to_owned(),
                text: s.fact.to_owned(),
                days_ago: None,
                valid_from: None,
                origin: Origin::Stated,
                tags: vec![],
                aliases: s.aliases.iter().map(|a| (*a).to_owned()).collect(),
                value: s.value.map(str::to_owned),
                relation: s.relation.map(|(label, of)| RelationTo {
                    label: label.to_owned(),
                    of: of.to_owned(),
                }),
            })
            .collect())
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

struct Store {
    memory: Memory,
    dir: std::path::PathBuf,
}

impl Store {
    /// Runs each line as its own session, which is what closing and reopening the app produces.
    async fn told(label: &str, script: Vec<Said>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-naming-{}-{label}-{:?}",
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

        let turns: Vec<&str> = script.iter().map(|s| s.turn).collect();
        let extractor = Reads(script);
        for turn in turns {
            memory.record("user", turn).await.expect("record");
            memory
                .close(&extractor, &FirstMatch, &Unbounded, today())
                .await
                .expect("close");
        }
        Self { memory, dir }
    }

    async fn knows(&self) -> Knowledge {
        self.memory.knowledge(today()).await.expect("knowledge")
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
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Everything on one card that a person can actually read.
async fn shown(store: &Store, name: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let knowledge = store.knows().await;
    let entity = knowledge
        .entities
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no card called {name}"));
    (
        entity.facts.iter().map(|f| f.text.clone()).collect(),
        entity.also_known_as.clone(),
        entity
            .relations
            .iter()
            .map(|r| format!("{} {}", r.label, r.name))
            .collect(),
    )
}

/// 1. Family. The reported case, told once.
#[tokio::test]
async fn a_nickname_is_something_the_user_can_see() {
    let store = Store::told(
        "nickname",
        vec![tied(
            named(
                valued(
                    said(
                        "people call my dad Ashok",
                        "Vaidyanathan",
                        "preferred_name",
                        "Vaidyanathan is called Ashok by people close to him",
                    ),
                    "Ashok",
                ),
                &["Ashok"],
            ),
            "father",
            "the user",
        )],
    )
    .await;

    let (facts, known_as, _) = shown(&store, "Vaidyanathan").await;
    assert!(facts.iter().any(|f| f.contains("Ashok")), "{facts:?}");
    assert!(known_as.contains(&"Ashok".to_owned()), "{known_as:?}");
}

/// 2. Family. The reported case, told three times across three sessions. Saying a thing again is
/// how a person insists, and the store has to end up in the same place rather than in three.
#[tokio::test]
async fn saying_it_three_times_leaves_one_fact_and_no_conflict() {
    let line = |turn: &'static str, fact: &'static str| {
        named(
            valued(said(turn, "Vaidyanathan", "preferred_name", fact), "Ashok"),
            &["Ashok"],
        )
    };
    let store = Store::told(
        "insisted",
        vec![
            line("people call my dad Ashok", "Vaidyanathan goes by Ashok"),
            line(
                "my dad is called Ashok by everyone",
                "Everyone calls Vaidyanathan Ashok",
            ),
            line(
                "did I tell you my dad's name is Ashok to his friends",
                "Vaidyanathan is Ashok to his friends",
            ),
        ],
    )
    .await;

    let (facts, known_as, _) = shown(&store, "Vaidyanathan").await;
    assert_eq!(facts.len(), 1, "one fact, said three ways: {facts:?}");
    assert_eq!(known_as, ["Ashok"], "{known_as:?}");

    let knowledge = store.knows().await;
    let entity = &knowledge.entities[0];
    assert!(
        entity.facts.iter().all(|f| f.also_said.is_empty()),
        "insisting is not disagreeing: {:?}",
        entity.facts
    );
}

/// 3. Family. The illogical conflict, exactly as it appeared: two sessions, one name, two wordings.
#[tokio::test]
async fn one_name_worded_two_ways_is_not_a_contradiction() {
    let store = Store::told(
        "two-wordings",
        vec![
            valued(
                said(
                    "my dad's name is Vaidyanathan",
                    "Vaidyanathan",
                    "name",
                    "The user's father's name is Vaidyanathan",
                ),
                "Vaidyanathan",
            ),
            valued(
                said(
                    "his official name is Vaidyanathan",
                    "Vaidyanathan",
                    "name",
                    "Vaidyanathan's official name is Vaidyanathan",
                ),
                "Vaidyanathan",
            ),
        ],
    )
    .await;

    let (facts, _, _) = shown(&store, "Vaidyanathan").await;
    assert_eq!(facts.len(), 1, "{facts:?}");
    let knowledge = store.knows().await;
    assert!(
        knowledge.entities[0]
            .facts
            .iter()
            .all(|f| f.also_said.is_empty()),
        "nothing here disagrees with anything"
    );
}

/// 4. The same shape somewhere else entirely. "I live in Chennai" and "my city is Chennai" are one
/// fact, and without a value they were a contradiction on a single-valued attribute.
#[tokio::test]
async fn one_city_worded_two_ways_is_not_a_contradiction() {
    let store = Store::told(
        "city-wordings",
        vec![
            valued(
                said(
                    "I live in Chennai",
                    "the user",
                    "city",
                    "The user lives in Chennai",
                ),
                "Chennai",
            ),
            valued(
                said(
                    "my city is Chennai",
                    "the user",
                    "city",
                    "The user's city is Chennai",
                ),
                "Chennai",
            ),
        ],
    )
    .await;

    let knowledge = store.knows().await;
    let facts: Vec<&str> = knowledge.entities[0]
        .facts
        .iter()
        .map(|f| f.text.as_str())
        .collect();
    assert_eq!(facts.len(), 1, "{facts:?}");
}

/// 5. And the opposite must not break. A different value on a single-valued attribute is still a
/// correction, which is the whole reason cardinality exists.
#[tokio::test]
async fn a_different_value_still_supersedes() {
    let store = Store::told(
        "moved",
        vec![
            valued(
                said(
                    "I live in Chennai",
                    "the user",
                    "city",
                    "The user lives in Chennai",
                ),
                "Chennai",
            ),
            valued(
                said(
                    "I have moved to Bangalore",
                    "the user",
                    "city",
                    "The user lives in Bangalore",
                ),
                "Bangalore",
            ),
        ],
    )
    .await;

    let knowledge = store.knows().await;
    let facts: Vec<&str> = knowledge.entities[0]
        .facts
        .iter()
        .map(|f| f.text.as_str())
        .collect();
    assert_eq!(facts, ["The user lives in Bangalore"], "{facts:?}");
}

/// 6. A project with a codename. Same class, no people in it.
#[tokio::test]
async fn a_project_codename_is_visible_and_findable() {
    let store = Store::told(
        "codename",
        vec![named(
            thing(said(
                "the platform work is officially Advanced Tooling, we call it ATLAS",
                "Advanced Tooling",
                "codename",
                "Advanced Tooling is called ATLAS internally",
            )),
            &["ATLAS"],
        )],
    )
    .await;

    let (facts, known_as, _) = shown(&store, "Advanced Tooling").await;
    assert!(facts.iter().any(|f| f.contains("ATLAS")), "{facts:?}");
    assert_eq!(known_as, ["ATLAS"]);
    assert!(
        !store.recall("ATLAS").is_empty(),
        "the codename has to find the project"
    );
}

/// 7. Work. A colleague marries and the surname changes. One person, both names.
#[tokio::test]
async fn a_new_surname_leaves_one_card_with_both_names() {
    let store = Store::told(
        "surname",
        vec![
            said(
                "Meera Raghunathan runs infra",
                "Meera Raghunathan",
                "team",
                "Meera runs the infra team",
            ),
            named(
                said(
                    "Meera is Meera Iyer now",
                    "Meera Raghunathan",
                    "surname",
                    "Meera's surname is now Iyer",
                ),
                &["Meera Iyer", "Meera"],
            ),
        ],
    )
    .await;

    let (_, known_as, _) = shown(&store, "Meera Raghunathan").await;
    assert!(known_as.contains(&"Meera Iyer".to_owned()), "{known_as:?}");
    let knowledge = store.knows().await;
    assert_eq!(knowledge.entities.len(), 1, "one colleague, not two");
}

/// 8. A name in another script with a romanisation people actually type.
#[tokio::test]
async fn a_romanisation_finds_the_original() {
    let store = Store::told(
        "romanised",
        vec![named(
            said(
                "陳美玲 signs her mail Meiling",
                "陳美玲",
                "preferred_name",
                "陳美玲 signs her mail as Meiling",
            ),
            &["Meiling"],
        )],
    )
    .await;

    let (_, known_as, _) = shown(&store, "陳美玲").await;
    assert_eq!(known_as, ["Meiling"]);
    assert!(
        !store.recall("Meiling").is_empty(),
        "the name people type has to find the card"
    );
}

/// 9. Medicine. A brand and a generic are one thing with two names, and getting that wrong is the
/// kind of mistake that matters outside a demo.
#[tokio::test]
async fn a_brand_name_and_a_generic_are_one_thing() {
    let store = Store::told(
        "medicine",
        vec![named(
            thing(said(
                "I take metformin, the brand is Glycomet",
                "metformin",
                "brand",
                "The user's metformin is the brand Glycomet",
            )),
            &["Glycomet"],
        )],
    )
    .await;

    let (facts, known_as, _) = shown(&store, "metformin").await;
    assert!(facts.iter().any(|f| f.contains("Glycomet")), "{facts:?}");
    assert_eq!(known_as, ["Glycomet"]);
}

/// 10. An edge stated plainly. The same invisibility as a nickname: it was stored in frontmatter
/// and the card said nothing about it, so three relations sat on the owner unseen.
#[tokio::test]
async fn a_relationship_is_visible_on_the_card() {
    let store = Store::told(
        "edge",
        vec![
            valued(
                said(
                    "my name is Sabharish",
                    "the user",
                    "name",
                    "The user's name is Sabharish",
                ),
                "Sabharish",
            ),
            tied(
                said("Zoe is my manager", "Zoe", "job", "Zoe manages the user"),
                "manager",
                "the user",
            ),
        ],
    )
    .await;

    let (_, _, edges) = shown(&store, "Sabharish").await;
    assert_eq!(edges, ["manager Zoe"], "{edges:?}");
}

/// 11. And the user gets the last word on both. A name that is wrong goes; an edge that has ended
/// closes, because a manager who changed is not a manager who was never yours.
#[tokio::test]
async fn the_user_can_take_a_name_or_an_edge_back() {
    let store = Store::told(
        "undo",
        vec![
            valued(
                said(
                    "my name is Sabharish",
                    "the user",
                    "name",
                    "The user's name is Sabharish",
                ),
                "Sabharish",
            ),
            tied(
                named(
                    said("Zoe is my manager", "Zoe", "job", "Zoe manages the user"),
                    &["Zo"],
                ),
                "manager",
                "the user",
            ),
        ],
    )
    .await;

    store
        .memory
        .forget_alias("people/zoe.md", "Zo")
        .await
        .expect("forget alias");
    store
        .memory
        .forget_relation("people/you.md", "manager", "people/zoe.md", today())
        .await
        .expect("close relation");

    let (_, known_as, _) = shown(&store, "Zoe").await;
    assert!(known_as.is_empty(), "{known_as:?}");
    let (_, _, edges) = shown(&store, "Sabharish").await;
    assert!(edges.is_empty(), "{edges:?}");
}
