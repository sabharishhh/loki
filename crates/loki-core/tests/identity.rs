//! Who an entity is, as opposed to what it is called (§9.4, S-21).
//!
//! Every failure this suite guards is one of two shapes: one thing referred to two ways becoming
//! two cards, or two things sharing a name becoming one. They are opposite errors on one dial, and
//! a fix for either can make the other worse, so both directions are tested together on purpose.

use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::bundle::{ASSISTANT, Bundle, OWNER};
use loki_core::memory::concept::{Label, Role};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::Index;

fn today() -> Date {
    date(2026, 9, 3)
}

struct Store {
    memory: Memory,
    dir: std::path::PathBuf,
}

impl Store {
    async fn open(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-identity-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self::reopen(dir, label).await
    }

    async fn reopen(dir: std::path::PathBuf, label: &str) -> Self {
        let memory = Memory::open(
            &dir,
            Index::in_memory().expect("index"),
            label,
            today(),
            TierScope::normal(Locality::Cloud),
        )
        .await
        .expect("open");
        Self { memory, dir }
    }

    async fn card(&self, path: &str) -> loki_core::memory::concept::RawConcept {
        let bundle = Bundle::open(&self.dir).await.expect("bundle");
        let reader = bundle.reader().await;
        reader.load_concept(path).expect("card")
    }

    /// Where blocking would send a claim about this surface form.
    fn blocks_to(&self, surface: &str) -> Vec<String> {
        self.memory
            .index()
            .candidates(surface, &[], 5)
            .expect("candidates")
            .into_iter()
            .map(|c| c.path)
            .collect()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// §11.3 needs the owner to exist before the first episode, or every import writes another person
/// for the same "I". Seeded on open, not on first mention.
#[tokio::test]
async fn the_owner_and_the_assistant_exist_before_anything_is_said() {
    let store = Store::open("seeded").await;

    let owner = store.card(OWNER).await;
    assert_eq!(owner.front.role, Role::Owner);
    assert_eq!(
        owner.front.label,
        Label::Described,
        "nobody has given a name yet, and the card should say so"
    );
    assert!(owner.claims().next().is_none(), "seeded, not populated");

    let loki = store.card(ASSISTANT).await;
    assert_eq!(loki.front.role, Role::Assistant);
    assert_eq!(loki.front.name, "Loki");
}

/// An empty card is not knowledge. Seeding must not make the timeline claim to know two people.
#[tokio::test]
async fn a_seeded_card_is_not_something_loki_knows() {
    let store = Store::open("empty-timeline").await;
    let knowledge = store.memory.knowledge(today()).await.expect("knowledge");
    assert!(
        knowledge.entities.is_empty(),
        "{:?}",
        knowledge
            .entities
            .iter()
            .map(|e| &e.name)
            .collect::<Vec<_>>()
    );
}

/// The point of seeding the aliases: a fact stated in the third person lands on the same card as
/// one stated in the first. Probe case 17.
#[tokio::test]
async fn the_third_person_and_the_first_reach_the_same_card() {
    let store = Store::open("voices").await;
    for form in ["the user", "me", "myself"] {
        assert_eq!(
            store.blocks_to(form),
            [OWNER],
            "{form:?} should reach the owner"
        );
    }
}

/// The correction to the plan. Inside a conversation "you" is Loki, so an owner that answered to
/// it would file "you are Loki" onto the user. See D-066.
#[tokio::test]
async fn you_is_the_assistant_and_never_the_owner() {
    let store = Store::open("you").await;
    let hits = store.blocks_to("you");
    assert!(hits.contains(&ASSISTANT.to_owned()), "{hits:?}");
    assert!(
        !hits.contains(&OWNER.to_owned()),
        "the owner must not answer to \"you\": {hits:?}"
    );
}

/// Reopening is what a person does every day. Seeding twice would give two owners, which is worse
/// than none: blocking would then have to choose between them for every "I".
#[tokio::test]
async fn reopening_does_not_seed_a_second_pair() {
    let store = Store::open("reopen").await;
    let dir = store.dir.clone();
    let owner_was = store.card(OWNER).await;
    std::mem::forget(store);

    let again = Store::reopen(dir, "second").await;
    let owner_now = again.card(OWNER).await;
    assert_eq!(owner_was.front, owner_now.front, "the card is untouched");

    let bundle = Bundle::open(&again.dir).await.expect("bundle");
    let reader = bundle.reader().await;
    let people = reader.concepts().expect("concepts");
    assert_eq!(people.len(), 2, "{people:?}");
}
