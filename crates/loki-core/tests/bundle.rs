//! The bundle against a real filesystem and a real git repository.
//!
//! Unit tests would mock away the two things most worth checking: that a model-supplied path
//! cannot escape the root, and that git actually commits.

use jiff::civil::date;
use loki_core::memory::bundle::{Bundle, BundleError, SCRATCH};
use loki_core::memory::claim::Claim;
use loki_core::memory::concept::{Frontmatter, RawConcept, Status};
use loki_core::memory::history::ChangeKind;
use std::sync::Arc;
use std::time::Duration;

/// A bundle in a fresh directory, removed when the guard drops.
struct Temp {
    bundle: Bundle,
    dir: std::path::PathBuf,
}

impl Temp {
    async fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-bundle-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).await.expect("open bundle");
        Self { bundle, dir }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn opening_creates_the_layout_from_section_9_3() {
    let temp = Temp::new("layout").await;
    for dir in ["people", "projects", "preferences", "episodes", SCRATCH] {
        assert!(temp.dir.join(dir).is_dir(), "{dir} was not created");
    }
    for file in ["index.md", "log.md", "working-set.md", "standing.md"] {
        assert!(temp.dir.join(file).is_file(), "{file} was not created");
    }
    assert!(temp.dir.join(".git").exists(), "git was not initialised");
}

#[tokio::test]
async fn opening_an_existing_bundle_does_not_disturb_it() {
    let temp = Temp::new("reopen").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "hello")
        .expect("write");
    let reopened = Bundle::open(&temp.dir).await.expect("reopen");
    assert_eq!(
        reopened.reader().await.read("people/meera.md").unwrap(),
        "hello"
    );
}

/// A model writes these paths, so escaping the root has to be impossible.
#[tokio::test]
async fn paths_cannot_escape_the_bundle() {
    let temp = Temp::new("escape").await;
    let attempts = [
        "../outside.md",
        "people/../../outside.md",
        "/etc/passwd",
        "people/../../../../../../tmp/owned.md",
    ];
    for path in attempts {
        assert!(
            matches!(
                temp.bundle.writer().await.write(path, "x"),
                Err(BundleError::OutsideBundle { .. })
            ),
            "{path} was not rejected"
        );
        assert!(
            matches!(
                temp.bundle.reader().await.read(path),
                Err(BundleError::OutsideBundle { .. })
            ),
            "{path} was readable"
        );
    }
    assert!(!temp.dir.parent().unwrap().join("outside.md").exists());
}

#[tokio::test]
async fn a_path_that_merely_mentions_dots_is_fine() {
    let temp = Temp::new("dots").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera..notes.md", "fine")
        .expect("write");
    assert_eq!(
        temp.bundle
            .reader()
            .await
            .read("people/meera..notes.md")
            .unwrap(),
        "fine"
    );
}

#[tokio::test]
async fn write_read_append_and_list_behave() {
    let temp = Temp::new("primitives").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "one\n")
        .expect("write");
    temp.bundle
        .writer()
        .await
        .append("people/meera.md", "two\n")
        .expect("append");
    assert_eq!(
        temp.bundle.reader().await.read("people/meera.md").unwrap(),
        "one\ntwo\n"
    );

    temp.bundle
        .writer()
        .await
        .write("people/rahul.md", "x")
        .expect("write");
    assert_eq!(
        temp.bundle.reader().await.ls("people").unwrap(),
        ["meera.md", "rahul.md"]
    );
}

#[tokio::test]
async fn appending_to_a_missing_file_creates_it() {
    let temp = Temp::new("append-new").await;
    temp.bundle
        .writer()
        .await
        .append("people/new.md", "first\n")
        .expect("append");
    assert_eq!(
        temp.bundle.reader().await.read("people/new.md").unwrap(),
        "first\n"
    );
}

/// Nothing may silently overwrite anything, so an edit that could hit two places is refused.
#[tokio::test]
async fn an_ambiguous_edit_is_refused_rather_than_guessed() {
    let temp = Temp::new("ambiguous").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "infra\ninfra\n")
        .expect("write");

    assert!(matches!(
        temp.bundle
            .writer()
            .await
            .edit("people/meera.md", "infra", "platform"),
        Err(BundleError::Ambiguous { .. })
    ));
    // Unchanged.
    assert_eq!(
        temp.bundle.reader().await.read("people/meera.md").unwrap(),
        "infra\ninfra\n"
    );
}

#[tokio::test]
async fn an_edit_with_no_match_is_an_error_not_a_no_op() {
    let temp = Temp::new("nomatch").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "infra\n")
        .expect("write");
    assert!(matches!(
        temp.bundle
            .writer()
            .await
            .edit("people/meera.md", "platform", "infra"),
        Err(BundleError::NoMatch { .. })
    ));
}

#[tokio::test]
async fn a_unique_edit_applies() {
    let temp = Temp::new("edit").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on platform\n")
        .expect("write");
    temp.bundle
        .writer()
        .await
        .edit("people/meera.md", "platform", "infra")
        .expect("edit");
    assert_eq!(
        temp.bundle.reader().await.read("people/meera.md").unwrap(),
        "Works on infra\n"
    );
}

#[tokio::test]
async fn grep_finds_lines_and_search_ranks_them() {
    let temp = Temp::new("search").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on the infra team\n")
        .expect("write");
    temp.bundle
        .writer()
        .await
        .write(
            "people/rahul.md",
            "Works on the platform team\nRuns infra reviews\n",
        )
        .expect("write");

    let hits = temp
        .bundle
        .reader()
        .await
        .grep("infra", None)
        .expect("grep");
    assert_eq!(hits.len(), 2);

    // Two terms beat one, so meera's line ranks above rahul's.
    let ranked = temp
        .bundle
        .reader()
        .await
        .search("infra team")
        .expect("search");
    assert_eq!(ranked[0].path, "people/meera.md");
}

#[tokio::test]
async fn search_ignores_the_git_directory() {
    let temp = Temp::new("gitignore").await;
    temp.bundle.writer().await.commit("first").expect("commit");
    // A term certain to appear in git's own files but not in memory.
    let hits = temp.bundle.reader().await.grep("ref:", None).expect("grep");
    assert!(hits.iter().all(|h| !h.path.contains(".git")));
}

#[tokio::test]
async fn a_concept_round_trips_through_the_bundle() {
    let temp = Temp::new("concept").await;
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);
    concept.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));

    temp.bundle
        .writer()
        .await
        .save_concept("people/meera.md", &concept)
        .expect("save");
    let loaded = temp
        .bundle
        .reader()
        .await
        .load_concept("people/meera.md")
        .expect("load");
    assert_eq!(loaded, concept);
}

#[tokio::test]
async fn concepts_excludes_scratch_and_generated_files() {
    let temp = Temp::new("listing").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "---\nx\n---\n")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .write("scratch/draft.md", "---\nx\n---\n")
        .unwrap();

    let concepts = temp.bundle.reader().await.concepts().expect("concepts");
    assert_eq!(concepts, ["people/meera.md"]);
}

#[tokio::test]
async fn committing_records_a_change_and_nothing_when_there_is_none() {
    let temp = Temp::new("git").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "one\n")
        .expect("write");

    assert!(temp.bundle.writer().await.commit("first").expect("commit"));
    assert_eq!(temp.bundle.reader().await.commit_count().unwrap(), 1);

    // Nothing changed, so nothing to record. Not an error.
    assert!(!temp.bundle.writer().await.commit("second").expect("commit"));
    assert_eq!(temp.bundle.reader().await.commit_count().unwrap(), 1);

    temp.bundle
        .writer()
        .await
        .append("people/meera.md", "two\n")
        .expect("append");
    assert!(temp.bundle.writer().await.commit("third").expect("commit"));
    assert_eq!(temp.bundle.reader().await.commit_count().unwrap(), 2);
}

/// The bundle is the whole store, so copying the directory has to be enough to move machines.
#[tokio::test]
async fn a_copied_bundle_carries_everything() {
    let temp = Temp::new("portable").await;
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);
    concept.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));
    temp.bundle
        .writer()
        .await
        .save_concept("people/meera.md", &concept)
        .unwrap();
    temp.bundle.writer().await.commit("seed").unwrap();

    let copy_dir = temp.dir.with_extension("copy");
    let _ = std::fs::remove_dir_all(&copy_dir);
    let status = std::process::Command::new("cp")
        .args(["-R", temp.dir.to_str().unwrap(), copy_dir.to_str().unwrap()])
        .status()
        .expect("cp");
    assert!(status.success());

    let moved = Bundle::open(&copy_dir).await.expect("open the copy");
    assert_eq!(
        moved
            .reader()
            .await
            .load_concept("people/meera.md")
            .unwrap(),
        concept
    );
    assert_eq!(moved.reader().await.commit_count().unwrap(), 1);

    let _ = std::fs::remove_dir_all(&copy_dir);
}

// History. Not a feature: the timeline is the trust surface, and memory undo is a revert.

#[tokio::test]
async fn history_reads_newest_first() {
    let temp = Temp::new("history").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "platform\n")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .commit("learned: platform team")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "infra\n")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .commit("corrected: infra team")
        .unwrap();

    let history = temp.bundle.reader().await.history(10).expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].message, "corrected: infra team");
    assert_eq!(history[1].message, "learned: platform team");
    assert!(history[0].at >= history[1].at);
}

/// Commits written inside one second have identical timestamps, which a consolidation touching
/// several files will produce. Ordering must still be parent-last.
#[tokio::test]
async fn history_is_ordered_even_when_commits_share_a_timestamp() {
    let temp = Temp::new("same-second").await;
    for n in 0..6 {
        temp.bundle
            .writer()
            .await
            .write("people/meera.md", &format!("version {n}\n"))
            .unwrap();
        temp.bundle
            .writer()
            .await
            .commit(&format!("commit {n}"))
            .unwrap();
    }

    let history = temp.bundle.reader().await.history(10).expect("history");
    let messages: Vec<&str> = history.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "commit 5", "commit 4", "commit 3", "commit 2", "commit 1", "commit 0"
        ]
    );
}

#[tokio::test]
async fn history_on_an_empty_bundle_is_empty_not_an_error() {
    let temp = Temp::new("history-empty").await;
    assert!(
        temp.bundle
            .reader()
            .await
            .history(10)
            .expect("history")
            .is_empty()
    );
    assert_eq!(temp.bundle.reader().await.commit_count().unwrap(), 0);
}

/// One timeline of the present cannot answer this. Git can.
#[tokio::test]
async fn a_file_can_be_read_as_it_stood_at_an_older_revision() {
    let temp = Temp::new("read-at").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on the platform team\n")
        .unwrap();
    temp.bundle.writer().await.commit("march").unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on the infra team\n")
        .unwrap();
    temp.bundle.writer().await.commit("august").unwrap();

    let history = temp.bundle.reader().await.history(10).unwrap();
    let march = &history[1].id;

    assert_eq!(
        temp.bundle
            .reader()
            .await
            .read_at("people/meera.md", march)
            .unwrap()
            .trim(),
        "Works on the platform team"
    );
    assert_eq!(
        temp.bundle
            .reader()
            .await
            .read("people/meera.md")
            .unwrap()
            .trim(),
        "Works on the infra team"
    );
}

#[tokio::test]
async fn a_concept_can_be_parsed_as_it_stood_at_a_revision() {
    let temp = Temp::new("concept-at").await;
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;

    let mut old = RawConcept::new(front.clone());
    old.add(
        "Role",
        Claim::stated("Works on platform", date(2026, 3, 12)),
    );
    temp.bundle
        .writer()
        .await
        .save_concept("people/meera.md", &old)
        .unwrap();
    temp.bundle.writer().await.commit("march").unwrap();

    let mut new = RawConcept::new(front);
    new.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));
    temp.bundle
        .writer()
        .await
        .save_concept("people/meera.md", &new)
        .unwrap();
    temp.bundle.writer().await.commit("august").unwrap();

    let march = temp.bundle.reader().await.history(10).unwrap()[1]
        .id
        .clone();
    let then = temp
        .bundle
        .reader()
        .await
        .load_concept_at("people/meera.md", &march)
        .expect("load at revision");
    assert_eq!(then, old);
}

#[tokio::test]
async fn a_revision_reports_what_it_changed() {
    let temp = Temp::new("changed").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "one\n")
        .unwrap();
    temp.bundle.writer().await.commit("first").unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "two\n")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/rahul.md", "new\n")
        .unwrap();
    temp.bundle.writer().await.commit("second").unwrap();

    let head = temp.bundle.reader().await.history(1).unwrap()[0].id.clone();
    let mut changes = temp
        .bundle
        .reader()
        .await
        .changed_in(&head)
        .expect("changed");
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].path, "people/meera.md");
    assert_eq!(changes[0].kind, ChangeKind::Modified);
    assert_eq!(changes[1].path, "people/rahul.md");
    assert_eq!(changes[1].kind, ChangeKind::Added);
}

/// Section 14.3. Undo is a compensating action appended, never a deletion.
#[tokio::test]
async fn reverting_restores_the_content_and_keeps_the_history() {
    let temp = Temp::new("revert").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on the platform team\n")
        .unwrap();
    temp.bundle.writer().await.commit("march").unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "Works on the infra team\n")
        .unwrap();
    temp.bundle
        .writer()
        .await
        .commit("a wrong correction")
        .unwrap();

    let bad = temp.bundle.reader().await.history(1).unwrap()[0].id.clone();
    temp.bundle.writer().await.revert(&bad).expect("revert");

    // Content is back.
    assert_eq!(
        temp.bundle
            .reader()
            .await
            .read("people/meera.md")
            .unwrap()
            .trim(),
        "Works on the platform team"
    );

    // And nothing was erased. Three commits, with the mistake still visible.
    let history = temp.bundle.reader().await.history(10).unwrap();
    assert_eq!(history.len(), 3);
    assert!(history[0].message.starts_with("Revert"));
    assert_eq!(history[1].message, "a wrong correction");
}

#[tokio::test]
async fn the_bundle_is_usable_again_after_a_revert() {
    let temp = Temp::new("revert-then-write").await;
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "one\n")
        .unwrap();
    temp.bundle.writer().await.commit("first").unwrap();
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "two\n")
        .unwrap();
    temp.bundle.writer().await.commit("second").unwrap();

    let second = temp.bundle.reader().await.history(1).unwrap()[0].id.clone();
    temp.bundle.writer().await.revert(&second).expect("revert");

    // A revert leaves git in a special state unless it is cleaned up. Prove it was.
    temp.bundle
        .writer()
        .await
        .write("people/meera.md", "three\n")
        .unwrap();
    assert!(
        temp.bundle
            .writer()
            .await
            .commit("third")
            .expect("commit after revert")
    );
    assert_eq!(
        temp.bundle.reader().await.history(1).unwrap()[0].message,
        "third"
    );
}

// Section 7.2: any number of readers, or one writer, never both.

#[tokio::test]
async fn many_readers_may_hold_the_bundle_at_once() {
    let temp = Temp::new("many-readers").await;
    temp.bundle
        .writer()
        .await
        .write("people/a.md", "x")
        .unwrap();

    let a = temp.bundle.reader().await;
    let b = temp.bundle.reader().await;
    let c = temp.bundle.reader().await;

    assert_eq!(a.read("people/a.md").unwrap(), "x");
    assert_eq!(b.read("people/a.md").unwrap(), "x");
    assert_eq!(c.read("people/a.md").unwrap(), "x");
}

/// A consolidation rewrites many files then commits. A reader landing mid-pass would see a bundle
/// that was never true, so the writer has to hold every reader off for the whole pass.
#[tokio::test]
async fn a_reader_cannot_observe_a_half_finished_write_pass() {
    let temp = Temp::new("exclusion").await;
    {
        let writer = temp.bundle.writer().await;
        writer.write("people/a.md", "before").unwrap();
        writer.write("people/b.md", "before").unwrap();
    }

    let bundle = Arc::new(temp.bundle.clone());
    let writing = {
        let bundle = Arc::clone(&bundle);
        tokio::spawn(async move {
            let writer = bundle.writer().await;
            writer.write("people/a.md", "after").unwrap();
            // A real consolidation does slow work between writes.
            tokio::time::sleep(Duration::from_millis(120)).await;
            writer.write("people/b.md", "after").unwrap();
        })
    };

    tokio::time::sleep(Duration::from_millis(30)).await;

    // Blocks until the pass finishes, so both files agree.
    let reader = bundle.reader().await;
    assert_eq!(reader.read("people/a.md").unwrap(), "after");
    assert_eq!(reader.read("people/b.md").unwrap(), "after");

    writing.await.unwrap();
}

#[tokio::test]
async fn a_checkpoint_can_record_the_bundle_head() {
    let temp = Temp::new("snapshot").await;
    assert!(
        temp.bundle.reader().await.snapshot().unwrap().is_none(),
        "an uncommitted bundle has no snapshot"
    );

    temp.bundle
        .writer()
        .await
        .write("people/a.md", "x")
        .unwrap();
    temp.bundle.writer().await.commit("first").unwrap();

    let first = temp
        .bundle
        .reader()
        .await
        .snapshot()
        .unwrap()
        .expect("snapshot");
    temp.bundle
        .writer()
        .await
        .write("people/a.md", "y")
        .unwrap();
    temp.bundle.writer().await.commit("second").unwrap();
    let second = temp
        .bundle
        .reader()
        .await
        .snapshot()
        .unwrap()
        .expect("snapshot");

    assert_ne!(first, second, "the snapshot must move when memory does");
}
