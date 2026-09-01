//! Consolidation over a real bundle, a real index and real git.
//!
//! Extraction and matching are scripted. Both are model calls in production, and a test whose
//! setup is a model call measures two things and fails for the wrong reasons. What is under test
//! is the pipeline: ordering, precedence, promotion, archival, resumption.

use std::sync::Mutex;

use async_trait::async_trait;
use jiff::civil::{Date, date};
use loki_core::memory::bundle::Bundle;
use loki_core::memory::claim::Source;
use loki_core::memory::concept::Status;
use loki_core::memory::consolidate::{
    Budget, Candidate, ConsolidateError, Episode, Extractor, Report, Unbounded, run,
};
use loki_core::memory::index::{Candidate as EntityCandidate, Index};
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

fn candidate(surface: &str, text: &str, valid_from: Date, source: Source) -> Candidate {
    Candidate {
        surface: surface.to_string(),
        kind: Kind::Person,
        heading: "Notes".to_string(),
        text: text.to_string(),
        days_ago: None,
        valid_from: Some(valid_from),
        source,
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
                Source::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Sabharish",
                "on the infra team",
                date(2026, 7, 15),
                Source::Stated,
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
                Source::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Meera",
                "at Globex",
                date(2026, 3, 1),
                Source::Stated,
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
            Source::Stated,
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
                Source::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Priya",
                "likes coffee",
                date(2026, 2, 1),
                Source::Stated,
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
        Source::Inferred,
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
        date(2025, 6, 6),
        "two weeks before the message, not before today"
    );
}

/// A first mention stays draft, so one offhand remark does not become a fact about you.
#[tokio::test]
async fn a_first_mention_stays_draft_and_a_second_promotes() {
    let store = Store::new("promote", &["episodes/a.md", "episodes/b.md"]).await;
    let extractor = Scripted::new(vec![
        (
            "episodes/a.md".to_string(),
            vec![candidate(
                "Dan",
                "prefers short replies",
                date(2026, 1, 1),
                Source::Stated,
            )],
        ),
        (
            "episodes/b.md".to_string(),
            vec![candidate(
                "Dan",
                "prefers short replies",
                date(2026, 1, 1),
                Source::Stated,
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
        "a first mention must not be prompt-eligible"
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
            Source::Stated,
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
