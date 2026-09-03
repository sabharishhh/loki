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

/// Cardinality, end to end (S-22). Two claims on one attribute are two facts or a correction, and
/// which one it is depends on the property, not on the shape of the sentence.
mod cardinality {
    use super::{Store, today};
    use async_trait::async_trait;
    use loki_core::memory::claim::Origin;
    use loki_core::memory::consolidate::{Candidate, ConsolidateError, Extractor, Unbounded};
    use loki_core::memory::index::Candidate as EntityCandidate;
    use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

    /// Emits one fact per staged line, keyed on a fragment of what was said.
    struct Says(Vec<(&'static str, &'static str, &'static str)>);

    #[async_trait]
    impl Extractor for Says {
        async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
            Ok(self
                .0
                .iter()
                .filter(|(trigger, _, _)| text.contains(trigger))
                .map(|(_, attribute, fact)| Candidate {
                    surface: "Sabharish".to_owned(),
                    kind: Kind::Person,
                    heading: (*attribute).to_owned(),
                    attribute: (*attribute).to_owned(),
                    text: (*fact).to_owned(),
                    days_ago: None,
                    valid_from: None,
                    origin: Origin::Stated,
                    tags: vec![],
                    aliases: vec![],
                    relation: None,
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
            candidates: &[EntityCandidate],
        ) -> Result<Decision, ResolveError> {
            Ok(if candidates.is_empty() {
                Decision::New
            } else {
                Decision::Existing(0)
            })
        }
    }

    /// Runs the staged lines one session per line, which is what a person shutting the app
    /// between thoughts actually produces, and returns what the store believes about them.
    async fn believed(
        label: &str,
        staged: &[(&'static str, &'static str, &'static str)],
    ) -> Vec<String> {
        let store = Store::open(label).await;
        let extractor = Says(staged.to_vec());
        for (said, _, _) in staged {
            store.memory.record("user", said).await.expect("record");
            store
                .memory
                .close(&extractor, &FirstMatch, &Unbounded, today())
                .await
                .expect("close");
        }
        store
            .memory
            .knowledge(today())
            .await
            .expect("knowledge")
            .entities
            .into_iter()
            .flat_map(|e| e.facts)
            .map(|f| f.text)
            .collect()
    }

    /// Probe case 10. Sabharish's own case: a certificate on top of a degree, both true.
    #[tokio::test]
    async fn a_certificate_does_not_retire_a_degree() {
        let facts = believed(
            "add",
            &[
                (
                    "degree",
                    "education",
                    "Sabharish has a degree in computer science",
                ),
                (
                    "certified",
                    "education",
                    "Sabharish is a certified machine learning engineer",
                ),
            ],
        )
        .await;
        assert_eq!(facts.len(), 2, "{facts:?}");
    }

    /// Probe case 11. The same shape, and the opposite right answer.
    #[tokio::test]
    async fn moving_house_replaces_the_old_address() {
        let facts = believed(
            "replace",
            &[
                ("Chennai", "city", "Sabharish lives in Chennai"),
                ("Bangalore", "city", "Sabharish lives in Bangalore"),
            ],
        )
        .await;
        assert_eq!(facts, ["Sabharish lives in Bangalore"], "{facts:?}");
    }

    /// Probe case 12. Two clients is a consultant, not a contradiction, which is why `employer`
    /// is many-valued while the relation of the same name is not.
    #[tokio::test]
    async fn a_consultant_keeps_both_clients() {
        let facts = believed(
            "consult",
            &[
                ("Acme", "employer", "Sabharish consults for Acme"),
                ("Globex", "employer", "Sabharish consults for Globex"),
            ],
        )
        .await;
        assert_eq!(facts.len(), 2, "{facts:?}");
    }

    /// The case that keeps the list from being empty. Two ship dates cannot both be true, and
    /// leaving both live is the PrefEval failure §9.5 exists to prevent.
    #[tokio::test]
    async fn two_ship_dates_leave_one() {
        let facts = believed(
            "ships",
            &[
                ("the 30th", "deadline", "Atlas ships on the 30th"),
                ("the 20th", "deadline", "Atlas ships on the 20th"),
            ],
        )
        .await;
        assert_eq!(facts, ["Atlas ships on the 20th"], "{facts:?}");
    }
}

/// Change C and D (S-21): an entity has names, rather than being one.
mod names {
    use super::{Store, today};
    use async_trait::async_trait;
    use loki_core::memory::claim::Origin;
    use loki_core::memory::concept::Label;
    use loki_core::memory::consolidate::{
        Candidate, ConsolidateError, Extractor, RelationTo, Unbounded,
    };
    use loki_core::memory::index::Candidate as EntityCandidate;
    use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError, looks_described};

    /// One fact, plus whatever the sentence said about what the entity is called.
    pub struct Fact {
        pub trigger: &'static str,
        pub surface: &'static str,
        pub attribute: &'static str,
        pub text: &'static str,
        pub aliases: &'static [&'static str],
        pub relation: Option<(&'static str, &'static str)>,
    }

    struct Says(Vec<Fact>);

    #[async_trait]
    impl Extractor for Says {
        async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
            Ok(self
                .0
                .iter()
                .filter(|f| text.contains(f.trigger))
                .map(|f| Candidate {
                    surface: f.surface.to_owned(),
                    kind: Kind::Person,
                    heading: f.attribute.to_owned(),
                    attribute: f.attribute.to_owned(),
                    text: f.text.to_owned(),
                    days_ago: None,
                    valid_from: None,
                    origin: Origin::Stated,
                    tags: vec![],
                    aliases: f.aliases.iter().map(|a| (*a).to_owned()).collect(),
                    relation: f.relation.map(|(label, of)| RelationTo {
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
            candidates: &[EntityCandidate],
        ) -> Result<Decision, ResolveError> {
            Ok(if candidates.is_empty() {
                Decision::New
            } else {
                Decision::Existing(0)
            })
        }
    }

    pub async fn store_with(label: &str, facts: Vec<Fact>) -> Store {
        let store = Store::open(label).await;
        let said: Vec<&str> = facts.iter().map(|f| f.trigger).collect();
        let extractor = Says(facts);
        for line in said {
            store.memory.record("user", line).await.expect("record");
            store
                .memory
                .close(&extractor, &FirstMatch, &Unbounded, today())
                .await
                .expect("close");
        }
        store
    }

    /// A descriptor is a placeholder. A name is a name. Deciding that with a rule rather than a
    /// model call is what makes the absorption below safe to do automatically.
    #[test]
    fn a_description_is_told_from_a_name() {
        for described in [
            "the user's sister",
            "the client from Tuesday",
            "my father",
            "her manager",
            "a colleague",
        ] {
            assert!(looks_described(described), "{described}");
        }
        for named in ["Meera", "Meera Raghunathan", "陳美玲", "Atlas", "O'Brien"] {
            assert!(!looks_described(named), "{named}");
        }
    }

    /// Probe case 5. One person, a formal name and the one everyone uses. Without the alias the
    /// second name is unfindable, which is the split entity failure wearing a different hat.
    #[tokio::test]
    async fn a_nickname_becomes_a_way_of_finding_someone() {
        let store = store_with(
            "nickname",
            vec![Fact {
                trigger: "prefers to be called",
                surface: "Vaidyanathan",
                attribute: "preferred_name",
                text: "Vaidyanathan prefers to be called Ashok",
                aliases: &["Ashok"],
                relation: None,
            }],
        )
        .await;

        assert_eq!(
            store.blocks_to("Ashok"),
            ["people/vaidyanathan.md"],
            "the nickname has to reach the same card"
        );
        assert_eq!(store.blocks_to("Vaidyanathan"), ["people/vaidyanathan.md"]);
    }

    /// Probe case 2. Meera marries and is Meera Iyer. The path is the identity, so the file stays
    /// where it is and both forms keep working.
    #[tokio::test]
    async fn a_new_surname_keeps_the_same_card() {
        let store = store_with(
            "rename",
            vec![
                Fact {
                    trigger: "Meera is on the infra team",
                    surface: "Meera",
                    attribute: "team",
                    text: "Meera is on the infra team",
                    aliases: &[],
                    relation: None,
                },
                Fact {
                    trigger: "Meera Iyer got promoted",
                    surface: "Meera",
                    attribute: "role",
                    text: "Meera Iyer got promoted",
                    aliases: &["Meera Iyer"],
                    relation: None,
                },
            ],
        )
        .await;

        for form in ["Meera", "Meera Iyer"] {
            assert_eq!(store.blocks_to(form), ["people/meera.md"], "{form}");
        }
        let card = store.card("people/meera.md").await;
        assert_eq!(card.claims().count(), 2, "one card, both facts");
    }

    /// Probe case 1's second half. A placeholder card that later learns a real name adopts it and
    /// keeps the old wording as a way of finding it.
    #[tokio::test]
    async fn a_real_name_absorbs_a_placeholder() {
        let store = store_with(
            "absorb",
            vec![
                Fact {
                    trigger: "my sister is studious",
                    surface: "the user's sister",
                    attribute: "trait",
                    text: "The user's sister is studious",
                    aliases: &[],
                    relation: Some(("sister", "the user")),
                },
                Fact {
                    trigger: "Lakshmi finished her exams",
                    surface: "Lakshmi",
                    attribute: "status",
                    text: "Lakshmi finished her exams",
                    aliases: &[],
                    relation: Some(("sister", "the user")),
                },
            ],
        )
        .await;

        let knowledge = store.memory.knowledge(today()).await.expect("knowledge");
        assert_eq!(
            knowledge.entities.len(),
            1,
            "one sister, not two: {:?}",
            knowledge
                .entities
                .iter()
                .map(|e| (&e.name, &e.path))
                .collect::<Vec<_>>()
        );
        let card = store.card(&knowledge.entities[0].path).await;
        assert_eq!(card.front.name, "Lakshmi");
        assert_eq!(card.front.label, Label::Named);
        assert!(
            card.front.answers_to("the user's sister"),
            "the old wording still finds her: {:?}",
            card.front.aliases
        );
    }
}

/// Change E (S-21). A relationship is an edge, not a sentence somebody has to reread.
mod relations {
    use super::names::{Fact, store_with};
    use super::today;
    use loki_core::memory::bundle::OWNER;

    /// Probe case 7. One label, two targets, and both correct. Relations are many-valued by
    /// default precisely so this does not regress: prose got it right by accident, and a naive
    /// one-slot edge would have been worse than the prose.
    #[tokio::test]
    async fn two_brothers_are_two_edges() {
        let store = store_with(
            "brothers",
            vec![
                Fact {
                    trigger: "Arjun is a doctor",
                    surface: "Arjun",
                    attribute: "job",
                    text: "Arjun is a doctor",
                    aliases: &[],
                    relation: Some(("brother", "the user")),
                },
                Fact {
                    trigger: "Karthik is a lawyer",
                    surface: "Karthik",
                    attribute: "job",
                    text: "Karthik is a lawyer",
                    aliases: &[],
                    relation: Some(("brother", "the user")),
                },
            ],
        )
        .await;

        let owner = store.card(OWNER).await;
        let brothers: Vec<&str> = owner
            .front
            .relations
            .iter()
            .filter(|r| r.label == "brother" && r.is_current())
            .map(|r| r.to.as_str())
            .collect();
        assert_eq!(brothers.len(), 2, "{brothers:?}");

        // And "my brother" has no singular answer, which is the honest result rather than a coin
        // toss between two true ones.
        assert_eq!(owner.front.related("brother"), None);
    }

    /// Probe case 9. A manager changes. The old edge closes the way a claim's window does, and
    /// nothing is deleted, so the store can still say who it used to be.
    #[tokio::test]
    async fn a_manager_who_changes_closes_the_old_edge() {
        let store = store_with(
            "manager",
            vec![
                Fact {
                    trigger: "my manager is Zoe",
                    surface: "Zoe",
                    attribute: "job",
                    text: "Zoe manages the user",
                    aliases: &[],
                    relation: Some(("manager", "the user")),
                },
                Fact {
                    trigger: "Priya is my manager now",
                    surface: "Priya",
                    attribute: "job",
                    text: "Priya manages the user",
                    aliases: &[],
                    relation: Some(("manager", "the user")),
                },
            ],
        )
        .await;

        let owner = store.card(OWNER).await;
        assert_eq!(
            owner.front.related("manager"),
            Some("people/priya.md"),
            "{:?}",
            owner.front.relations
        );
        let closed = owner
            .front
            .relations
            .iter()
            .find(|r| r.to == "people/zoe.md")
            .expect("Zoe is still there");
        assert_eq!(closed.until, Some(today()), "closed, not deleted");
    }

    /// Probe case 8. My father's brother. Reachable only because both hops are edges: the first
    /// resolves "my father" to a card, and the second hangs off that card rather than off a name.
    #[tokio::test]
    async fn a_relative_two_hops_away_is_reachable() {
        let store = store_with(
            "two-hops",
            vec![
                Fact {
                    trigger: "my father is Vaidyanathan",
                    surface: "Vaidyanathan",
                    attribute: "job",
                    text: "Vaidyanathan is retired",
                    aliases: &[],
                    relation: Some(("father", "the user")),
                },
                Fact {
                    trigger: "Vaidyanathan's brother is Ramesh",
                    surface: "Ramesh",
                    attribute: "job",
                    text: "Ramesh is an engineer",
                    aliases: &[],
                    relation: Some(("brother", "Vaidyanathan")),
                },
            ],
        )
        .await;

        let owner = store.card(OWNER).await;
        let father = owner.front.related("father").expect("father");
        assert_eq!(father, "people/vaidyanathan.md");

        let card = store.card(father).await;
        assert_eq!(card.front.related("brother"), Some("people/ramesh.md"));
    }

    /// The failure that made this necessary. A descriptor near-name-matches the person it
    /// describes, and merging there writes a fact about one person onto another.
    #[tokio::test]
    async fn a_descriptor_never_merges_into_what_it_describes() {
        let store = store_with(
            "descriptor",
            vec![Fact {
                trigger: "my sister is studious",
                surface: "the user's sister",
                attribute: "trait",
                text: "The user's sister is studious",
                aliases: &[],
                relation: Some(("sister", "the user")),
            }],
        )
        .await;

        let owner = store.card(OWNER).await;
        assert!(
            owner.claims().next().is_none(),
            "the sister's fact must not land on the user: {:?}",
            owner.claims().map(|c| &c.text).collect::<Vec<_>>()
        );
        assert!(owner.front.related("sister").is_some());
    }
}
