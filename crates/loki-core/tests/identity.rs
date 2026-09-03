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

    /// What lane 1 returns for a question.
    fn recall(&self, question: &str) -> Vec<String> {
        self.memory
            .index()
            .recall(&loki_core::memory::index::Query::prefetch(
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

    /// Probe cases 6 and 17. The user's name arrives as a fact about `the user`, so it names the
    /// card that already exists rather than creating a second person, and every later mention of
    /// that name reaches the same card.
    #[tokio::test]
    async fn the_owner_learns_a_name_without_becoming_a_second_person() {
        let store = store_with(
            "owner-name",
            vec![
                Fact {
                    trigger: "my name is Sabharish",
                    surface: "the user",
                    attribute: "name",
                    text: "The user's name is Sabharish",
                    aliases: &["Sabharish"],
                    relation: None,
                },
                Fact {
                    trigger: "the user studied computer science",
                    surface: "the user",
                    attribute: "education",
                    text: "The user studied computer science",
                    aliases: &[],
                    relation: None,
                },
            ],
        )
        .await;

        let owner = store.card(loki_core::memory::bundle::OWNER).await;
        assert_eq!(owner.front.name, "Sabharish");
        assert_eq!(owner.front.label, Label::Named);
        assert_eq!(owner.claims().count(), 2, "one card, both voices");
        assert!(
            owner.front.answers_to("the user"),
            "{:?}",
            owner.front.aliases
        );
        assert_eq!(
            store.blocks_to("Sabharish"),
            [loki_core::memory::bundle::OWNER],
            "and the name reaches the owner from now on"
        );
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

/// Merge safety. The opposite error from a split, quieter and more damaging: a split leaves two
/// visible rows and a merge silently hides a true fact (§21.2).
mod merging {
    use super::{Store, today};
    use async_trait::async_trait;
    use loki_core::memory::claim::Origin;
    use loki_core::memory::consolidate::{Candidate, ConsolidateError, Extractor, Unbounded};
    use loki_core::memory::index::Candidate as EntityCandidate;
    use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

    struct Says(&'static str, &'static str, Kind);

    #[async_trait]
    impl Extractor for Says {
        async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
            if !text.contains(self.1) {
                return Ok(vec![]);
            }
            Ok(vec![Candidate {
                surface: self.0.to_owned(),
                kind: self.2,
                heading: "note".to_owned(),
                attribute: "note".to_owned(),
                text: self.1.to_owned(),
                days_ago: None,
                valid_from: None,
                origin: Origin::Stated,
                tags: vec![],
                aliases: vec![],
                relation: None,
            }])
        }
    }

    /// Records what the matcher was shown, and always answers NEW.
    struct Watcher(std::sync::Mutex<Vec<EntityCandidate>>);

    #[async_trait]
    impl Matcher for Watcher {
        async fn decide(
            &self,
            _s: &str,
            _c: &str,
            candidates: &[EntityCandidate],
        ) -> Result<Decision, ResolveError> {
            self.0
                .lock()
                .expect("lock")
                .extend(candidates.iter().cloned());
            Ok(Decision::New)
        }
    }

    /// Probe case 3, and the worst result in the whole probe. Two Meeras merged because the
    /// matcher was asked whether two identical strings were the same person, which has no answer.
    /// It sees what is already believed about each now.
    #[tokio::test]
    async fn the_matcher_is_shown_what_it_needs_to_tell_two_people_apart() {
        let store = Store::open("two-meeras").await;
        let watcher = Watcher(std::sync::Mutex::new(Vec::new()));

        store
            .memory
            .record("user", "Meera is on the design team")
            .await
            .expect("record");
        store
            .memory
            .close(
                &Says("Meera", "Meera is on the design team", Kind::Person),
                &watcher,
                &Unbounded,
                today(),
            )
            .await
            .expect("close");

        store
            .memory
            .record("user", "the other Meera runs infra")
            .await
            .expect("record");
        store
            .memory
            .close(
                &Says("Meera", "the other Meera runs infra", Kind::Person),
                &watcher,
                &Unbounded,
                today(),
            )
            .await
            .expect("close");

        let seen = watcher.0.lock().expect("lock");
        let meera = seen
            .iter()
            .find(|c| c.path == "people/meera.md")
            .expect("the first Meera was offered as a candidate");
        assert!(
            meera.facts.iter().any(|f| f.contains("design team")),
            "the matcher has to see the facts, not just the name: {:?}",
            meera.facts
        );
        assert_eq!(meera.kind, "people");
    }

    /// Probe case 16. Apple the company and apple the fruit. Kind is evidence and not a filter, so
    /// the two are still offered to each other rather than being partitioned apart, which is what
    /// §12's fetched pages will need when one of them is written from outside.
    #[tokio::test]
    async fn a_different_kind_is_evidence_and_still_offered() {
        let store = Store::open("apple").await;
        let watcher = Watcher(std::sync::Mutex::new(Vec::new()));

        store
            .memory
            .record("user", "Apple announced a new laptop")
            .await
            .expect("record");
        store
            .memory
            .close(
                &Says("Apple", "Apple announced a new laptop", Kind::Project),
                &watcher,
                &Unbounded,
                today(),
            )
            .await
            .expect("close");

        store
            .memory
            .record("user", "I am allergic to apple")
            .await
            .expect("record");
        store
            .memory
            .close(
                &Says("apple", "I am allergic to apple", Kind::Preference),
                &watcher,
                &Unbounded,
                today(),
            )
            .await
            .expect("close");

        let seen = watcher.0.lock().expect("lock");
        assert!(
            seen.iter().any(|c| c.path == "projects/apple.md"),
            "a different kind must still reach the matcher: {:?}",
            seen.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
    }
}

/// Retrieval over what an entity is called and what points at it, not just what is written about
/// it (§10.1). Half the store used to be unsearchable: a nickname and an edge are the only place a
/// word like "father" appears, and lane 1 ranked claim text alone.
mod finding {
    use super::names::{Fact, store_with};

    /// The general shape. A card is reachable by the label of any live edge pointing at it,
    /// whatever the card happens to be called.
    #[tokio::test]
    async fn an_edge_label_finds_the_card_it_points_at() {
        let store = store_with(
            "by-edge",
            vec![Fact {
                trigger: "my father is Vaidyanathan",
                surface: "Vaidyanathan",
                attribute: "status",
                text: "Vaidyanathan is retired",
                aliases: &[],
                relation: Some(("father", "the user")),
            }],
        )
        .await;

        let hits = store.recall("what does my father do");
        assert!(
            hits.iter().any(|h| h.contains("retired")),
            "nothing in the claim says father: {hits:?}"
        );
    }

    /// And by any name it answers to, which is what makes a nickname worth storing.
    #[tokio::test]
    async fn a_nickname_finds_the_claims_under_the_formal_name() {
        let store = store_with(
            "by-alias",
            vec![
                Fact {
                    trigger: "called Ashok",
                    surface: "Vaidyanathan",
                    attribute: "preferred_name",
                    text: "Vaidyanathan prefers to be called Ashok",
                    aliases: &["Ashok"],
                    relation: None,
                },
                Fact {
                    trigger: "he is retired",
                    surface: "Vaidyanathan",
                    attribute: "status",
                    text: "Vaidyanathan is retired",
                    aliases: &[],
                    relation: None,
                },
            ],
        )
        .await;

        let hits = store.recall("is Ashok retired");
        assert!(
            hits.iter().any(|h| h.contains("retired")),
            "the retirement claim never says Ashok: {hits:?}"
        );
    }

    /// One-sided, and it has to stay that way. A form match adds candidates and never removes
    /// them, so an ordinary question is ranked exactly as it was before any of this existed.
    #[tokio::test]
    async fn a_word_that_names_nobody_changes_nothing() {
        let store = store_with(
            "unnamed",
            vec![Fact {
                trigger: "Meera is on the infra team",
                surface: "Meera",
                attribute: "team",
                text: "Meera is on the infra team",
                aliases: &[],
                relation: None,
            }],
        )
        .await;

        assert!(store.recall("who runs the bakery").is_empty());
        let hits = store.recall("Meera team");
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    /// A closed edge stops being a way of finding anything. Otherwise "my manager" would keep
    /// returning the person who used to be, which is the failure bi-temporal edges exist to stop.
    #[tokio::test]
    async fn a_closed_edge_no_longer_finds_its_target() {
        let store = store_with(
            "closed",
            vec![
                Fact {
                    trigger: "my manager is Zoe",
                    surface: "Zoe",
                    attribute: "job",
                    text: "Zoe took over the platform group",
                    aliases: &[],
                    relation: Some(("manager", "the user")),
                },
                Fact {
                    trigger: "Priya is my manager now",
                    surface: "Priya",
                    attribute: "job",
                    text: "Priya joined last spring",
                    aliases: &[],
                    relation: Some(("manager", "the user")),
                },
            ],
        )
        .await;

        let hits = store.recall("who is my manager");
        assert!(hits.iter().any(|h| h.contains("Priya")), "{hits:?}");
        assert!(
            !hits.iter().any(|h| h.contains("Zoe")),
            "the old manager must not answer the question: {hits:?}"
        );
    }
}

/// The repair. Everything else in §9.4 stops a split at write time; this is what happens when one
/// gets through anyway.
mod repair {
    use super::names::{Fact, store_with};
    use super::today;
    use loki_core::memory::bundle::OWNER;

    /// Scorecard case 19. A name used in the third person before the user says it is theirs.
    /// Two cards, and the store has to say so before anybody can do anything about it.
    async fn a_split_store() -> super::Store {
        store_with(
            "split",
            vec![
                Fact {
                    trigger: "Sabharish will be late",
                    surface: "Sabharish",
                    attribute: "status",
                    text: "Sabharish will be late",
                    aliases: &[],
                    relation: None,
                },
                Fact {
                    trigger: "my name is Sabharish",
                    surface: "the user",
                    attribute: "name",
                    text: "The user's name is Sabharish",
                    aliases: &["Sabharish"],
                    relation: None,
                },
            ],
        )
        .await
    }

    #[tokio::test]
    async fn two_cards_answering_to_one_name_are_reported() {
        let store = a_split_store().await;
        let knowledge = store.memory.knowledge(today()).await.expect("knowledge");

        let split = knowledge
            .duplicates
            .iter()
            .find(|d| d.form == "sabharish")
            .unwrap_or_else(|| panic!("no duplicate reported: {:?}", knowledge.duplicates));
        assert_eq!(split.paths.len(), 2, "{:?}", split.paths);
        assert!(split.paths.contains(&OWNER.to_owned()), "{:?}", split.paths);
    }

    #[tokio::test]
    async fn merging_folds_one_card_into_the_other() {
        let store = a_split_store().await;
        store
            .memory
            .merge("people/sabharish.md", OWNER, today())
            .await
            .expect("merge");

        let owner = store.card(OWNER).await;
        assert_eq!(owner.claims().count(), 2, "both facts, one card");
        assert!(owner.front.answers_to("Sabharish"));

        let knowledge = store.memory.knowledge(today()).await.expect("knowledge");
        assert!(
            knowledge.duplicates.is_empty(),
            "{:?}",
            knowledge.duplicates
        );
        assert_eq!(knowledge.entities.len(), 1, "{:?}", knowledge.entities);

        // Nothing is deleted. The old card is a tombstone that says where its contents went.
        let husk = store.card("people/sabharish.md").await;
        assert_eq!(husk.front.merged_into.as_deref(), Some(OWNER));
        assert!(husk.claims().next().is_none());
        assert!(
            husk.front.aliases.is_empty(),
            "its names moved with its claims: {:?}",
            husk.front.aliases
        );
    }

    /// A merge that leaves an edge pointing at the tombstone breaks every graph lookup that went
    /// through it, which is a quieter failure than the split it was fixing.
    #[tokio::test]
    async fn an_edge_pointing_at_the_merged_card_follows_it() {
        let store = store_with(
            "repoint",
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
                    trigger: "Lakshmi lives in Chennai",
                    surface: "Lakshmi",
                    attribute: "city",
                    text: "Lakshmi lives in Chennai",
                    aliases: &[],
                    relation: None,
                },
            ],
        )
        .await;

        store
            .memory
            .merge("people/the-user-s-sister.md", "people/lakshmi.md", today())
            .await
            .expect("merge");

        let owner = store.card(OWNER).await;
        assert_eq!(
            owner.front.related("sister"),
            Some("people/lakshmi.md"),
            "{:?}",
            owner.front.relations
        );
        assert!(
            store
                .recall("my sister")
                .iter()
                .any(|h| h.contains("Chennai")),
            "and the edge still finds her: {:?}",
            store.recall("my sister")
        );
    }

    /// Merging is the damaging direction (§21.2), so the two ways of doing it by accident are
    /// refused outright rather than being made to work.
    #[tokio::test]
    async fn a_merge_that_makes_no_sense_is_refused() {
        let store = a_split_store().await;
        assert!(store.memory.merge(OWNER, OWNER, today()).await.is_err());

        store
            .memory
            .merge("people/sabharish.md", OWNER, today())
            .await
            .expect("merge");
        assert!(
            store
                .memory
                .merge("people/sabharish.md", OWNER, today())
                .await
                .is_err(),
            "a tombstone cannot be merged a second time"
        );
    }

    /// Two cards about one person say some of the same things. A merge that copied them all would
    /// turn every duplicate into a pair of near-identical rows on the trust surface.
    #[tokio::test]
    async fn a_fact_both_cards_held_arrives_once() {
        // The shape an import produces: the same sentence filed under two ways of referring to
        // one person, because the two exports worded the subject differently.
        let store = store_with(
            "restated",
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
                    trigger: "the reviewer is on infra",
                    surface: "the design reviewer",
                    attribute: "team",
                    text: "Meera is on the infra team",
                    aliases: &[],
                    relation: None,
                },
            ],
        )
        .await;

        store
            .memory
            .merge("people/the-design-reviewer.md", "people/meera.md", today())
            .await
            .expect("merge");

        let card = store.card("people/meera.md").await;
        assert_eq!(card.claims().count(), 1, "{:?}", card.sections);
    }
}
