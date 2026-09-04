//! §17.3's trust surface: what Loki knows, and what a person can do about it.
//!
//! The screen is the product's trust surface, so these run against a real bundle, a real index and
//! real git rather than against in-memory structures. A control that works on a struct and not on
//! the store is exactly the failure this surface exists to make impossible.

use std::sync::Arc;

use jiff::civil::{Date, date};
use loki_core::core::event::Event;
use loki_core::core::sink::{Broadcast, Collector};
use loki_core::core::vocab::Locality;
use loki_core::memory::claim::Claim;
use loki_core::memory::concept::{Frontmatter, RawConcept, Status};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::Index;

fn today() -> Date {
    date(2026, 9, 2)
}

struct Store {
    memory: Memory,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str, concepts: &[(&str, RawConcept)]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-knowledge-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let index = Index::in_memory().expect("index");
        let memory = Memory::open(
            &dir,
            index,
            "session",
            today(),
            TierScope::normal(Locality::Cloud),
            Arc::new(Broadcast::new()),
        )
        .await
        .expect("open");

        {
            let writer = memory.bundle().writer().await;
            for (path, concept) in concepts {
                writer.save_concept(path, concept).expect("write");
            }
            writer.commit("fixture").expect("commit");
        }
        // The fixture is written after `open`, so the index has to catch up. Without this a
        // recall assertion measures an empty index rather than the rule under test.
        {
            let reader = memory.bundle().reader().await;
            memory.index().sync(&reader).expect("sync");
        }
        Self { memory, dir }
    }

    async fn concept(&self, path: &str) -> RawConcept {
        let reader = self.memory.bundle().reader().await;
        reader.load_concept(path).expect("load")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A concept with an open conflict: two believed claims about one attribute, plus a fact that has
/// nothing to do with either.
///
/// `stable`, deliberately. Rule 4 takes the two claims out of use and nothing else with them, so a
/// concept holding a question is still a concept Loki uses.
fn contested() -> RawConcept {
    let mut front = Frontmatter::new("Sabharish", date(2026, 1, 1));
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);
    concept.add(
        "city",
        Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
    );
    concept.add(
        "city",
        Claim::stated("Sabharish lives in Bangalore", date(2026, 1, 1)).about("city"),
    );
    concept.add(
        "education",
        Claim::stated("Sabharish is a computer science graduate", date(2026, 1, 1))
            .about("education"),
    );
    concept
}

/// Rule 4 under option A: the newer statement is used at once, the older hangs off it, and one tap
/// makes the choice permanent. Nothing blocks while the user has not looked.
#[tokio::test]
async fn one_tap_settles_a_conflict_the_store_had_already_decided() {
    let store = Store::new("settle", &[("people/sabharish.md", contested())]).await;

    let before = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .remove(0);
    assert_eq!(
        before.facts.len(),
        2,
        "one row per property, and a disagreement is not a row of its own: {:?}",
        before.facts
    );
    let city = before
        .facts
        .iter()
        .find(|f| f.attribute == "city")
        .expect("city is decided, not deferred");
    assert_eq!(
        city.text, "Sabharish lives in Bangalore",
        "the later statement is the one in use"
    );
    assert_eq!(city.also_said.len(), 1, "the earlier one is offered back");

    let keep = city.ordinal;
    store
        .memory
        .settle("people/sabharish.md", keep, today())
        .await
        .expect("settle");

    let after = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .remove(0);
    assert_eq!(after.facts.len(), 2, "{after:?}");
    assert!(
        after.facts.iter().all(|f| f.also_said.is_empty()),
        "settling closes the loose end: {:?}",
        after.facts
    );
    assert!(after.confirmed, "a person picked, so nothing may decay it");

    let bangalore = after
        .facts
        .iter()
        .find(|f| f.attribute == "city")
        .expect("the kept claim is now a fact");
    assert_eq!(bangalore.text, "Sabharish lives in Bangalore");
    assert!(
        bangalore.was.is_some(),
        "the losing side is not deleted, it becomes what this replaced"
    );

    // Nothing is removed by a tap. The retired claim is still in the file, for git and the record.
    let concept = store.concept("people/sabharish.md").await;
    assert_eq!(concept.claims().count(), 3);
}

/// Editing a row is a supersession, not an overwrite. Principle 6 holds for a hand edit exactly as
/// it does for a model one.
#[tokio::test]
async fn an_edit_supersedes_rather_than_overwrites() {
    let mut concept = RawConcept::new(Frontmatter::new("Sabharish", date(2026, 1, 1)));
    concept.front.status = Status::Stable;
    concept.add(
        "city",
        Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
    );
    let store = Store::new("amend", &[("people/sabharish.md", concept)]).await;

    store
        .memory
        .amend(
            "people/sabharish.md",
            0,
            "Sabharish lives in Bangalore",
            today(),
        )
        .await
        .expect("amend");

    let entity = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .remove(0);
    assert_eq!(entity.facts.len(), 1, "one row: {:?}", entity.facts);
    assert_eq!(entity.facts[0].text, "Sabharish lives in Bangalore");
    assert_eq!(
        entity.facts[0]
            .was
            .as_ref()
            .expect("the row says what it replaced")
            .text,
        "Sabharish lives in Chennai"
    );
    assert!(entity.confirmed, "the user typed it, so it is confirmed");

    let file = store.concept("people/sabharish.md").await;
    assert_eq!(file.claims().count(), 2, "the old wording is kept");
    assert_eq!(
        file.claims().next().expect("old").attribute,
        "city",
        "an edit stays about the same property"
    );
}

/// Delete retires, and never removes. A store that deletes on a tap cannot show what it used to
/// think, which is the whole of §17.3.
#[tokio::test]
async fn forgetting_retires_a_claim_without_removing_it() {
    let mut concept = RawConcept::new(Frontmatter::new("Sabharish", date(2026, 1, 1)));
    concept.front.status = Status::Stable;
    concept.add(
        "city",
        Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
    );
    let store = Store::new("forget", &[("people/sabharish.md", concept)]).await;

    store
        .memory
        .forget("people/sabharish.md", 0, today())
        .await
        .expect("forget");

    let entity = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .remove(0);
    assert!(entity.facts.is_empty(), "{:?}", entity.facts);

    let file = store.concept("people/sabharish.md").await;
    let claim = file.claims().next().expect("still on disk");
    assert!(!claim.validity.is_believed());
    assert_eq!(
        claim.replaced_by, None,
        "nothing replaced it, the user simply said no"
    );
}

/// A control that names a claim that is not there must say so rather than doing something else.
#[tokio::test]
async fn an_ordinal_that_names_nothing_is_an_error_not_a_guess() {
    let store = Store::new("bad-ordinal", &[("people/sabharish.md", contested())]).await;

    assert!(
        store
            .memory
            .settle("people/sabharish.md", 99, today())
            .await
            .is_err()
    );
    assert!(
        store
            .memory
            .forget("people/sabharish.md", 99, today())
            .await
            .is_err()
    );
    assert!(
        store
            .memory
            .amend("people/sabharish.md", 99, "anything", today())
            .await
            .is_err()
    );
}

/// Every hand edit commits, so `git revert` is a real recovery path (§14.3).
#[tokio::test]
async fn a_hand_edit_lands_as_a_commit() {
    let store = Store::new("commits", &[("people/sabharish.md", contested())]).await;
    let before = {
        let reader = store.memory.bundle().reader().await;
        reader.history(20).expect("history").len()
    };

    store
        .memory
        .settle("people/sabharish.md", 1, today())
        .await
        .expect("settle");

    let after = {
        let reader = store.memory.bundle().reader().await;
        reader.history(20).expect("history")
    };
    assert!(after.len() > before, "{after:?}");
}

/// The Swift decoder names these fields explicitly, and a rename on this side would fail silently:
/// `Decodable` throws, the screen catches nothing, and the user sees an empty store. Pinning the
/// wire shape here means the break is a failing test rather than a blank screen.
#[tokio::test]
async fn the_wire_shape_the_app_decodes_is_fixed() {
    let store = Store::new("wire", &[("people/sabharish.md", contested())]).await;
    let knowledge = store.memory.knowledge(today()).await.expect("knowledge");
    let json = serde_json::to_string(&knowledge).expect("serialize");

    for field in [
        "\"entities\"",
        "\"path\"",
        "\"name\"",
        "\"kind\"",
        "\"in_use\"",
        "\"confirmed\"",
        "\"facts\"",
        "\"ordinal\"",
        "\"attribute\"",
        "\"text\"",
        "\"since\"",
        "\"was\"",
        "\"from_elsewhere\"",
        "\"also_said\"",
    ] {
        assert!(json.contains(field), "{field} missing from {json}");
    }
}

/// A correction's own fields, which only appear once something has been superseded.
#[tokio::test]
async fn a_correction_serializes_the_fields_the_app_reads() {
    let store = Store::new("wire-correction", &[("people/sabharish.md", contested())]).await;
    store
        .memory
        .settle("people/sabharish.md", 1, today())
        .await
        .expect("settle");

    let json = serde_json::to_string(&store.memory.knowledge(today()).await.expect("knowledge"))
        .expect("serialize");
    assert!(json.contains("\"held\""), "{json}");
    // `wrong_for` is skipped when there was no gap, so assert the key the app reads exists on the
    // shape rather than in this particular instance.
    assert!(json.contains("\"was\""), "{json}");
}

/// Found in Sabharish's store, 2026-09-02. The name was stored, correct, stated and high
/// confidence, and Loki answered "I don't know your name yet".
///
/// Rule 4 says "mark both uncertain, surface it, use neither". *Both* is the two conflicting
/// claims. The implementation dropped the whole concept to `draft`, so one argument about a
/// degree took the person's name out of use with it.
#[tokio::test]
async fn a_conflict_about_one_thing_does_not_hide_everything_else() {
    let store = Store::new("blast-radius", &[("people/sabharish.md", contested())]).await;

    let entity = store
        .memory
        .knowledge(today())
        .await
        .expect("knowledge")
        .entities
        .remove(0);

    let usable: Vec<&str> = entity.facts.iter().map(|f| f.attribute.as_str()).collect();
    assert_eq!(
        usable,
        ["city", "education"],
        "a disagreement about one property costs nothing anywhere else"
    );

    let recalled = store
        .memory
        .recall("what did Sabharish study", 0, today())
        .expect("recall");
    assert!(
        recalled.iter().any(|r| r.text.contains("graduate")),
        "an open question about the city must not make the degree unreachable: {recalled:?}"
    );
    assert!(
        !recalled.iter().any(|r| r.text.contains("Chennai")),
        "the shadowed claim never reaches a prompt: {recalled:?}"
    );
}

/// B-59. §17.3's rule applied to the case where the file cannot be read at all.
///
/// A card that will not parse used to vanish from this screen while the index carried on serving
/// it, so the one surface built for checking the work was the only place it disappeared from.
#[tokio::test]
async fn a_card_that_will_not_parse_is_reported_rather_than_omitted() {
    let store = Store::new("unreadable", &[("people/sabharish.md", contested())]).await;

    {
        let writer = store.memory.bundle().writer().await;
        writer
            .write(
                "people/broken.md",
                "---\nname: Broken\nstatus: stable\ngenerated:\n  by: loki\n  at: 2026-01-01\n---\n\n## R\n- A claim\n  learned 2026-01-01\n",
            )
            .expect("write");
    }

    let known = store.memory.knowledge(today()).await.expect("knowledge");

    assert_eq!(known.unreadable.len(), 1, "the card has to be reported");
    assert_eq!(known.unreadable[0].path, "people/broken.md");
    assert!(
        known.unreadable[0].detail.contains("11"),
        "the detail names the line, which is what makes it fixable: {:?}",
        known.unreadable[0].detail
    );
    assert!(
        known.entities.iter().all(|e| e.path != "people/broken.md"),
        "it is reported as unreadable, not listed as knowledge"
    );
    assert_eq!(known.entities.len(), 1, "the readable card is unaffected");
}

/// A store with a broken card already on disk, and a sink that keeps what it is told.
async fn watched(
    label: &str,
    files: &[(&str, &str)],
) -> (std::path::PathBuf, Arc<Collector>, Memory) {
    let dir = std::env::temp_dir().join(format!(
        "loki-events-{}-{label}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    // Written before `Memory::open`, because a card that is already broken at session start is
    // the case an after-the-fact hook would miss.
    let bundle = loki_core::memory::bundle::Bundle::open(&dir)
        .await
        .expect("bundle");
    {
        let writer = bundle.writer().await;
        for (path, text) in files {
            writer.write(path, text).expect("write");
        }
        writer.commit("fixture").expect("commit");
    }
    drop(bundle);

    let events = Arc::new(Collector::new());
    let memory = Memory::open(
        &dir,
        Index::in_memory().expect("index"),
        "watched",
        today(),
        TierScope::normal(Locality::Cloud),
        Arc::clone(&events) as Arc<dyn loki_core::core::sink::EventSink>,
    )
    .await
    .expect("open");
    (dir, events, memory)
}

const BROKEN: &str = "---\nname: Broken\nstatus: stable\ngenerated:\n  by: loki\n  at: 2026-01-01\n---\n\n## R\n- A claim\n  learned 2026-01-01\n";
const WHOLE: &str = "---\nname: Whole\nstatus: stable\ngenerated:\n  by: loki\n  at: 2026-01-01\n---\n\n## R\n- A claim\n  learned: 2026-01-01\n";

fn unreadable_in(events: &Collector) -> Vec<(String, String)> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::MemoryUnreadable { concept_id, detail } => {
                Some((concept_id.as_str().to_owned(), detail))
            }
            _ => None,
        })
        .collect()
}

/// B-59, the half that was left open. Principle 7: an act that emits nothing is invisible to a
/// stream written only by the code that chose to write it. These three are one family.
#[tokio::test]
async fn a_card_broken_before_the_session_starts_is_reported_at_open() {
    let (dir, events, _memory) = watched("at-open", &[("people/broken.md", BROKEN)]).await;

    let found = unreadable_in(&events);
    assert_eq!(
        found.len(),
        1,
        "the event has to fire during open: {found:?}"
    );
    assert_eq!(found[0].0, "people/broken.md", "it names the file");
    assert!(
        found[0].1.contains("11"),
        "and the line, which is what makes it fixable: {}",
        found[0].1
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The other moment: a file that was fine at open and is edited during the session.
#[tokio::test]
async fn a_card_that_breaks_mid_session_is_reported_on_the_next_sync() {
    let (dir, events, memory) = watched(
        "mid-session",
        &[("people/whole.md", WHOLE), ("people/other.md", WHOLE)],
    )
    .await;
    assert!(
        unreadable_in(&events).is_empty(),
        "a whole store says nothing"
    );

    {
        let writer = memory.bundle().writer().await;
        writer.write("people/whole.md", BROKEN).expect("break it");
    }
    // The realistic shape: a person breaks one card in an editor, and the next thing Loki does is
    // an ordinary correction on a different one. An operation on the broken card cannot be the
    // trigger, because it fails on the same parse before it reaches the sync.
    memory
        .contradict("people/other.md", 0)
        .await
        .expect("a correction elsewhere must not fail over one bad file");

    let found = unreadable_in(&events);
    assert_eq!(found.len(), 1, "the edit has to be reported: {found:?}");
    assert_eq!(found[0].0, "people/whole.md");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Silence when there is nothing wrong. An event that fires on a healthy store is noise, and
/// noise is what gets a stream ignored.
#[tokio::test]
async fn a_store_that_reads_cleanly_emits_nothing_about_it() {
    let (dir, events, _memory) = watched("quiet", &[("people/whole.md", WHOLE)]).await;

    assert!(
        unreadable_in(&events).is_empty(),
        "nothing is wrong, so nothing is said: {:?}",
        events.events()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
