//! The index against a real bundle on a real filesystem.
//!
//! Written as markdown files rather than through the concept API, because that is how the store
//! actually arrives: the agent writes files, and the index reads whatever is there.

use jiff::civil::date;
use loki_core::core::vocab::Locality;
use loki_core::memory::bundle::Bundle;
use loki_core::memory::claim::Privacy;
use loki_core::memory::concept::Status;
use loki_core::memory::gate::TierScope;
use loki_core::memory::index::{Index, Query, Use, Visibility};

const TODAY: jiff::civil::Date = date(2026, 9, 1);

/// The §9.5 worked example. One current claim, one superseded by it.
const MEERA: &str = r"---
name: Meera
status: stable
generated:
  by: loki/0.1
  at: 2026-03-12
verified:
- by: 'human:sabharish'
  at: 2026-08-29
tags:
- person
okf_version: '0.2'
---

## Role
- Works on the infra team at [Loki](../projects/loki.md)
  valid_from: 2026-07-15   valid_to: null
  learned: 2026-08-29   unlearned: null
  confidence: high   source: stated

- Works on the platform team
  valid_from: 2026-03-12   valid_to: 2026-07-15
  learned: 2026-03-12   unlearned: 2026-08-29
  confidence: high   source: stated
  replaced_by: Works on the infra team
";

const LOKI: &str = r"---
name: Loki
status: stable
generated:
  by: loki/0.1
  at: 2026-06-01
tags:
- project
okf_version: '0.2'
---

## Shape
- The infra team owns the deployment pipeline
  valid_from: 2026-06-01   valid_to: null
  learned: 2026-06-01   unlearned: null
  confidence: high   source: stated
";

/// Unrelated to the other two, and deliberately matching the same words, so link distance is the
/// only thing that can separate it from Meera's claim.
const NOTES: &str = r"---
name: Old Notes
status: stable
generated:
  by: loki/0.1
  at: 2026-06-01
okf_version: '0.2'
---

## Misc
- Someone mentioned an infra team in passing
  valid_from: 2026-06-01   valid_to: null
  learned: 2026-06-01   unlearned: null
  confidence: low   source: inferred

- The salary discussion happened in March
  valid_from: 2026-03-01   valid_to: null
  learned: 2026-03-01   unlearned: null
  confidence: high   source: stated
  privacy: private
";

/// What import writes: everything `draft`, in `scratch/`.
const IMPORTED: &str = r"---
name: Imported Fragment
status: draft
generated:
  by: loki-import/0.1
  at: 2026-09-01
okf_version: '0.2'
---

## Claims
- Used to work on the infra team years ago
  valid_from: 2020-01-01   valid_to: null
  learned: 2026-09-01   unlearned: null
  confidence: low   source: inferred
";

struct Store {
    bundle: Bundle,
    index: Index,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-index-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).await.expect("open bundle");
        {
            let writer = bundle.writer().await;
            writer.write("people/meera.md", MEERA).expect("meera");
            writer.write("projects/loki.md", LOKI).expect("loki");
            writer.write("episodes/notes.md", NOTES).expect("notes");
            writer
                .write("scratch/imported.md", IMPORTED)
                .expect("imported");
        }
        let index = Index::in_memory().expect("index");
        let store = Self { bundle, index, dir };
        store.sync().await;
        store
    }

    async fn sync(&self) -> loki_core::memory::index::Stats {
        let reader = self.bundle.reader().await;
        self.index.sync(&reader).expect("sync")
    }

    fn recall(&self, text: &str) -> Vec<loki_core::memory::index::Recalled> {
        self.index
            .recall(&Query::prefetch(
                text,
                TierScope::normal(Locality::Cloud),
                TODAY,
                10,
            ))
            .expect("recall")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn a_superseded_claim_never_comes_back() {
    let store = Store::new("superseded").await;
    let hits = store.recall("what team is Meera on");

    let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("infra team at")),
        "the current claim should surface: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("platform team")),
        "the superseded claim must not surface: {texts:?}"
    );
}

#[tokio::test]
async fn drafts_are_searchable_but_never_prompt_eligible() {
    let store = Store::new("drafts").await;

    let eligible = store.recall("infra team");
    assert!(
        eligible.iter().all(|h| h.status == Status::Stable),
        "pre-fetch returned a non-stable claim: {eligible:?}"
    );

    let everything = store
        .index
        .recall(&Query {
            text: "infra team",
            limit: 10,
            context: &[],
            scope: TierScope::normal(Locality::Cloud),
            visibility: Visibility::Everything,
            today: TODAY,
            session: None,
        })
        .expect("recall");
    assert!(
        everything
            .iter()
            .any(|h| h.path == "scratch/imported.md" && h.status == Status::Draft),
        "the review screen must be able to find the draft: {everything:?}"
    );
}

#[tokio::test]
async fn private_claims_need_a_scope_that_asks_for_them() {
    let store = Store::new("private").await;

    let normal = store.recall("salary discussion");
    assert!(
        normal.iter().all(|h| h.privacy == Privacy::Normal),
        "a private claim leaked into pre-fetch: {normal:?}"
    );

    let asked = store
        .index
        .recall(&Query {
            text: "salary discussion",
            limit: 10,
            context: &[],
            scope: TierScope::including_private(Locality::Cloud),
            visibility: Visibility::PromptEligible,
            today: TODAY,
            session: None,
        })
        .expect("recall");
    assert!(
        asked.iter().any(|h| h.privacy == Privacy::Private),
        "a task that asked for private claims got none: {asked:?}"
    );
}

/// Two claims match "infra team" equally well. The one linked from what is already in context
/// should win, which is the §10.1 link-distance signal doing its job.
#[tokio::test]
async fn link_distance_breaks_a_tie_between_equal_matches() {
    let store = Store::new("links").await;

    let blind = store.recall("infra team");
    let ranked = store
        .index
        .recall(&Query {
            text: "infra team",
            limit: 10,
            context: &["projects/loki.md".to_string()],
            scope: TierScope::normal(Locality::Cloud),
            visibility: Visibility::PromptEligible,
            today: TODAY,
            session: None,
        })
        .expect("recall");

    let meera_blind = blind
        .iter()
        .position(|h| h.path == "people/meera.md")
        .expect("meera in the blind result");
    let meera_ranked = ranked
        .iter()
        .position(|h| h.path == "people/meera.md")
        .expect("meera in the context result");
    let notes_ranked = ranked
        .iter()
        .position(|h| h.path == "episodes/notes.md")
        .expect("notes in the context result");

    assert!(
        meera_ranked < notes_ranked,
        "the linked concept should outrank the unrelated one: {ranked:?}"
    );
    let with_context = ranked[meera_ranked].score.value();
    let without = blind[meera_blind].score.value();
    assert!(
        with_context > without,
        "context should raise the score: {with_context} vs {without}"
    );
}

#[tokio::test]
async fn syncing_twice_reindexes_nothing() {
    let store = Store::new("idempotent").await;
    let before = store.index.claim_count().expect("count");

    let stats = store.sync().await;

    assert_eq!(stats.indexed, 0, "nothing changed, so nothing to index");
    assert_eq!(stats.removed, 0);
    assert!(stats.unchanged >= 4, "{stats:?}");
    assert_eq!(store.index.claim_count().expect("count"), before);
}

/// A revert (§14.3) rewrites files underneath the index. Nothing tells the index directly, so it
/// has to notice on the next sync.
#[tokio::test]
async fn a_file_rewritten_underneath_the_index_is_picked_up() {
    let store = Store::new("rewrite").await;
    assert!(!store.recall("deployment pipeline").is_empty());

    let shrunk = LOKI.replace(
        "- The infra team owns the deployment pipeline",
        "- The infra team owns the release calendar",
    );
    store
        .bundle
        .writer()
        .await
        .write("projects/loki.md", &shrunk)
        .expect("rewrite");

    let stats = store.sync().await;
    assert_eq!(stats.indexed, 1, "only the changed concept: {stats:?}");
    assert!(
        store.recall("deployment pipeline").is_empty(),
        "the old claim is still indexed"
    );
    assert!(!store.recall("release calendar").is_empty());
}

#[tokio::test]
async fn a_deleted_concept_leaves_the_index() {
    let store = Store::new("delete").await;
    std::fs::remove_file(store.dir.join("episodes/notes.md")).expect("remove");

    let stats = store.sync().await;

    assert_eq!(stats.removed, 1, "{stats:?}");
    assert!(
        store
            .recall("infra team")
            .iter()
            .all(|h| h.path != "episodes/notes.md")
    );
}

#[tokio::test]
async fn rebuilding_from_the_files_reproduces_the_index() {
    let store = Store::new("rebuild").await;
    let before = store.index.claim_count().expect("count");
    let expected = store.recall("infra team");

    {
        let reader = store.bundle.reader().await;
        store.index.rebuild(&reader).expect("rebuild");
    }

    assert_eq!(store.index.claim_count().expect("count"), before);
    assert_eq!(store.recall("infra team"), expected);
}

/// §9.9 needs use counts, and §9.10 archives on low use plus high age. Counts land in the index
/// on the hot path and consolidation folds them into the files, so they must survive the round
/// trip exactly once.
#[tokio::test]
async fn uses_are_counted_once_and_handed_over_once() {
    let store = Store::new("uses").await;
    let hit = store
        .recall("infra team")
        .into_iter()
        .find(|h| h.path == "people/meera.md")
        .expect("meera");
    let before = hit.score.value();

    let reference = hit.reference();
    for _ in 0..3 {
        store
            .index
            .record_use(std::slice::from_ref(&reference))
            .expect("record");
    }

    let pending = store.index.drain_pending_uses().expect("drain");
    assert_eq!(
        pending.len(),
        1,
        "one claim was used, so one row is pending: {pending:?}"
    );
    assert_eq!(pending[0].uses, 3);
    assert_eq!(pending[0].path, "people/meera.md");

    assert!(
        store.index.drain_pending_uses().expect("drain").is_empty(),
        "draining twice would double-count in the files"
    );

    let after = store
        .recall("infra team")
        .into_iter()
        .find(|h| h.path == "people/meera.md")
        .expect("meera")
        .score
        .value();
    assert!(
        after > before,
        "use should raise the score: {after} vs {before}"
    );
}

#[tokio::test]
async fn recording_a_use_against_a_claim_that_is_gone_is_not_an_error() {
    let store = Store::new("missing-use").await;
    store
        .index
        .record_use(&[Use {
            path: "people/nobody.md".to_string(),
            ordinal: 0,
        }])
        .expect("recording against a missing claim should be a no-op");
    assert!(store.index.drain_pending_uses().expect("drain").is_empty());
}

/// §12.6 asks whether memory already knew, before deciding to search. That is a threshold on the
/// score, so a real hit and a nonsense query have to be far apart on an absolute scale.
#[tokio::test]
async fn a_known_answer_scores_far_above_an_unknown_one() {
    let store = Store::new("threshold").await;

    let known = store.recall("which team is Meera on");
    let unknown = store.recall("quarterly revenue in Lisbon");

    let best_known = known.first().map_or(0.0, |h| h.score.value());
    let best_unknown = unknown.first().map_or(0.0, |h| h.score.value());
    assert!(
        best_known > 0.3,
        "a real hit should be clearly good: {best_known}"
    );
    assert!(
        best_unknown < best_known / 2.0,
        "an unknown question should not look like a hit: {best_unknown} vs {best_known}"
    );
}

/// The in-memory index is what every test above uses. The real one is a file, and it has to
/// survive being closed and reopened.
#[tokio::test]
async fn an_index_on_disk_survives_a_reopen() {
    let store = Store::new("ondisk").await;
    let path = store.dir.join("index.sqlite");

    {
        let index = Index::open(&path).expect("open");
        let reader = store.bundle.reader().await;
        index.sync(&reader).expect("sync");
        assert!(index.claim_count().expect("count") > 0);
    }

    let reopened = Index::open(&path).expect("reopen");
    let hits = reopened
        .recall(&Query::prefetch(
            "infra team",
            TierScope::normal(Locality::Cloud),
            TODAY,
            10,
        ))
        .expect("recall");
    assert!(!hits.is_empty(), "the index did not persist");

    // Nothing changed on disk, so a sync against the reopened index re-reads nothing.
    let reader = store.bundle.reader().await;
    let stats = reopened.sync(&reader).expect("sync");
    assert_eq!(stats.indexed, 0, "{stats:?}");
}

/// §10.7 puts the ceiling at 50 to 300 entities and thousands of claims, and §10.6 claims a
/// rebuild costs milliseconds there. Both are worth holding to a number rather than a hope.
#[tokio::test]
async fn a_full_store_rebuilds_and_recalls_quickly() {
    let dir = std::env::temp_dir().join(format!("loki-index-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bundle = Bundle::open(&dir).await.expect("open");
    {
        let writer = bundle.writer().await;
        for entity in 0..300 {
            let mut doc = format!(
                "---\nname: Entity {entity}\nstatus: stable\ngenerated:\n  by: loki/0.1\n  at: 2026-01-01\nokf_version: '0.2'\n---\n\n## Facts\n"
            );
            for claim in 0..10 {
                doc.push_str(&format!(
                    "- Entity {entity} has property {claim} on the infra team\n  valid_from: 2026-01-01   valid_to: null\n  learned: 2026-01-01   unlearned: null\n  confidence: high   source: stated\n\n"
                ));
            }
            writer
                .write(&format!("people/entity-{entity}.md"), &doc)
                .expect("write");
        }
    }

    let index = Index::in_memory().expect("index");
    let start = std::time::Instant::now();
    {
        let reader = bundle.reader().await;
        index.rebuild(&reader).expect("rebuild");
    }
    let rebuild = start.elapsed();

    assert_eq!(index.claim_count().expect("count"), 3_000);

    let start = std::time::Instant::now();
    let hits = index
        .recall(&Query::prefetch(
            "property 4 infra team",
            TierScope::normal(Locality::Cloud),
            TODAY,
            8,
        ))
        .expect("recall");
    let query = start.elapsed();

    assert_eq!(hits.len(), 8, "the cap should be filled at this scale");
    // Generous, because CI machines vary. The point is to catch an order-of-magnitude regression,
    // not to police milliseconds.
    assert!(
        rebuild < std::time::Duration::from_secs(20),
        "rebuild of 3000 claims took {rebuild:?}"
    );
    assert!(
        query < std::time::Duration::from_millis(500),
        "recall over 3000 claims took {query:?}"
    );
    println!("3000 claims: rebuild {rebuild:?}, recall {query:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
