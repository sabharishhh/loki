//! An instrument, not a test. Runs the real memory pipeline against cases nobody has fixed yet and
//! prints what the store actually becomes.
//!
//! `cargo run -p loki-core --example probe`
//!
//! **Why an example rather than a test.** A test asserts what should happen. This is for finding
//! out what does happen, on cases where the right answer is still being argued about. Making these
//! assertions now would freeze whatever the code currently does as correct, which is the opposite
//! of the point.
//!
//! **The extractor here is deliberately fallible.** It emits what a good model plausibly emits,
//! including the plausible mistakes: a descriptor on one turn and a name on the next, an attribute
//! spelled two ways, a relationship written as prose. An extractor that never slipped would prove
//! nothing, because the store's job is to survive one that does.
//!
//! Cases come from two places: what Sabharish hit in real use, and the failure taxonomy the field
//! already has for entity resolution, which names split entities, wrongly merged entities,
//! ambiguous names, and historical name changes as the four that matter.

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

/// One thing said, and what a plausible extractor would take from it.
struct Turn {
    said: &'static str,
    facts: Vec<Fact>,
}

/// A candidate as the extractor would report it: a subject as it was referred to, a property, and
/// a sentence.
struct Fact {
    subject: &'static str,
    kind: Kind,
    attribute: &'static str,
    text: &'static str,
    origin: Origin,
}

fn person(subject: &'static str, attribute: &'static str, text: &'static str) -> Fact {
    Fact {
        subject,
        kind: Kind::Person,
        attribute,
        text,
        origin: Origin::Stated,
    }
}

fn thing(subject: &'static str, attribute: &'static str, text: &'static str) -> Fact {
    Fact {
        subject,
        kind: Kind::Project,
        attribute,
        text,
        origin: Origin::Stated,
    }
}

/// One probe: a name, some turns, and the questions worth asking the store afterwards.
struct Case {
    name: &'static str,
    why: &'static str,
    turns: Vec<Turn>,
    ask: Vec<&'static str>,
}

/// Replays whatever the case staged for the turn it is handed.
struct Staged {
    turns: Mutex<Vec<Turn>>,
}

#[async_trait]
impl Extractor for Staged {
    async fn extract(&self, _e: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError> {
        let mut out = Vec::new();
        let turns = self.turns.lock().expect("lock");
        for turn in turns.iter() {
            if !text.contains(turn.said) {
                continue;
            }
            for fact in &turn.facts {
                out.push(Candidate {
                    surface: fact.subject.to_owned(),
                    kind: fact.kind,
                    heading: fact.attribute.to_owned(),
                    attribute: fact.attribute.to_owned(),
                    text: fact.text.to_owned(),
                    days_ago: None,
                    valid_from: None,
                    origin: fact.origin,
                    tags: vec![],
                    aliases: vec![],
                    relation: None,
                });
            }
        }
        Ok(out)
    }
}

/// Blocking's own answer, then the model's. This one says yes to the first candidate, which is what
/// a competent matcher does when blocking has already narrowed to near-matches.
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

fn today() -> Date {
    date(2026, 9, 3)
}

async fn run(case: &Case) {
    println!("\n{}", "─".repeat(96));
    println!("CASE  {}", case.name);
    println!("WHY   {}", case.why);
    println!("{}", "─".repeat(96));

    let dir = std::env::temp_dir().join(format!(
        "loki-probe-{}-{}",
        std::process::id(),
        case.name.replace(' ', "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let memory = Memory::open(
        &dir,
        Index::in_memory().expect("index"),
        "probe",
        today(),
        TierScope::normal(Locality::Cloud),
    )
    .await
    .expect("open");

    // One close per turn, which is what a person who opens and shuts the app all day produces.
    for turn in &case.turns {
        println!("  said  {}", turn.said);
        memory.record("user", turn.said).await.expect("record");
        let staged = Staged {
            turns: Mutex::new(vec![Turn {
                said: turn.said,
                facts: turn.facts.iter().map(clone_fact).collect(),
            }]),
        };
        memory
            .close(&staged, &FirstMatch, &Unbounded, today())
            .await
            .expect("close");
    }

    let knowledge = memory.knowledge(today()).await.expect("knowledge");
    println!("\n  STORE  {} entities", knowledge.entities.len());
    for entity in &knowledge.entities {
        let flag = if entity.in_use { "" } else { "  [not in use]" };
        println!("    {} ({}){}", entity.name, entity.path, flag);
        for fact in &entity.facts {
            println!("      · [{}] {}", fact.attribute, fact.text);
            for other in &fact.also_said {
                println!("        ~ also said: {}", other.text);
            }
        }
    }

    if !case.ask.is_empty() {
        println!("\n  RECALL");
        for question in &case.ask {
            let hits = memory
                .index()
                .recall(&Query::prefetch(
                    question,
                    TierScope::normal(Locality::Cloud),
                    today(),
                    3,
                ))
                .expect("recall");
            let answers: Vec<String> = hits.into_iter().map(|h| h.text).collect();
            println!(
                "    \"{question}\" -> {}",
                if answers.is_empty() {
                    "(nothing)".to_owned()
                } else {
                    answers.join(" | ")
                }
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn clone_fact(f: &Fact) -> Fact {
    Fact {
        subject: f.subject,
        kind: f.kind,
        attribute: f.attribute,
        text: f.text,
        origin: f.origin,
    }
}

#[tokio::main]
async fn main() {
    let cases = cases();
    println!("Loki memory probe. {} cases.", cases.len());
    for case in &cases {
        run(case).await;
    }
    println!("\n{}\nDone.", "─".repeat(96));
}

#[expect(clippy::too_many_lines, reason = "a list of cases, not logic")]
fn cases() -> Vec<Case> {
    vec![
        // ── Identity ────────────────────────────────────────────────────────────────────────
        Case {
            name: "1 split entity: a descriptor then a name",
            why: "Sabharish's case. The same sister, referred to two ways across two turns.",
            turns: vec![
                Turn {
                    said: "my sister is studious",
                    facts: vec![person(
                        "the user's sister",
                        "trait",
                        "The user's sister is studious",
                    )],
                },
                Turn {
                    said: "Lakshmi finished her exams",
                    facts: vec![person("Lakshmi", "status", "Lakshmi finished her exams")],
                },
            ],
            ask: vec!["sister", "Lakshmi"],
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
                    facts: vec![person("Meera Iyer", "role", "Meera Iyer got promoted")],
                },
            ],
            ask: vec!["Meera team", "Meera Iyer"],
        },
        Case {
            name: "3 two people, one name",
            why: "The field calls this the same-name ambiguity. §9.4 calls it the known failure.",
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
            ask: vec!["Meera team"],
        },
        Case {
            name: "4 a descriptor that is not a relationship",
            why: "\"the client from Tuesday\". No edge to follow, so no way to resolve it later.",
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
            ask: vec!["client discount"],
        },
        Case {
            name: "5 a nickname",
            why: "Sabharish's father. One person, a formal name and a preferred one.",
            turns: vec![Turn {
                said: "my father Vaidyanathan prefers to be called Ashok",
                facts: vec![
                    person(
                        "Vaidyanathan",
                        "preferred_name",
                        "Vaidyanathan prefers to be called Ashok",
                    ),
                    person(
                        "the user's father",
                        "name",
                        "The user's father's name is Vaidyanathan",
                    ),
                ],
            }],
            ask: vec!["Ashok", "father"],
        },
        // ── Self and owner ──────────────────────────────────────────────────────────────────
        Case {
            name: "6 the owner and the assistant",
            why: "Neither is a distinguished thing today. Both land as ordinary people.",
            turns: vec![Turn {
                said: "my name is Sabharish and you are Loki",
                facts: vec![
                    person("Sabharish", "name", "The user's name is Sabharish"),
                    person("Loki", "name", "The assistant's name is Loki"),
                ],
            }],
            ask: vec!["who am I", "what are you called"],
        },
        // ── Relationships ───────────────────────────────────────────────────────────────────
        Case {
            name: "7 two brothers",
            why: "One relationship label, two targets. A single edge would be wrong.",
            turns: vec![
                Turn {
                    said: "my brother Arjun is a doctor",
                    facts: vec![person("Arjun", "relation", "Arjun is the user's brother")],
                },
                Turn {
                    said: "my brother Karthik is a lawyer",
                    facts: vec![person(
                        "Karthik",
                        "relation",
                        "Karthik is the user's brother",
                    )],
                },
            ],
            ask: vec!["brother"],
        },
        Case {
            name: "8 a relationship two hops away",
            why: "My father's brother. Reachable only if relationships are traversable.",
            turns: vec![
                Turn {
                    said: "my father is Vaidyanathan",
                    facts: vec![person(
                        "Vaidyanathan",
                        "relation",
                        "Vaidyanathan is the user's father",
                    )],
                },
                Turn {
                    said: "Vaidyanathan's brother is Ramesh",
                    facts: vec![person(
                        "Ramesh",
                        "relation",
                        "Ramesh is Vaidyanathan's brother",
                    )],
                },
            ],
            ask: vec!["uncle", "father brother"],
        },
        Case {
            name: "9 a relationship that ends",
            why: "A manager changes. The old edge has to close the way a claim does.",
            turns: vec![
                Turn {
                    said: "my manager is Zoe",
                    facts: vec![person("Zoe", "relation", "Zoe is the user's manager")],
                },
                Turn {
                    said: "Priya is my manager now",
                    facts: vec![person("Priya", "relation", "Priya is the user's manager")],
                },
            ],
            ask: vec!["manager"],
        },
        // ── Cardinality ─────────────────────────────────────────────────────────────────────
        Case {
            name: "10 add on top, do not replace",
            why: "Sabharish's case. A certificate on top of a degree, both true.",
            turns: vec![
                Turn {
                    said: "I have a degree in computer science",
                    facts: vec![person(
                        "Sabharish",
                        "education",
                        "Sabharish has a degree in computer science",
                    )],
                },
                Turn {
                    said: "I am now a certified machine learning engineer",
                    facts: vec![person(
                        "Sabharish",
                        "education",
                        "Sabharish is a certified machine learning engineer",
                    )],
                },
            ],
            ask: vec!["degree", "certified"],
        },
        Case {
            name: "11 replace, do not add",
            why: "The same shape as case 10, and the opposite right answer.",
            turns: vec![
                Turn {
                    said: "I live in Chennai",
                    facts: vec![person("Sabharish", "city", "Sabharish lives in Chennai")],
                },
                Turn {
                    said: "I have moved to Bangalore",
                    facts: vec![person("Sabharish", "city", "Sabharish lives in Bangalore")],
                },
            ],
            ask: vec!["Sabharish lives"],
        },
        Case {
            name: "12 cardinality that is not fixed for everyone",
            why: "One employer for most people, two for a consultant. Same attribute, both true.",
            turns: vec![
                Turn {
                    said: "I consult for Acme",
                    facts: vec![person(
                        "Sabharish",
                        "employer",
                        "Sabharish consults for Acme",
                    )],
                },
                Turn {
                    said: "I also consult for Globex",
                    facts: vec![person(
                        "Sabharish",
                        "employer",
                        "Sabharish consults for Globex",
                    )],
                },
            ],
            ask: vec!["consults"],
        },
        // ── Shape ───────────────────────────────────────────────────────────────────────────
        Case {
            name: "13 an entity that is not a person or a project",
            why: "A medication and a car. Neither fits the three folders.",
            turns: vec![
                Turn {
                    said: "I take metformin twice a day",
                    facts: vec![thing(
                        "metformin",
                        "dose",
                        "Sabharish takes metformin twice a day",
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
            ask: vec!["metformin", "car"],
        },
        Case {
            name: "14 a fact about a pair",
            why: "The fact is about the relationship, not about either person.",
            turns: vec![Turn {
                said: "Meera and I disagree about the release date",
                facts: vec![person(
                    "Meera",
                    "disagreement",
                    "Meera and Sabharish disagree about the release date",
                )],
            }],
            ask: vec!["disagree release date"],
        },
        Case {
            name: "15 an attribute spelled two ways",
            why: "relation against relationship. The plural fold does not catch this.",
            turns: vec![
                Turn {
                    said: "Lakshmi is my sister",
                    facts: vec![person(
                        "Lakshmi",
                        "relation",
                        "Lakshmi is the user's sister",
                    )],
                },
                Turn {
                    said: "Lakshmi, my sister, is studious",
                    facts: vec![person(
                        "Lakshmi",
                        "relationship",
                        "Lakshmi is the user's sister",
                    )],
                },
            ],
            ask: vec!["Lakshmi sister"],
        },
        Case {
            name: "16 an ambiguous name",
            why: "Apple the company and apple the fruit. The field's standard example.",
            turns: vec![
                Turn {
                    said: "Apple announced a new laptop",
                    facts: vec![thing("Apple", "news", "Apple announced a new laptop")],
                },
                Turn {
                    said: "I am allergic to apple",
                    facts: vec![thing("apple", "allergy", "Sabharish is allergic to apple")],
                },
            ],
            ask: vec!["apple"],
        },
        Case {
            name: "17 a fact stated about the owner in the third person",
            why: "The same fact, two voices. A store keyed on the string sees two subjects.",
            turns: vec![
                Turn {
                    said: "I am a computer science graduate",
                    facts: vec![person(
                        "Sabharish",
                        "education",
                        "Sabharish is a computer science graduate",
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
            ask: vec!["computer science"],
        },
    ]
}
