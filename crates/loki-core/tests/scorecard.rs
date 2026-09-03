//! The identity scorecard: twenty cases with written expected outcomes (2r-6).
//!
//! This began as `examples/probe.rs`, an instrument for finding out what the store did on cases
//! nobody had fixed. The answers are settled now, so it is a test: every case says what should
//! happen, and a case whose expected outcome is a known gap says so in `gap` rather than being
//! quietly deleted. A gap that closes fails here, which is the point.
//!
//! **Extraction emits what the instructions now ask a model for**: `the user` as the subject of the
//! owner's own facts, a relation line where a sentence connects two people, an alias line where it
//! gives another name. It still makes the mistakes a model makes, which is what the store has to
//! survive: a descriptor on one turn and a name on the next, a fact about a pair filed against one
//! of them, the same name meaning two things.
//!
//! Cases come from three places: what Sabharish hit in real use, the entity-resolution failure
//! taxonomy (split entities, wrongly merged entities, ambiguous names, historical name changes),
//! and the families `CLAUDE.md` asks for: the empty case, the enormous case, and the case where two
//! rules meet.

use std::sync::Mutex;

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::bundle::OWNER;
use loki_core::memory::claim::Origin;
use loki_core::memory::consolidate::{
    Candidate, ConsolidateError, Extractor, RelationTo, Unbounded,
};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Candidate as EntityCandidate, Index, Query};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

fn today() -> Date {
    date(2026, 9, 3)
}

/// A candidate as extraction would report it under the current instructions.
struct Fact {
    subject: &'static str,
    kind: Kind,
    attribute: &'static str,
    text: &'static str,
    /// Other names for the subject the same sentence gave.
    aliases: &'static [&'static str],
    /// `(label, whose)`. The subject is the `label` of `whose`.
    relation: Option<(&'static str, &'static str)>,
}

const fn person(subject: &'static str, attribute: &'static str, text: &'static str) -> Fact {
    Fact {
        subject,
        kind: Kind::Person,
        attribute,
        text,
        aliases: &[],
        relation: None,
    }
}

const fn thing(subject: &'static str, attribute: &'static str, text: &'static str) -> Fact {
    Fact {
        subject,
        kind: Kind::Project,
        attribute,
        text,
        aliases: &[],
        relation: None,
    }
}

const fn related(mut fact: Fact, label: &'static str, of: &'static str) -> Fact {
    fact.relation = Some((label, of));
    fact
}

const fn also(mut fact: Fact, aliases: &'static [&'static str]) -> Fact {
    fact.aliases = aliases;
    fact
}

struct Turn {
    said: &'static str,
    facts: Vec<Fact>,
}

/// What the store should look like once the case has run.
struct Expect {
    /// Cards carrying at least one claim. The seeded owner and assistant do not count until
    /// something has been learned about them.
    entities: usize,
    /// Questions that must come back with a line containing the fragment beside them.
    recalls: &'static [(&'static str, &'static str)],
    /// Questions that must come back with nothing.
    silent: &'static [&'static str],
    /// Current edges on the owner's card, as `(label, target)`.
    owner_edges: &'static [(&'static str, &'static str)],
    /// Set when the expected outcome above is the honest current answer rather than the right one.
    gap: Option<&'static str>,
}

const NOTHING: Expect = Expect {
    entities: 0,
    recalls: &[],
    silent: &[],
    owner_edges: &[],
    gap: None,
};

struct Case {
    name: &'static str,
    why: &'static str,
    turns: Vec<Turn>,
    expect: Expect,
}

struct Staged(Mutex<Vec<Turn>>);

#[async_trait]
impl Extractor for Staged {
    async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
        let turns = self.0.lock().expect("lock");
        Ok(turns
            .iter()
            .filter(|turn| text.contains(turn.said))
            .flat_map(|turn| turn.facts.iter())
            .map(|fact| Candidate {
                surface: fact.subject.to_owned(),
                kind: fact.kind,
                heading: fact.attribute.to_owned(),
                attribute: fact.attribute.to_owned(),
                text: fact.text.to_owned(),
                days_ago: None,
                valid_from: None,
                origin: Origin::Stated,
                tags: vec![],
                aliases: fact.aliases.iter().map(|a| (*a).to_owned()).collect(),
                relation: fact.relation.map(|(label, of)| RelationTo {
                    label: label.to_owned(),
                    of: of.to_owned(),
                }),
            })
            .collect())
    }
}

/// Says yes to the first candidate blocking offered.
///
/// Deliberately naive, and kept that way. Blocking has already narrowed to near matches, so this is
/// what a competent matcher does most of the time, and the cases it gets wrong are exactly the ones
/// worth seeing: they are where the store is relying on the model rather than on structure.
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

struct Run {
    memory: Memory,
    dir: std::path::PathBuf,
}

impl Run {
    async fn of(case: &Case) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-scorecard-{}-{}-{:?}",
            std::process::id(),
            case.name.replace(' ', "-"),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let memory = Memory::open(
            &dir,
            Index::in_memory().expect("index"),
            "scorecard",
            today(),
            TierScope::normal(Locality::Cloud),
        )
        .await
        .expect("open");

        // One close per turn, which is what a person opening and shutting the app all day makes.
        for turn in &case.turns {
            memory.record("user", turn.said).await.expect("record");
            let staged = Staged(Mutex::new(vec![Turn {
                said: turn.said,
                facts: turn.facts.iter().map(clone_fact).collect(),
            }]));
            memory
                .close(&staged, &FirstMatch, &Unbounded, today())
                .await
                .expect("close");
        }
        Self { memory, dir }
    }

    fn recall(&self, question: &str) -> Vec<String> {
        self.memory
            .index()
            .recall(&Query::prefetch(
                question,
                TierScope::normal(Locality::Cloud),
                today(),
                3,
            ))
            .expect("recall")
            .into_iter()
            .map(|hit| hit.text)
            .collect()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn clone_fact(f: &Fact) -> Fact {
    Fact {
        subject: f.subject,
        kind: f.kind,
        attribute: f.attribute,
        text: f.text,
        aliases: f.aliases,
        relation: f.relation,
    }
}

/// Checks one case and returns what went wrong, if anything.
async fn score(case: &Case) -> Vec<String> {
    let run = Run::of(case).await;
    let mut misses = Vec::new();

    let knowledge = run.memory.knowledge(today()).await.expect("knowledge");
    if knowledge.entities.len() != case.expect.entities {
        misses.push(format!(
            "expected {} entities, got {:?}",
            case.expect.entities,
            knowledge
                .entities
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
        ));
    }

    for (question, wanted) in case.expect.recalls {
        let hits = run.recall(question);
        if !hits.iter().any(|hit| hit.contains(wanted)) {
            misses.push(format!(
                "{question:?} should have found {wanted:?}: {hits:?}"
            ));
        }
    }
    for question in case.expect.silent {
        let hits = run.recall(question);
        if !hits.is_empty() {
            misses.push(format!("{question:?} should have found nothing: {hits:?}"));
        }
    }

    if !case.expect.owner_edges.is_empty() {
        let bundle = loki_core::memory::bundle::Bundle::open(&run.dir)
            .await
            .expect("bundle");
        let reader = bundle.reader().await;
        let owner = reader.load_concept(OWNER).expect("owner card");
        let live: Vec<(String, String)> = owner
            .front
            .relations
            .iter()
            .filter(|r| r.is_current())
            .map(|r| (r.label.clone(), r.to.clone()))
            .collect();
        for (label, to) in case.expect.owner_edges {
            if !live.iter().any(|(l, t)| l == label && t == to) {
                misses.push(format!("owner should have {label} -> {to}: {live:?}"));
            }
        }
        if live.len() != case.expect.owner_edges.len() {
            misses.push(format!(
                "owner should have exactly {} live edges: {live:?}",
                case.expect.owner_edges.len()
            ));
        }
    }
    misses
}

#[tokio::test]
async fn the_store_behaves_the_way_the_scorecard_says() {
    let cases = cases();
    let mut failed: Vec<String> = Vec::new();
    let mut gaps = 0;

    for case in &cases {
        let misses = score(case).await;
        if case.expect.gap.is_some() {
            gaps += 1;
        }
        if !misses.is_empty() {
            failed.push(format!(
                "\n{}\n  {}\n  {}",
                case.name,
                case.why,
                misses.join("\n  ")
            ));
        }
    }

    println!(
        "{} cases, {gaps} of them recording a known gap",
        cases.len()
    );
    for case in &cases {
        if let Some(gap) = case.expect.gap {
            println!("  GAP  {}: {gap}", case.name);
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[expect(clippy::too_many_lines, reason = "a list of cases, not logic")]
fn cases() -> Vec<Case> {
    vec![
        // Identity
        Case {
            name: "1 split entity: a descriptor then a name",
            why: "Sabharish's case. The same sister, referred to two ways across two turns.",
            turns: vec![
                Turn {
                    said: "my sister is studious",
                    facts: vec![related(
                        person(
                            "the user's sister",
                            "trait",
                            "The user's sister is studious",
                        ),
                        "sister",
                        "the user",
                    )],
                },
                Turn {
                    said: "Lakshmi finished her exams",
                    facts: vec![related(
                        person("Lakshmi", "status", "Lakshmi finished her exams"),
                        "sister",
                        "the user",
                    )],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("sister", "studious"), ("Lakshmi", "exams")],
                owner_edges: &[("sister", "people/the-user-s-sister.md")],
                ..NOTHING
            },
        },
        Case {
            name: "2 name change",
            why: "Meera marries and is now Meera Iyer. Name is not identity.",
            turns: vec![
                Turn {
                    said: "Meera is on the infra team",
                    facts: vec![person("Meera", "team", "Meera is on the infra team")],
                },
                Turn {
                    said: "Meera Iyer got promoted",
                    facts: vec![also(
                        person("Meera", "role", "Meera Iyer got promoted"),
                        &["Meera Iyer"],
                    )],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("Meera team", "infra"), ("Meera Iyer", "promoted")],
                ..NOTHING
            },
        },
        Case {
            name: "3 two people, one name",
            why: "The field's same-name ambiguity. §9.4 calls it the known failure.",
            turns: vec![
                Turn {
                    said: "Meera from design reviewed it",
                    facts: vec![person("Meera", "team", "Meera is on the design team")],
                },
                Turn {
                    said: "the other Meera runs infra",
                    facts: vec![person("Meera", "team", "Meera runs the infra team")],
                },
            ],
            expect: Expect {
                entities: 1,
                // Both survive, which is the improvement. `team` is many-valued, so the merge no
                // longer hides a true fact behind rule 4 the way it did in the probe.
                recalls: &[("Meera team", "design"), ("Meera design", "infra")],
                gap: Some(
                    "a naive matcher still merges them. It now sees each candidate's kind and \
                     facts, so telling them apart is a prompt away, but nothing structural stops it",
                ),
                ..NOTHING
            },
        },
        Case {
            name: "4 a descriptor that is not a relationship",
            why: "\"the client from Tuesday\". No edge to follow, so nothing can resolve it later.",
            turns: vec![
                Turn {
                    said: "the client from Tuesday wants a discount",
                    facts: vec![person(
                        "the client from Tuesday",
                        "want",
                        "The client from Tuesday wants a discount",
                    )],
                },
                Turn {
                    said: "Ravi agreed to the discount",
                    facts: vec![person("Ravi", "status", "Ravi agreed to the discount")],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("client discount", "discount")],
                gap: Some(
                    "correct, and it is the limit of the design: a description with no relation in \
                     it is not an edge, and inventing one out of a preposition would be worse",
                ),
                ..NOTHING
            },
        },
        Case {
            name: "5 a nickname",
            why: "Sabharish's father. One person, a formal name and a preferred one.",
            turns: vec![Turn {
                said: "my father Vaidyanathan prefers to be called Ashok",
                facts: vec![related(
                    also(
                        person(
                            "the user's father",
                            "preferred_name",
                            "Vaidyanathan prefers to be called Ashok",
                        ),
                        &["Vaidyanathan", "Ashok"],
                    ),
                    "father",
                    "the user",
                )],
            }],
            expect: Expect {
                entities: 1,
                recalls: &[("Ashok", "Ashok"), ("my father", "Ashok")],
                owner_edges: &[("father", "people/the-user-s-father.md")],
                ..NOTHING
            },
        },
        // Self and owner
        Case {
            name: "6 the owner and the assistant",
            why: "Neither was a distinguished thing. Both landed as ordinary people.",
            turns: vec![Turn {
                said: "my name is Sabharish and you are Loki",
                facts: vec![
                    also(
                        person("the user", "name", "The user's name is Sabharish"),
                        &["Sabharish"],
                    ),
                    person("Loki", "name", "The assistant's name is Loki"),
                ],
            }],
            expect: Expect {
                entities: 2,
                recalls: &[("Sabharish name", "Sabharish")],
                silent: &["who am I"],
                gap: Some(
                    "not an ontology gap and not fixable by indexing: who, am and I are all \
                     stopwords, so the question produces no query at all. The owner's card is in \
                     the working set, so the prompt has the answer even where recall does not. A \
                     question made entirely of function words is what the semantic fallback and \
                     lane 2 are for (§10.5)",
                ),
                ..NOTHING
            },
        },
        // Relationships
        Case {
            name: "7 two brothers",
            why: "One label, two targets. A single edge would lose a person.",
            turns: vec![
                Turn {
                    said: "my brother Arjun is a doctor",
                    facts: vec![related(
                        person("Arjun", "job", "Arjun is a doctor"),
                        "brother",
                        "the user",
                    )],
                },
                Turn {
                    said: "my brother Karthik is a lawyer",
                    facts: vec![related(
                        person("Karthik", "job", "Karthik is a lawyer"),
                        "brother",
                        "the user",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("Arjun", "doctor"), ("Karthik", "lawyer")],
                owner_edges: &[
                    ("brother", "people/arjun.md"),
                    ("brother", "people/karthik.md"),
                ],
                ..NOTHING
            },
        },
        Case {
            name: "8 a relationship two hops away",
            why: "My father's brother. Reachable only if relationships are edges.",
            turns: vec![
                Turn {
                    said: "my father is Vaidyanathan",
                    facts: vec![related(
                        person("Vaidyanathan", "status", "Vaidyanathan is retired"),
                        "father",
                        "the user",
                    )],
                },
                Turn {
                    said: "Vaidyanathan's brother is Ramesh",
                    facts: vec![related(
                        person("Ramesh", "job", "Ramesh is an engineer"),
                        "brother",
                        "Vaidyanathan",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("Ramesh", "engineer")],
                owner_edges: &[("father", "people/vaidyanathan.md")],
                ..NOTHING
            },
        },
        Case {
            name: "9 a relationship that ends",
            why: "A manager changes. The old edge closes the way a claim's window does.",
            turns: vec![
                Turn {
                    said: "my manager is Zoe",
                    facts: vec![related(
                        person("Zoe", "job", "Zoe manages the user"),
                        "manager",
                        "the user",
                    )],
                },
                Turn {
                    said: "Priya is my manager now",
                    facts: vec![related(
                        person("Priya", "job", "Priya manages the user"),
                        "manager",
                        "the user",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("manager", "manages")],
                owner_edges: &[("manager", "people/priya.md")],
                ..NOTHING
            },
        },
        // Cardinality
        Case {
            name: "10 add on top, do not replace",
            why: "Sabharish's case. A certificate on top of a degree, both true.",
            turns: vec![
                Turn {
                    said: "I have a degree in computer science",
                    facts: vec![person(
                        "the user",
                        "education",
                        "The user has a degree in computer science",
                    )],
                },
                Turn {
                    said: "I am now a certified machine learning engineer",
                    facts: vec![person(
                        "the user",
                        "education",
                        "The user is a certified machine learning engineer",
                    )],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("degree", "degree"), ("certified", "certified")],
                ..NOTHING
            },
        },
        Case {
            name: "11 replace, do not add",
            why: "The same shape as case 10, and the opposite right answer.",
            turns: vec![
                Turn {
                    said: "I live in Chennai",
                    facts: vec![person("the user", "city", "The user lives in Chennai")],
                },
                Turn {
                    said: "I have moved to Bangalore",
                    facts: vec![person("the user", "city", "The user lives in Bangalore")],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("where do I live", "Bangalore")],
                silent: &["Chennai"],
                ..NOTHING
            },
        },
        Case {
            name: "12 cardinality that is not fixed for everyone",
            why: "One employer for most people, two for a consultant. Both true.",
            turns: vec![
                Turn {
                    said: "I consult for Acme",
                    facts: vec![person("the user", "employer", "The user consults for Acme")],
                },
                Turn {
                    said: "I also consult for Globex",
                    facts: vec![person(
                        "the user",
                        "employer",
                        "The user consults for Globex",
                    )],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("Acme", "Acme"), ("Globex", "Globex")],
                ..NOTHING
            },
        },
        // Shape
        Case {
            name: "13 an entity that is not a person or a project",
            why: "A medication and a car. Neither fits the three folders.",
            turns: vec![
                Turn {
                    said: "I take metformin twice a day",
                    facts: vec![thing(
                        "metformin",
                        "dose",
                        "The user takes metformin twice a day",
                    )],
                },
                Turn {
                    said: "my car is a Swift",
                    facts: vec![thing(
                        "the user's car",
                        "model",
                        "The user's car is a Swift",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("metformin", "metformin"), ("car", "Swift")],
                gap: Some(
                    "both land under projects/. §9.3 has three folders and this is a fourth kind \
                     of thing. Storing it works; the folder is a lie",
                ),
                ..NOTHING
            },
        },
        Case {
            name: "14 a fact about a pair",
            why: "The fact is about the relationship, not about either person.",
            turns: vec![Turn {
                said: "Meera and I disagree about the release date",
                facts: vec![person(
                    "Meera",
                    "disagreement",
                    "Meera and the user disagree about the release date",
                )],
            }],
            expect: Expect {
                entities: 1,
                recalls: &[("disagree release date", "disagree")],
                gap: Some(
                    "filed against one of the pair. A claim belongs to one entity, and a fact on \
                     an edge is a real extension nothing yet needs",
                ),
                ..NOTHING
            },
        },
        Case {
            name: "15 an ambiguous name",
            why: "Apple the company and apple the fruit. The field's standard example.",
            turns: vec![
                Turn {
                    said: "Apple announced a new laptop",
                    facts: vec![thing("Apple", "news", "Apple announced a new laptop")],
                },
                Turn {
                    said: "I am allergic to apple",
                    facts: vec![Fact {
                        kind: Kind::Preference,
                        ..person("apple", "allergy", "The user is allergic to apple")
                    }],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("apple", "apple")],
                gap: Some(
                    "still merged by a naive matcher. Kind is evidence now and reaches the prompt, \
                     which is what §12's fetched pages will need",
                ),
                ..NOTHING
            },
        },
        Case {
            name: "16 a fact about the owner in the third person",
            why: "The same person, two voices. A store keyed on the string sees two subjects.",
            turns: vec![
                Turn {
                    said: "I am a computer science graduate",
                    facts: vec![person(
                        "the user",
                        "education",
                        "The user is a computer science graduate",
                    )],
                },
                Turn {
                    said: "the user studied computer science",
                    facts: vec![person(
                        "the user",
                        "education",
                        "The user studied computer science",
                    )],
                },
            ],
            expect: Expect {
                entities: 1,
                recalls: &[("computer science", "computer science")],
                ..NOTHING
            },
        },
        // Three cases from families nothing above covers.
        Case {
            name: "17 the empty case: a subject with nothing in it",
            why: "A model emitting punctuation as a name, and an edge to somebody who does not \
                  exist. Neither should cost anything.",
            turns: vec![
                Turn {
                    said: "??? said yes",
                    facts: vec![person("???", "status", "They said yes")],
                },
                Turn {
                    said: "her manager approved it",
                    facts: vec![related(
                        person("the approver", "status", "The approver approved it"),
                        "manager",
                        "somebody nobody has mentioned",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("approved", "approved")],
                // No edge, because the other end does not exist. A dangling pointer would be worse
                // than a missing one, and creating the card to hold it would invent a person.
                ..NOTHING
            },
        },
        Case {
            name: "18 where two rules meet: one label at two levels",
            why: "The owner's manager and that manager's manager. A single-valued relation must \
                  close its own source's edge and nobody else's.",
            turns: vec![
                Turn {
                    said: "my manager is Zoe",
                    facts: vec![related(
                        person("Zoe", "job", "Zoe manages the user"),
                        "manager",
                        "the user",
                    )],
                },
                Turn {
                    said: "Zoe's manager is Priya",
                    facts: vec![related(
                        person("Priya", "job", "Priya manages Zoe"),
                        "manager",
                        "Zoe",
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("Zoe", "manages"), ("Priya", "manages")],
                owner_edges: &[("manager", "people/zoe.md")],
                ..NOTHING
            },
        },
        Case {
            name: "19 the name before the person",
            why: "Named in the third person before ever saying it is their name. The card exists \
                  before anything connects it to the owner.",
            turns: vec![
                Turn {
                    said: "Sabharish will be late",
                    facts: vec![person("Sabharish", "status", "Sabharish will be late")],
                },
                Turn {
                    said: "my name is Sabharish",
                    facts: vec![also(
                        person("the user", "name", "The user's name is Sabharish"),
                        &["Sabharish"],
                    )],
                },
            ],
            expect: Expect {
                entities: 2,
                recalls: &[("Sabharish", "Sabharish")],
                gap: Some(
                    "two cards answer to Sabharish: the one the third-person mention created and \
                     the owner, which adopted the name afterwards. Merging two existing cards is \
                     not something 2r built, and it is the next thing this area needs",
                ),
                ..NOTHING
            },
        },
    ]
}
