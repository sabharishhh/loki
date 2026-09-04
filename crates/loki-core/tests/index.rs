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

/// What import writes: everything `draft` (§11.4), indexed and filtered at query time.
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

/// A card that answers to a word none of its claims contain (§10.1, D-070).
const ASHOK: &str = r"---
name: Ashok
status: stable
aliases:
- appa
generated:
  by: loki/0.1
  at: 2026-08-01
tags:
- person
okf_version: '0.2'
---

## Work
- Runs a civil contracting firm in Palakkad
  learned: 2026-08-01   unlearned: null
  confidence: high   source: stated
  usage_count: 6
";

/// One long claim carrying the usage and recency §10.1 weighs, and a bm25 that buries it.
///
/// Long on purpose: bm25 penalises a long document, so this sorts below every decoy on keyword
/// strength and can only surface if the other three signals are allowed to speak.
const NANDINI: &str = r"---
name: Nandini
status: stable
generated:
  by: loki/0.1
  at: 2026-08-30
tags:
- person
okf_version: '0.2'
---

## Delivery
- Argued for most of an afternoon that the tooling rewrite, the migration and the documentation backlog should not all land together, and settled on holding every one of them back until the next release
  learned: 2026-08-30   unlearned: null
  confidence: high   source: stated
  usage_count: 40
";

/// A short card mentioning the crowded word once, so bm25 puts it near the top and nothing else
/// about it earns a place.
fn decoy(name: &str) -> String {
    format!(
        "---
name: {name}
status: stable
generated:
  by: loki/0.1
  at: 2025-01-05
okf_version: '0.2'
---

## Misc
- The release went out
  learned: 2025-01-05   unlearned: null
  confidence: low   source: inferred
"
    )
}

struct Store {
    bundle: Bundle,
    index: Index,
    dir: std::path::PathBuf,
}

/// A scratch directory unique to the test and the thread running it.
fn scratch(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loki-index-{}-{label}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

impl Store {
    async fn new(label: &str) -> Self {
        let dir = scratch(label);
        let bundle = Bundle::open(&dir).await.expect("open bundle");
        {
            let writer = bundle.writer().await;
            writer.write("people/meera.md", MEERA).expect("meera");
            writer.write("projects/loki.md", LOKI).expect("loki");
            writer.write("episodes/notes.md", NOTES).expect("notes");
            writer
                .write("people/imported.md", IMPORTED)
                .expect("imported");
        }
        let index = Index::in_memory().expect("index");
        let store = Self { bundle, index, dir };
        store.sync().await;
        store
    }

    /// Six cards mentioning one word, so any realistic cap is smaller than the candidate set.
    ///
    /// The other two are the cases a cap can hide: one that bm25 buries and the signals lift, and
    /// one that answers to a name rather than to its own words.
    async fn crowded(label: &str) -> Self {
        let dir = scratch(label);
        let bundle = Bundle::open(&dir).await.expect("open bundle");
        {
            let writer = bundle.writer().await;
            for name in ["one", "two", "three", "four", "five", "six"] {
                writer
                    .write(&format!("projects/{name}.md"), &decoy(name))
                    .expect("decoy");
            }
            writer.write("people/nandini.md", NANDINI).expect("nandini");
            writer.write("people/ashok.md", ASHOK).expect("ashok");
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
        self.recall_at(text, 10)
    }

    fn recall_at(&self, text: &str, limit: usize) -> Vec<loki_core::memory::index::Recalled> {
        self.index
            .recall(&Query::prefetch(
                text,
                TierScope::normal(Locality::Cloud),
                TODAY,
                limit,
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
            .any(|h| h.path == "people/imported.md" && h.status == Status::Draft),
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

/// A schema bump has to rebuild, or a store written by the previous version keeps a shape the
/// current queries do not match. Found while renaming a column inside one version: the index
/// still claimed to be current and every recall failed on the missing column.
#[test]
fn an_index_from_an_older_schema_is_rebuilt_rather_than_queried() {
    let dir = std::env::temp_dir().join(format!(
        "loki-index-upgrade-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("index.sqlite");

    // A store as an older version left it: a plausible table, and a stale version number.
    {
        let db = rusqlite::Connection::open(&path).expect("open");
        db.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key, value) VALUES ('schema', '1');
             CREATE TABLE claim (id INTEGER PRIMARY KEY, gone TEXT);
             CREATE TABLE turn (id INTEGER PRIMARY KEY, gone TEXT);",
        )
        .expect("old schema");
    }

    let index = Index::open(&path).expect("an older index has to open, not fail");
    assert_eq!(index.claim_count().expect("count"), 0);

    // The current shape is in place, so a real query runs rather than failing on a missing column.
    let hits = index
        .recall(&Query::prefetch(
            "anything",
            TierScope::normal(Locality::Cloud),
            jiff::civil::date(2026, 1, 1),
            5,
        ))
        .expect("recall against a rebuilt index");
    assert!(hits.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// B-58. The cap is on what reaches the prompt, never on what the ranking may consider.
///
/// Recall scored candidates in bm25 order and stopped at the cap, so the three signals that are
/// not keyword match, 45 percent of the weight, could only reorder claims bm25 had already put in
/// front. The four cases below are one family: a cut taken before the sort.
#[tokio::test]
async fn a_claim_bm25_buries_is_still_ranked_when_the_signals_lift_it() {
    let store = Store::crowded("buried").await;

    let hits = store.recall_at("the release", 3);

    assert!(
        hits.iter().any(|h| h.path == "people/nandini.md"),
        "a heavily used, recent claim must reach a cap of three: {hits:?}"
    );
}

/// The other door into §10.1's ranking, and the one a cap closed first.
///
/// The question names a card by an alias and also carries a word six other cards match, so the
/// keyword hits fill the cap on their own. The margin here is deliberately wide: the case is about
/// whether the alias door is reachable at all, not about where it lands.
#[tokio::test]
async fn a_name_match_is_reached_even_when_keyword_hits_fill_the_cap() {
    let store = Store::crowded("alias").await;

    let hits = store.recall_at("what did appa say about the release", 3);

    assert!(
        hits.iter().any(|h| h.path == "people/ashok.md"),
        "a card answering to a query term must be ranked, not left past the cut: {hits:?}"
    );
}

/// The invariant rather than an instance, so a future cut before the sort fails here too.
#[tokio::test]
async fn a_capped_result_is_the_head_of_an_uncapped_one() {
    let store = Store::crowded("head").await;

    let capped = store.recall_at("the release", 3);
    let full = store.recall_at("the release", 50);

    let rows = |hits: &[loki_core::memory::index::Recalled]| {
        hits.iter()
            .take(3)
            .map(|h| (h.path.clone(), h.ordinal))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(&capped),
        rows(&full),
        "capping must cut the tail, not decide the head"
    );
}

/// The other direction, because scoring everything is not a licence to return everything.
#[tokio::test]
async fn the_cap_still_holds_when_far_more_claims_match() {
    let store = Store::crowded("cap").await;

    assert_eq!(store.recall_at("the release", 2).len(), 2);
    assert!(store.recall_at("the release", 50).len() > 2);
}

/// B-59. A record nobody can read projects to nothing.
///
/// A hand edit that broke a file used to leave its rows standing: recall kept serving the text of
/// a claim the file no longer held, while §17.3's screen dropped the card entirely. The two
/// cases below are the halves of that, and the family is a projection outliving its record.
#[tokio::test]
async fn a_file_that_stops_parsing_stops_being_recalled() {
    let store = Store::new("unreadable").await;
    assert!(
        !store.recall("what team is Meera on").is_empty(),
        "the fixture has to be recallable before the edit means anything"
    );

    // A person edits the card and loses a colon. Nothing else about the file changes.
    {
        let writer = store.bundle.writer().await;
        writer
            .write(
                "people/meera.md",
                &MEERA.replace("learned: 2026-08-29", "learned 2026-08-29"),
            )
            .expect("edit");
    }
    let stats = store.sync().await;

    assert_eq!(
        stats.unreadable.len(),
        1,
        "the sync has to notice: {stats:?}"
    );
    assert_eq!(stats.unreadable[0].0, "people/meera.md");
    assert!(
        store
            .recall("what team is Meera on")
            .iter()
            .all(|h| h.path != "people/meera.md"),
        "a claim the file no longer holds must not still answer"
    );
}

/// The property underneath it. `rebuild` is `sync` over a wiped index, so if the two disagree,
/// one of them is wrong about what the files say.
#[tokio::test]
async fn sync_and_rebuild_agree_about_a_file_that_will_not_parse() {
    let store = Store::new("agree").await;
    {
        let writer = store.bundle.writer().await;
        writer
            .write(
                "people/meera.md",
                &MEERA.replace("learned: 2026-08-29", "learned 2026-08-29"),
            )
            .expect("edit");
    }
    store.sync().await;
    let after_sync = store.index.claim_count().expect("count");

    {
        let reader = store.bundle.reader().await;
        store.index.rebuild(&reader).expect("rebuild");
    }
    let after_rebuild = store.index.claim_count().expect("count");

    assert_eq!(
        after_sync, after_rebuild,
        "an incremental sync and a rebuild have to describe the same store"
    );
}
