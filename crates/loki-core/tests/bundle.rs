//! The bundle against a real filesystem and a real git repository.
//!
//! Unit tests would mock away the two things most worth checking: that a model-supplied path
//! cannot escape the root, and that git actually commits.

use jiff::civil::date;
use loki_core::memory::bundle::{Bundle, BundleError, SCRATCH};
use loki_core::memory::claim::Claim;
use loki_core::memory::concept::{Frontmatter, RawConcept, Status};
use loki_core::memory::history::ChangeKind;

/// A bundle in a fresh directory, removed when the guard drops.
struct Temp {
    bundle: Bundle,
    dir: std::path::PathBuf,
}

impl Temp {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-bundle-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).expect("open bundle");
        Self { bundle, dir }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn opening_creates_the_layout_from_section_9_3() {
    let temp = Temp::new("layout");
    for dir in ["people", "projects", "preferences", "episodes", SCRATCH] {
        assert!(temp.dir.join(dir).is_dir(), "{dir} was not created");
    }
    for file in ["index.md", "log.md", "working-set.md", "standing.md"] {
        assert!(temp.dir.join(file).is_file(), "{file} was not created");
    }
    assert!(temp.dir.join(".git").exists(), "git was not initialised");
}

#[test]
fn opening_an_existing_bundle_does_not_disturb_it() {
    let temp = Temp::new("reopen");
    temp.bundle
        .write("people/meera.md", "hello")
        .expect("write");
    let reopened = Bundle::open(&temp.dir).expect("reopen");
    assert_eq!(reopened.read("people/meera.md").unwrap(), "hello");
}

/// A model writes these paths, so escaping the root has to be impossible.
#[test]
fn paths_cannot_escape_the_bundle() {
    let temp = Temp::new("escape");
    let attempts = [
        "../outside.md",
        "people/../../outside.md",
        "/etc/passwd",
        "people/../../../../../../tmp/owned.md",
    ];
    for path in attempts {
        assert!(
            matches!(
                temp.bundle.write(path, "x"),
                Err(BundleError::OutsideBundle { .. })
            ),
            "{path} was not rejected"
        );
        assert!(
            matches!(
                temp.bundle.read(path),
                Err(BundleError::OutsideBundle { .. })
            ),
            "{path} was readable"
        );
    }
    assert!(!temp.dir.parent().unwrap().join("outside.md").exists());
}

#[test]
fn a_path_that_merely_mentions_dots_is_fine() {
    let temp = Temp::new("dots");
    temp.bundle
        .write("people/meera..notes.md", "fine")
        .expect("write");
    assert_eq!(temp.bundle.read("people/meera..notes.md").unwrap(), "fine");
}

#[test]
fn write_read_append_and_list_behave() {
    let temp = Temp::new("primitives");
    temp.bundle
        .write("people/meera.md", "one\n")
        .expect("write");
    temp.bundle
        .append("people/meera.md", "two\n")
        .expect("append");
    assert_eq!(temp.bundle.read("people/meera.md").unwrap(), "one\ntwo\n");

    temp.bundle.write("people/rahul.md", "x").expect("write");
    assert_eq!(temp.bundle.ls("people").unwrap(), ["meera.md", "rahul.md"]);
}

#[test]
fn appending_to_a_missing_file_creates_it() {
    let temp = Temp::new("append-new");
    temp.bundle
        .append("people/new.md", "first\n")
        .expect("append");
    assert_eq!(temp.bundle.read("people/new.md").unwrap(), "first\n");
}

/// Nothing may silently overwrite anything, so an edit that could hit two places is refused.
#[test]
fn an_ambiguous_edit_is_refused_rather_than_guessed() {
    let temp = Temp::new("ambiguous");
    temp.bundle
        .write("people/meera.md", "infra\ninfra\n")
        .expect("write");

    assert!(matches!(
        temp.bundle.edit("people/meera.md", "infra", "platform"),
        Err(BundleError::Ambiguous { .. })
    ));
    // Unchanged.
    assert_eq!(
        temp.bundle.read("people/meera.md").unwrap(),
        "infra\ninfra\n"
    );
}

#[test]
fn an_edit_with_no_match_is_an_error_not_a_no_op() {
    let temp = Temp::new("nomatch");
    temp.bundle
        .write("people/meera.md", "infra\n")
        .expect("write");
    assert!(matches!(
        temp.bundle.edit("people/meera.md", "platform", "infra"),
        Err(BundleError::NoMatch { .. })
    ));
}

#[test]
fn a_unique_edit_applies() {
    let temp = Temp::new("edit");
    temp.bundle
        .write("people/meera.md", "Works on platform\n")
        .expect("write");
    temp.bundle
        .edit("people/meera.md", "platform", "infra")
        .expect("edit");
    assert_eq!(
        temp.bundle.read("people/meera.md").unwrap(),
        "Works on infra\n"
    );
}

#[test]
fn grep_finds_lines_and_search_ranks_them() {
    let temp = Temp::new("search");
    temp.bundle
        .write("people/meera.md", "Works on the infra team\n")
        .expect("write");
    temp.bundle
        .write(
            "people/rahul.md",
            "Works on the platform team\nRuns infra reviews\n",
        )
        .expect("write");

    let hits = temp.bundle.grep("infra", None).expect("grep");
    assert_eq!(hits.len(), 2);

    // Two terms beat one, so meera's line ranks above rahul's.
    let ranked = temp.bundle.search("infra team").expect("search");
    assert_eq!(ranked[0].path, "people/meera.md");
}

#[test]
fn search_ignores_the_git_directory() {
    let temp = Temp::new("gitignore");
    temp.bundle.commit("first").expect("commit");
    // A term certain to appear in git's own files but not in memory.
    let hits = temp.bundle.grep("ref:", None).expect("grep");
    assert!(hits.iter().all(|h| !h.path.contains(".git")));
}

#[test]
fn a_concept_round_trips_through_the_bundle() {
    let temp = Temp::new("concept");
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);
    concept.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));

    temp.bundle
        .save_concept("people/meera.md", &concept)
        .expect("save");
    let loaded = temp.bundle.load_concept("people/meera.md").expect("load");
    assert_eq!(loaded, concept);
}

#[test]
fn concepts_excludes_scratch_and_generated_files() {
    let temp = Temp::new("listing");
    temp.bundle
        .write("people/meera.md", "---\nx\n---\n")
        .unwrap();
    temp.bundle
        .write("scratch/draft.md", "---\nx\n---\n")
        .unwrap();

    let concepts = temp.bundle.concepts().expect("concepts");
    assert_eq!(concepts, ["people/meera.md"]);
}

#[test]
fn committing_records_a_change_and_nothing_when_there_is_none() {
    let temp = Temp::new("git");
    temp.bundle
        .write("people/meera.md", "one\n")
        .expect("write");

    assert!(temp.bundle.commit("first").expect("commit"));
    assert_eq!(temp.bundle.commit_count().unwrap(), 1);

    // Nothing changed, so nothing to record. Not an error.
    assert!(!temp.bundle.commit("second").expect("commit"));
    assert_eq!(temp.bundle.commit_count().unwrap(), 1);

    temp.bundle
        .append("people/meera.md", "two\n")
        .expect("append");
    assert!(temp.bundle.commit("third").expect("commit"));
    assert_eq!(temp.bundle.commit_count().unwrap(), 2);
}

/// The bundle is the whole store, so copying the directory has to be enough to move machines.
#[test]
fn a_copied_bundle_carries_everything() {
    let temp = Temp::new("portable");
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);
    concept.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));
    temp.bundle
        .save_concept("people/meera.md", &concept)
        .unwrap();
    temp.bundle.commit("seed").unwrap();

    let copy_dir = temp.dir.with_extension("copy");
    let _ = std::fs::remove_dir_all(&copy_dir);
    let status = std::process::Command::new("cp")
        .args(["-R", temp.dir.to_str().unwrap(), copy_dir.to_str().unwrap()])
        .status()
        .expect("cp");
    assert!(status.success());

    let moved = Bundle::open(&copy_dir).expect("open the copy");
    assert_eq!(moved.load_concept("people/meera.md").unwrap(), concept);
    assert_eq!(moved.commit_count().unwrap(), 1);

    let _ = std::fs::remove_dir_all(&copy_dir);
}

// History. Not a feature: the timeline is the trust surface, and memory undo is a revert.

#[test]
fn history_reads_newest_first() {
    let temp = Temp::new("history");
    temp.bundle.write("people/meera.md", "platform\n").unwrap();
    temp.bundle.commit("learned: platform team").unwrap();
    temp.bundle.write("people/meera.md", "infra\n").unwrap();
    temp.bundle.commit("corrected: infra team").unwrap();

    let history = temp.bundle.history(10).expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].message, "corrected: infra team");
    assert_eq!(history[1].message, "learned: platform team");
    assert!(history[0].at >= history[1].at);
}

/// Commits written inside one second have identical timestamps, which a consolidation touching
/// several files will produce. Ordering must still be parent-last.
#[test]
fn history_is_ordered_even_when_commits_share_a_timestamp() {
    let temp = Temp::new("same-second");
    for n in 0..6 {
        temp.bundle
            .write("people/meera.md", &format!("version {n}\n"))
            .unwrap();
        temp.bundle.commit(&format!("commit {n}")).unwrap();
    }

    let history = temp.bundle.history(10).expect("history");
    let messages: Vec<&str> = history.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "commit 5", "commit 4", "commit 3", "commit 2", "commit 1", "commit 0"
        ]
    );
}

#[test]
fn history_on_an_empty_bundle_is_empty_not_an_error() {
    let temp = Temp::new("history-empty");
    assert!(temp.bundle.history(10).expect("history").is_empty());
    assert_eq!(temp.bundle.commit_count().unwrap(), 0);
}

/// One timeline of the present cannot answer this. Git can.
#[test]
fn a_file_can_be_read_as_it_stood_at_an_older_revision() {
    let temp = Temp::new("read-at");
    temp.bundle
        .write("people/meera.md", "Works on the platform team\n")
        .unwrap();
    temp.bundle.commit("march").unwrap();
    temp.bundle
        .write("people/meera.md", "Works on the infra team\n")
        .unwrap();
    temp.bundle.commit("august").unwrap();

    let history = temp.bundle.history(10).unwrap();
    let march = &history[1].id;

    assert_eq!(
        temp.bundle
            .read_at("people/meera.md", march)
            .unwrap()
            .trim(),
        "Works on the platform team"
    );
    assert_eq!(
        temp.bundle.read("people/meera.md").unwrap().trim(),
        "Works on the infra team"
    );
}

#[test]
fn a_concept_can_be_parsed_as_it_stood_at_a_revision() {
    let temp = Temp::new("concept-at");
    let mut front = Frontmatter::new("Meera", date(2026, 1, 1));
    front.status = Status::Stable;

    let mut old = RawConcept::new(front.clone());
    old.add(
        "Role",
        Claim::stated("Works on platform", date(2026, 3, 12)),
    );
    temp.bundle.save_concept("people/meera.md", &old).unwrap();
    temp.bundle.commit("march").unwrap();

    let mut new = RawConcept::new(front);
    new.add("Role", Claim::stated("Works on infra", date(2026, 7, 15)));
    temp.bundle.save_concept("people/meera.md", &new).unwrap();
    temp.bundle.commit("august").unwrap();

    let march = temp.bundle.history(10).unwrap()[1].id.clone();
    let then = temp
        .bundle
        .load_concept_at("people/meera.md", &march)
        .expect("load at revision");
    assert_eq!(then, old);
}

#[test]
fn a_revision_reports_what_it_changed() {
    let temp = Temp::new("changed");
    temp.bundle.write("people/meera.md", "one\n").unwrap();
    temp.bundle.commit("first").unwrap();
    temp.bundle.write("people/meera.md", "two\n").unwrap();
    temp.bundle.write("people/rahul.md", "new\n").unwrap();
    temp.bundle.commit("second").unwrap();

    let head = temp.bundle.history(1).unwrap()[0].id.clone();
    let mut changes = temp.bundle.changed_in(&head).expect("changed");
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].path, "people/meera.md");
    assert_eq!(changes[0].kind, ChangeKind::Modified);
    assert_eq!(changes[1].path, "people/rahul.md");
    assert_eq!(changes[1].kind, ChangeKind::Added);
}

/// Section 14.3. Undo is a compensating action appended, never a deletion.
#[test]
fn reverting_restores_the_content_and_keeps_the_history() {
    let temp = Temp::new("revert");
    temp.bundle
        .write("people/meera.md", "Works on the platform team\n")
        .unwrap();
    temp.bundle.commit("march").unwrap();
    temp.bundle
        .write("people/meera.md", "Works on the infra team\n")
        .unwrap();
    temp.bundle.commit("a wrong correction").unwrap();

    let bad = temp.bundle.history(1).unwrap()[0].id.clone();
    temp.bundle.revert(&bad).expect("revert");

    // Content is back.
    assert_eq!(
        temp.bundle.read("people/meera.md").unwrap().trim(),
        "Works on the platform team"
    );

    // And nothing was erased. Three commits, with the mistake still visible.
    let history = temp.bundle.history(10).unwrap();
    assert_eq!(history.len(), 3);
    assert!(history[0].message.starts_with("Revert"));
    assert_eq!(history[1].message, "a wrong correction");
}

#[test]
fn the_bundle_is_usable_again_after_a_revert() {
    let temp = Temp::new("revert-then-write");
    temp.bundle.write("people/meera.md", "one\n").unwrap();
    temp.bundle.commit("first").unwrap();
    temp.bundle.write("people/meera.md", "two\n").unwrap();
    temp.bundle.commit("second").unwrap();

    let second = temp.bundle.history(1).unwrap()[0].id.clone();
    temp.bundle.revert(&second).expect("revert");

    // A revert leaves git in a special state unless it is cleaned up. Prove it was.
    temp.bundle.write("people/meera.md", "three\n").unwrap();
    assert!(temp.bundle.commit("third").expect("commit after revert"));
    assert_eq!(temp.bundle.history(1).unwrap()[0].message, "third");
}
