//! Consolidation over a real bundle, a real index and real git.
//!
//! Extraction and matching are scripted. Both are model calls in production, and a test whose
//! setup is a model call measures two things and fails for the wrong reasons. What is under test
//! is the pipeline: ordering, precedence, promotion, archival, resumption.

use std::sync::Mutex;

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::bundle::Bundle;
use loki_core::memory::claim::Origin;
use loki_core::memory::concept::Status;
use loki_core::memory::consolidate::{
    Budget, Candidate, ConsolidateError, Episode, Extractor, Report, Unbounded, run,
};
use loki_core::memory::gate::TierScope;
use loki_core::memory::index::{Candidate as EntityCandidate, Index, Query, Visibility};
use loki_core::memory::reconcile::{Precedence, Reference};
use loki_core::memory::resolve::{Decision, Kind, Matcher, ResolveError};

/// Hands back whatever the test staged for each episode path, and records the order it was asked.
struct Scripted {
    per_episode: Vec<(String, Vec<Candidate>)>,
    seen: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(per_episode: Vec<(String, Vec<Candidate>)>) -> Self {
        Self {
            per_episode,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn order(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Extractor for Scripted {
    async fn extract(
        &self,
        episode: &str,
        _text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        self.seen.lock().expect("lock").push(episode.to_string());
        Ok(self
            .per_episode
            .iter()
            .find(|(path, _)| path == episode)
            .map(|(_, out)| out.clone())
            .unwrap_or_default())
    }
}

/// Always says the first candidate is the entity, or that it is new when there are none.
struct FirstMatch;

#[async_trait]
impl Matcher for FirstMatch {
    async fn decide(
        &self,
        _surface: &str,
        _claim: &str,
        candidates: &[EntityCandidate],
    ) -> Result<Decision, ResolveError> {
        Ok(if candidates.is_empty() {
            Decision::New
        } else {
            Decision::Existing(0)
        })
    }
}

/// Stops after a fixed number of episodes, standing in for §20.2's ceiling.
struct StopsAfter {
    limit: usize,
    used: Mutex<usize>,
}

impl StopsAfter {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
        }
    }
}

impl Budget for StopsAfter {
    fn may_continue(&self) -> bool {
        let mut used = self.used.lock().expect("lock");
        if *used >= self.limit {
            return false;
        }
        *used += 1;
        true
    }
}

fn candidate(surface: &str, text: &str, valid_from: Date, origin: Origin) -> Candidate {
    about("fact", surface, text, valid_from, origin)
}

/// A candidate that says which property of the entity it sets.
fn about(
    attribute: &str,
    surface: &str,
    text: &str,
    valid_from: Date,
    origin: Origin,
) -> Candidate {
    Candidate {
        surface: surface.to_string(),
        kind: Kind::Person,
        heading: attribute.to_string(),
        attribute: attribute.to_string(),
        text: text.to_string(),
        days_ago: None,
        valid_from: Some(valid_from),
        origin,
        tags: vec![],
    }
}

struct Store {
    bundle: Bundle,
    index: Index,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str, episodes: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-consolidate-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).await.expect("open");
        {
            let writer = bundle.writer().await;
            for path in episodes {
                writer.write(path, "a session happened").expect("episode");
            }
            writer.commit("episodes").expect("commit");
        }
        let index = Index::in_memory().expect("index");
        {
            let reader = bundle.reader().await;
            index.sync(&reader).expect("sync");
        }
        Self { bundle, index, dir }
    }

    async fn concept_at(&self, path: &str) -> loki_core::memory::concept::RawConcept {
        let reader = self.bundle.reader().await;
        reader.load_concept(path).expect("load concept")
    }

    async fn go(&self, episodes: &[Episode], extractor: &Scripted, budget: &dyn Budget) -> Report {
        run(
            episodes,
            &self.bundle,
            &self.index,
            extractor,
            &FirstMatch,
            budget,
            date(2026, 8, 29),
        )
        .await
        .expect("consolidate")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn episode(path: &str, on: Date) -> Episode {
    Episode {
        path: path.to_string(),
        reference: Reference::Message(on),
    }
}

/// §9.5's worked example, driven through the whole pipeline rather than asserted on a claim.
#[tokio::test]
async fn a_correction_supersedes_and_records_how_long_it_was_wrong() {
    let store = Store::new("correction", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![candidate(
                "Sabharish",
                "on the design team",
                date(2026, 3, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Sabharish",
                "on the infra team",
                date(2026, 7, 15),
                Origin::Stated,
            )],
        ),
    ]);

    let report = store
        .go(
            &[
                episode("episodes/a.md", date(2026, 3, 1)),
                episode("episodes/b.md", date(2026, 8, 29)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    assert_eq!(
        extractor.order(),
        ["episodes/a.md", "episodes/b.md"],
        "oldest first is not a detail"
    );
    assert_eq!(report.decisions.len(), 1, "{:?}", report.decisions);
    assert_eq!(report.decisions[0].outcome, Precedence::Replace);

    let concept = store.concept_at("people/sabharish.md").await;
    let old = concept
        .claims()
        .find(|c| c.text == "on the design team")
        .expect("held claim");
    assert!(!old.validity.is_believed());
    assert_eq!(old.replaced_by.as_deref(), Some("on the infra team"));
    assert_eq!(old.validity.wrong_for_days(), Some(45));
}

/// Rule 4. Two stated claims of one vintage, so neither is used and a person decides.
#[tokio::test]
async fn a_conflict_with_no_clear_winner_is_surfaced_not_guessed() {
    let store = Store::new("surface", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![candidate(
                "Meera",
                "at Acme",
                date(2026, 3, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Meera",
                "at Globex",
                date(2026, 3, 1),
                Origin::Stated,
            )],
        ),
    ]);

    let report = store
        .go(
            &[
                episode("episodes/a.md", date(2026, 3, 1)),
                episode("episodes/b.md", date(2026, 3, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    assert_eq!(report.decisions[0].outcome, Precedence::Surface);
    assert!(!report.surfaced.is_empty(), "the tie has to reach the user");
    let concept = store.concept_at("people/meera.md").await;
    assert_eq!(
        concept.front.status,
        Status::Draft,
        "an unresolved conflict must not stay prompt-eligible"
    );
}

/// §11.5. The ceiling stops the run, and what is left comes back so it can be picked up again.
#[tokio::test]
async fn a_run_that_hits_its_ceiling_reports_what_is_left() {
    let store = Store::new(
        "budget",
        &["episodes/a.md", "episodes/b.md", "episodes/c.md"],
    )
    .await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![candidate(
            "Dan",
            "likes tea",
            date(2026, 1, 1),
            Origin::Stated,
        )],
    )]);
    let episodes = [
        episode("episodes/a.md", date(2026, 1, 1)),
        episode("episodes/b.md", date(2026, 2, 1)),
        episode("episodes/c.md", date(2026, 3, 1)),
    ];

    let report = store.go(&episodes, &extractor, &StopsAfter::new(1)).await;

    assert_eq!(report.episodes, ["episodes/a.md"]);
    assert_eq!(report.remaining, ["episodes/b.md", "episodes/c.md"]);
    assert_eq!(extractor.order().len(), 1, "the budget must stop the work");
}

/// Resuming from a report finishes the job without redoing the first part.
#[tokio::test]
async fn a_paused_run_resumes_where_it_stopped() {
    let store = Store::new("resume", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![candidate(
                "Dan",
                "likes tea",
                date(2026, 1, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Priya",
                "likes coffee",
                date(2026, 2, 1),
                Origin::Stated,
            )],
        ),
    ]);
    let episodes = [
        episode("episodes/a.md", date(2026, 1, 1)),
        episode("episodes/b.md", date(2026, 2, 1)),
    ];

    let first = store.go(&episodes, &extractor, &StopsAfter::new(1)).await;
    assert_eq!(first.remaining, ["episodes/b.md"]);

    let rest: Vec<Episode> = episodes
        .iter()
        .filter(|e| first.remaining.contains(&e.path))
        .cloned()
        .collect();
    let second = store.go(&rest, &extractor, &Unbounded).await;

    assert_eq!(second.episodes, ["episodes/b.md"]);
    assert_eq!(extractor.order(), ["episodes/a.md", "episodes/b.md"]);
    let reader = store.bundle.reader().await;
    assert!(reader.load_concept("people/dan.md").is_ok());
    assert!(reader.load_concept("people/priya.md").is_ok());
}

/// §9.6. The same relative expression from a year-old export must not land on today.
#[tokio::test]
async fn relative_time_resolves_against_the_episode_not_today() {
    let store = Store::new("reference", &["episodes/old.md"]).await;
    let mut relative = candidate(
        "Dan",
        "started a new job",
        date(2026, 1, 1),
        Origin::Inferred,
    );
    relative.valid_from = None;
    relative.days_ago = Some(14);
    let extractor = Scripted::new(vec![("episodes/old.md".to_string(), vec![relative])]);

    store
        .go(
            &[episode("episodes/old.md", date(2025, 6, 20))],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/dan.md").await;
    let claim = concept.claims().next().expect("claim");
    assert_eq!(
        claim.validity.valid_from,
        Some(date(2025, 6, 6)),
        "two weeks before the message, not before today"
    );
}

/// An inferred first mention stays draft, so a guess does not become a fact about you.
#[tokio::test]
async fn an_inferred_first_mention_stays_draft_and_a_second_promotes() {
    let store = Store::new("promote", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![candidate(
                "Dan",
                "prefers short replies",
                date(2026, 1, 1),
                Origin::Inferred,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Dan",
                "prefers short replies",
                date(2026, 1, 1),
                Origin::Inferred,
            )],
        ),
    ]);

    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;
    assert_eq!(
        store.concept_at("people/dan.md").await.front.status,
        Status::Draft,
        "an inferred first mention must not be prompt-eligible"
    );

    store
        .go(
            &[episode("episodes/b.md", date(2026, 2, 1))],
            &extractor,
            &Unbounded,
        )
        .await;
    assert_eq!(
        store.concept_at("people/dan.md").await.front.status,
        Status::Stable,
        "a second occurrence earns it"
    );
}

#[tokio::test]
async fn a_run_that_learned_nothing_says_nothing() {
    let store = Store::new("quiet", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![("episodes/a.md".to_string(), vec![])]);

    let report = store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    assert_eq!(report.extracted, 0);
    assert!(
        !report.is_newsworthy(),
        "a card that says it learned nothing teaches people to ignore the card"
    );
}

/// The run has to leave a commit behind, or the timeline and revert have nothing to stand on.
#[tokio::test]
async fn a_run_commits_and_the_index_sees_the_result() {
    let store = Store::new("commit", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![candidate(
            "Priya",
            "runs the platform team",
            date(2026, 1, 1),
            Origin::Stated,
        )],
    )]);

    let before = {
        let reader = store.bundle.reader().await;
        reader.commit_count().expect("count")
    };
    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;
    let after = {
        let reader = store.bundle.reader().await;
        reader.commit_count().expect("count")
    };

    assert!(after > before, "consolidation must commit");
    assert!(
        !store
            .index
            .candidates("Priya", &[], 5)
            .expect("candidates")
            .is_empty(),
        "the new entity has to be findable straight away"
    );
}

/// What the user says is usable straight away. The whole product promise depends on it.
#[tokio::test]
async fn a_stated_first_mention_is_usable_at_once() {
    let store = Store::new("stated-now", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![candidate(
            "Sabharish",
            "is the user's name",
            date(2026, 1, 1),
            Origin::Stated,
        )],
    )]);

    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    assert_eq!(
        store.concept_at("people/sabharish.md").await.front.status,
        Status::Stable,
        "a fact the user stated has to be usable without being said twice"
    );
}

/// Closing a session twice, or resuming a paused import, must not duplicate what it already knows.
#[tokio::test]
async fn re_consolidating_an_episode_does_not_duplicate_its_claims() {
    let store = Store::new("idempotent", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![candidate(
            "Sabharish",
            "is a computer science graduate",
            date(2026, 1, 1),
            Origin::Stated,
        )],
    )]);
    let episodes = [episode("episodes/a.md", date(2026, 1, 1))];

    store.go(&episodes, &extractor, &Unbounded).await;
    store.go(&episodes, &extractor, &Unbounded).await;
    store.go(&episodes, &extractor, &Unbounded).await;

    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        concept.claims().count(),
        1,
        "the same fact three times is one claim: {:?}",
        concept.claims().map(|c| c.text.clone()).collect::<Vec<_>>()
    );
}

/// B-27, seen in the live store: `log.md` held "Name is Sabharish" and "Sabharish's name is
/// Sabharish" as two claims, and each run retired the last wording as if the name had changed.
///
/// The extractor is a model, so it never words a fact the same way twice. Three sessions, three
/// wordings, one fact, and nothing to correct.
#[tokio::test]
async fn one_fact_worded_differently_each_run_stays_one_claim() {
    let store = Store::new(
        "rephrased",
        &["episodes/a.md", "episodes/b.md", "episodes/c.md"],
    )
    .await;
    let wordings = [
        ("episodes/a.md", "Name is Sabharish"),
        ("episodes/b.md", "Sabharish's name is Sabharish"),
        ("episodes/c.md", "The user's name is Sabharish"),
    ];
    let extractor = Scripted::new(
        wordings
            .iter()
            .map(|(path, text)| {
                (
                    (*path).to_string(),
                    vec![about(
                        "name",
                        "Sabharish",
                        text,
                        date(2026, 1, 1),
                        Origin::Stated,
                    )],
                )
            })
            .collect(),
    );

    let mut reports = Vec::new();
    for (path, _) in wordings {
        reports.push(
            store
                .go(&[episode(path, date(2026, 1, 1))], &extractor, &Unbounded)
                .await,
        );
    }

    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        concept.claims().count(),
        1,
        "one fact, three wordings: {:?}",
        concept.claims().map(|c| c.text.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        concept.claims().next().expect("a claim").text,
        "Name is Sabharish",
        "the stored wording is kept, so re-running does not churn the file"
    );
    assert!(
        reports.iter().all(|r| r.decisions.is_empty()),
        "nothing changed, so nothing was superseded: {:?}",
        reports
            .iter()
            .flat_map(|r| &r.decisions)
            .collect::<Vec<_>>()
    );
}

/// A rewording must not swallow a real change: the value differs, so this is a correction.
#[tokio::test]
async fn a_reworded_claim_with_a_new_value_still_supersedes() {
    let store = Store::new("reworded-change", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "Sabharish lives in Chennai",
                date(2026, 1, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "Sabharish has moved to Bangalore",
                date(2026, 7, 1),
                Origin::Stated,
            )],
        ),
    ]);

    store
        .go(
            &[
                episode("episodes/a.md", date(2026, 1, 1)),
                episode("episodes/b.md", date(2026, 7, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    let old = concept
        .claims()
        .find(|c| c.text.contains("Chennai"))
        .expect("the old claim is kept");
    assert!(!old.validity.is_believed(), "the move should be recorded");
}

/// An inferred claim the user later states outright gains standing without gaining a second claim.
#[tokio::test]
async fn a_restatement_upgrades_a_guess_rather_than_adding_to_it() {
    let store = Store::new("upgrade", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "Sabharish's city is Chennai",
                date(2026, 1, 1),
                Origin::Inferred,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "Sabharish is in Chennai",
                date(2026, 2, 1),
                Origin::Stated,
            )],
        ),
    ]);

    store
        .go(
            &[
                episode("episodes/a.md", date(2026, 1, 1)),
                episode("episodes/b.md", date(2026, 2, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(concept.claims().count(), 1);
    let claim = concept.claims().next().expect("a claim");
    assert_eq!(claim.origin, Origin::Stated);
    assert_eq!(
        concept.front.status,
        Status::Stable,
        "a guess said out loud is usable"
    );
}

/// B-25, the reported failure: a name and a degree are not a contradiction.
///
/// Comparing text called every second fact about a person a conflict, rule 4 then took the whole
/// concept out of use, and Loki answered "I don't know your name yet" about a name it had stored.
#[tokio::test]
async fn two_different_facts_about_one_person_both_stand() {
    let store = Store::new("coexist", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![
            about(
                "name",
                "Sabharish",
                "is called Sabharish",
                date(2026, 1, 1),
                Origin::Stated,
            ),
            about(
                "education",
                "Sabharish",
                "is a computer science graduate",
                date(2026, 1, 1),
                Origin::Stated,
            ),
            about(
                "city",
                "Sabharish",
                "lives in Chennai",
                date(2026, 1, 1),
                Origin::Stated,
            ),
        ],
    )]);

    let report = store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    assert!(
        report.surfaced.is_empty(),
        "unrelated facts were filed as conflicts: {:?}",
        report.surfaced
    );
    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(concept.claims().count(), 3, "all three facts should stand");
    assert!(
        concept.claims().all(|c| c.validity.is_believed()),
        "no fact should have been invalidated by an unrelated one"
    );
    assert_eq!(
        concept.front.status,
        Status::Stable,
        "the concept has to stay usable, or none of it can ever be recalled"
    );
}

/// The same attribute stated twice is a correction, and the later one wins.
#[tokio::test]
async fn the_same_attribute_stated_again_supersedes() {
    let store = Store::new("supersede", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "lives in Chennai",
                date(2026, 1, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![about(
                "city",
                "Sabharish",
                "lives in Bangalore",
                date(2026, 7, 1),
                Origin::Stated,
            )],
        ),
    ]);

    store
        .go(
            &[
                episode("episodes/a.md", date(2026, 1, 1)),
                episode("episodes/b.md", date(2026, 7, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    let old = concept
        .claims()
        .find(|c| c.text.contains("Chennai"))
        .expect("the old claim is kept, not deleted");
    assert!(
        !old.validity.is_believed(),
        "the old city should be retired"
    );
    assert_eq!(old.replaced_by.as_deref(), Some("lives in Bangalore"));
    assert_eq!(concept.front.status, Status::Stable);
}

/// A claim that cannot say what it is about has no standing to displace one that can.
#[tokio::test]
async fn a_claim_with_no_attribute_never_conflicts() {
    let store = Store::new("no-key", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![
            about(
                "",
                "Sabharish",
                "something vague",
                date(2026, 1, 1),
                Origin::Stated,
            ),
            about(
                "",
                "Sabharish",
                "something else vague",
                date(2026, 1, 1),
                Origin::Stated,
            ),
        ],
    )]);

    let report = store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    assert!(report.surfaced.is_empty(), "{:?}", report.surfaced);
    assert_eq!(
        store
            .concept_at("people/sabharish.md")
            .await
            .claims()
            .count(),
        2
    );
}

/// B-34: rule 4 marks a concept `draft` so neither claim is used. An unrelated stated claim must
/// not promote it back, or both conflicting claims reach a prompt with nobody having resolved them.
#[tokio::test]
async fn an_unrelated_fact_does_not_clear_a_surfaced_conflict() {
    let store = Store::new("surface-then-promote", &["episodes/a.md"]).await;
    let extractor = Scripted::new(vec![(
        "episodes/a.md".to_string(),
        vec![
            about(
                "city",
                "Sabharish",
                "Sabharish lives in Chennai",
                date(2026, 1, 1),
                Origin::Stated,
            ),
            about(
                "city",
                "Sabharish",
                "Sabharish lives in Bangalore",
                date(2026, 1, 1),
                Origin::Stated,
            ),
            about(
                "education",
                "Sabharish",
                "Sabharish is a computer science graduate",
                date(2026, 1, 1),
                Origin::Stated,
            ),
        ],
    )]);

    let report = store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    assert_eq!(report.surfaced.len(), 1, "the two cities should conflict");

    // The rule is about the two claims, not about the concept. Both cities stay out of a prompt
    // until a person picks; the unrelated degree is unaffected. Asserting the concept's status
    // instead was the first fix, and it was what made one argument hide everything else.
    let visible = store
        .index
        .recall(&Query::prefetch(
            "Sabharish Chennai Bangalore graduate",
            TierScope::normal(Locality::Cloud),
            date(2026, 1, 1),
            10,
        ))
        .expect("recall");
    let texts: Vec<&str> = visible.iter().map(|r| r.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| t.contains("graduate")),
        "the unrelated fact stays usable: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|t| t.contains("Chennai") || t.contains("Bangalore")),
        "neither side of an open conflict may reach a prompt: {texts:?}"
    );
}

/// §9.12, the exposure that arrives with §12's web search. A page's claim about the user can
/// accumulate recurrence and promote into a durable fact with no user statement in its lineage,
/// and nothing else in the design stops it: conflict rules only engage once something contradicts,
/// and an uncontested false claim is never contradicted.
#[tokio::test]
async fn a_claim_from_a_page_is_stored_and_never_promotes() {
    let store = Store::new("web-origin", &["episodes/a.md", "episodes/b.md"]).await;
    let from_page = |path: &str| {
        (
            path.to_string(),
            vec![about(
                "employer",
                "Sabharish",
                "Sabharish works at Acme",
                date(2026, 1, 1),
                Origin::Web,
            )],
        )
    };
    let extractor = Scripted::new(vec![from_page("episodes/a.md"), from_page("episodes/b.md")]);

    // Twice, because recurrence is exactly the signal that would otherwise promote it.
    store
        .go(
            &[
                episode("episodes/a.md", date(2026, 1, 1)),
                episode("episodes/b.md", date(2026, 2, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(concept.claims().count(), 1, "stored, and stored once");
    assert_eq!(concept.claims().next().expect("claim").origin, Origin::Web);
    assert_eq!(
        concept.front.status,
        Status::Draft,
        "a page's claim never earns its way into a prompt by repetition"
    );

    // Searchable, which is the half §9.12 keeps: usable in the turn that fetched it.
    let hits = store
        .index
        .recall(&Query {
            visibility: Visibility::Everything,
            ..Query::prefetch(
                "Acme",
                TierScope::normal(Locality::Cloud).including_foreign(),
                date(2026, 3, 1),
                5,
            )
        })
        .expect("recall");
    assert!(
        hits.iter().any(|h| h.text.contains("Acme")),
        "a web claim stays searchable: {hits:?}"
    );

    // And invisible to the automatic path, which is the half it takes away.
    let prefetched = store
        .index
        .recall(&Query::prefetch(
            "Acme",
            TierScope::normal(Locality::Cloud),
            date(2026, 3, 1),
            5,
        ))
        .expect("recall");
    assert!(
        prefetched.is_empty(),
        "pre-fetch must not carry content the user never gave: {prefetched:?}"
    );
}

/// §9.7 rule 3, widened by §9.12: content that did not come from the user never displaces
/// content that did.
#[tokio::test]
async fn a_page_never_overwrites_what_the_user_said() {
    let store = Store::new("web-vs-stated", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![about(
                "employer",
                "Sabharish",
                "Sabharish works at Loki",
                date(2026, 1, 1),
                Origin::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![about(
                "employer",
                "Sabharish",
                "Sabharish works at Acme",
                date(2026, 7, 1),
                Origin::Web,
            )],
        ),
    ]);

    store
        .go(
            &[
                episode("episodes/a.md", date(2026, 1, 1)),
                episode("episodes/b.md", date(2026, 7, 1)),
            ],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    let stated = concept
        .claims()
        .find(|c| c.text.contains("Loki"))
        .expect("what the user said is still there");
    assert!(
        stated.validity.is_believed(),
        "a page is newer in world time and still loses"
    );
}

/// Found in Sabharish's store: a concept left `draft` by the rule-4 behaviour that no longer
/// exists, holding a stated, unconflicted name that nothing would ever promote, because promotion
/// only runs when a new claim arrives for that entity.
///
/// The status is derivable from the claims, so a run repairs it rather than needing a wipe.
#[tokio::test]
async fn a_run_repairs_a_draft_that_has_already_earned_stable() {
    let store = Store::new("repair", &["episodes/a.md"]).await;
    {
        let writer = store.bundle.writer().await;
        writer
            .write(
                "people/sabharish.md",
                "---\nname: Sabharish\nstatus: draft\ngenerated:\n  by: loki/0.1\n  \
                 at: 2026-01-01\nokf_version: '0.2'\n---\n\n## name\n\
                 - The user's name is Sabharish\n  attribute: name\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   origin: stated\n",
            )
            .expect("write");
        writer
            .commit("a store left draft by an older rule")
            .expect("commit");
    }

    let extractor = Scripted::new(vec![]);
    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    let concept = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        concept.front.status,
        Status::Stable,
        "a stated, unconflicted claim has earned its way into use"
    );

    let hits = store
        .index
        .recall(&Query::prefetch(
            "what is my name",
            TierScope::normal(Locality::Cloud),
            date(2026, 1, 1),
            5,
        ))
        .expect("recall");
    assert!(
        hits.iter().any(|h| h.text.contains("Sabharish")),
        "the repaired concept is reachable: {hits:?}"
    );
}

/// From Sabharish's store: `interest` and `interests` kept two spellings of one property from ever
/// being compared, so the identical sentence sat in the file twice and surfaced as two sides of a
/// question the user could not meaningfully answer.
#[tokio::test]
async fn a_run_folds_duplicates_that_two_attribute_spellings_hid() {
    let store = Store::new("fold", &["episodes/a.md"]).await;
    {
        let writer = store.bundle.writer().await;
        writer
            .write(
                "people/sabharish.md",
                "---\nname: Sabharish\nstatus: stable\ngenerated:\n  by: loki/0.1\n  \
                 at: 2026-01-01\nokf_version: '0.2'\n---\n\n## interest\n\
                 - Sabharish is interested in AI\n  attribute: interest\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   origin: stated\n\
                 \n## interests\n\
                 - Sabharish is interested in AI\n  attribute: interests\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   origin: stated\n",
            )
            .expect("write");
        writer.commit("two spellings of one key").expect("commit");
    }

    let before = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        before.claims().count(),
        2,
        "the fixture really does hold two"
    );

    let extractor = Scripted::new(vec![]);
    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    let after = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        after.claims().count(),
        1,
        "one fact written twice is one fact: {:?}",
        after.claims().map(|c| c.text.clone()).collect::<Vec<_>>()
    );
    assert_eq!(after.front.status, Status::Stable);
}

/// The fold is only for restatements. Two things that genuinely differ both stay.
#[tokio::test]
async fn folding_leaves_claims_that_actually_differ_alone() {
    let store = Store::new("fold-keeps", &["episodes/a.md"]).await;
    {
        let writer = store.bundle.writer().await;
        writer
            .write(
                "people/sabharish.md",
                "---\nname: Sabharish\nstatus: stable\ngenerated:\n  by: loki/0.1\n  \
                 at: 2026-01-01\nokf_version: '0.2'\n---\n\n## education\n\
                 - Sabharish is a computer science student\n  attribute: education\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   origin: stated\n\
                 - Sabharish completed a B.Tech in May 2026\n  attribute: education\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   origin: stated\n",
            )
            .expect("write");
        writer.commit("two real claims").expect("commit");
    }

    let extractor = Scripted::new(vec![]);
    store
        .go(
            &[episode("episodes/a.md", date(2026, 1, 1))],
            &extractor,
            &Unbounded,
        )
        .await;

    let after = store.concept_at("people/sabharish.md").await;
    assert_eq!(
        after.claims().count(),
        2,
        "these are a real question, not a duplicate"
    );
}
