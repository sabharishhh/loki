//! Git history, as the timeline and memory undo need it.
//!
//! Not a convenience. Section 17.3 backs the memory timeline on git history and calls it the trust
//! surface for the whole product, section 14.3 makes `git revert` the mechanism for memory undo,
//! and section 9.2 leans on a commit existing for every prior state as the protection against
//! losing entity files.
//!
//! So this reads history properly rather than parsing the output of a subprocess.

use git2::{Oid, Repository, Signature, Sort};
use jiff::Timestamp;

use super::bundle::BundleError;

/// A commit, identified by its full hex id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RevisionId(String);

impl RevisionId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first seven characters, for display.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(7)]
    }

    fn oid(&self) -> Result<Oid, BundleError> {
        Oid::from_str(&self.0).map_err(|e| BundleError::Git {
            operation: "parse revision".into(),
            detail: e.to_string(),
        })
    }
}

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One point in the bundle's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub id: RevisionId,
    pub message: String,
    pub at: Timestamp,
}

/// What happened to one file in a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

/// Opens the repository at a bundle root.
///
/// Opened per call rather than held, so a `Bundle` stays a cheap `Clone` that crosses threads.
/// Opening reads `.git` and is not on any hot path.
pub(super) fn open(root: &std::path::Path) -> Result<Repository, BundleError> {
    Repository::open(root).map_err(|e| BundleError::Git {
        operation: "open".into(),
        detail: e.to_string(),
    })
}

pub(super) fn git_err(operation: &str, e: &git2::Error) -> BundleError {
    BundleError::Git {
        operation: operation.to_owned(),
        detail: e.message().to_owned(),
    }
}

fn signature() -> Result<Signature<'static>, BundleError> {
    Signature::now("Loki", "loki@localhost").map_err(|e| git_err("signature", &e))
}

/// Initialises the repository if it is absent.
pub(super) fn init(root: &std::path::Path) -> Result<(), BundleError> {
    if root.join(".git").exists() {
        return Ok(());
    }
    Repository::init(root).map_err(|e| git_err("init", &e))?;
    Ok(())
}

/// Stages everything and commits, returning whether anything changed.
///
/// Nothing to commit is a normal outcome, not an error.
pub(super) fn commit(root: &std::path::Path, message: &str) -> Result<bool, BundleError> {
    let repo = open(root)?;
    let mut index = repo.index().map_err(|e| git_err("index", &e))?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| git_err("add", &e))?;
    index.write().map_err(|e| git_err("write index", &e))?;

    let tree_id = index.write_tree().map_err(|e| git_err("write tree", &e))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|e| git_err("find tree", &e))?;

    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    if let Some(parent) = &parent
        && parent.tree_id() == tree_id
    {
        return Ok(false);
    }

    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let sig = signature()?;
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(|e| git_err("commit", &e))?;
    Ok(true)
}

/// Commits newest first.
pub(super) fn history(root: &std::path::Path, limit: usize) -> Result<Vec<Revision>, BundleError> {
    let repo = open(root)?;
    if repo.head().is_err() {
        return Ok(Vec::new());
    }

    let mut walk = repo.revwalk().map_err(|e| git_err("revwalk", &e))?;
    walk.push_head().map_err(|e| git_err("push head", &e))?;
    // Topological as well as time. Commits made in the same second, which a consolidation
    // writing several files will produce, have identical timestamps and would otherwise come
    // back in an undefined order.
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(|e| git_err("sort", &e))?;

    let mut revisions = Vec::new();
    for id in walk.take(limit) {
        let Ok(id) = id else { continue };
        let Ok(commit) = repo.find_commit(id) else {
            continue;
        };
        revisions.push(Revision {
            id: RevisionId(id.to_string()),
            message: commit.summary().ok().flatten().unwrap_or("").to_owned(),
            at: Timestamp::from_second(commit.time().seconds()).unwrap_or(Timestamp::UNIX_EPOCH),
        });
    }
    Ok(revisions)
}

/// A file as it stood at a revision. Answers "what did Loki believe in July".
pub(super) fn read_at(
    root: &std::path::Path,
    path: &str,
    revision: &RevisionId,
) -> Result<String, BundleError> {
    let repo = open(root)?;
    let commit = repo
        .find_commit(revision.oid()?)
        .map_err(|e| git_err("find commit", &e))?;
    let tree = commit.tree().map_err(|e| git_err("tree", &e))?;
    let entry = tree
        .get_path(std::path::Path::new(path))
        .map_err(|_| BundleError::NotFound {
            path: format!("{path} at {}", revision.short()),
        })?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|e| git_err("blob", &e))?;
    Ok(String::from_utf8_lossy(blob.content()).into_owned())
}

/// What a revision changed. The correction pair renders from this.
pub(super) fn changed_in(
    root: &std::path::Path,
    revision: &RevisionId,
) -> Result<Vec<Change>, BundleError> {
    let repo = open(root)?;
    let commit = repo
        .find_commit(revision.oid()?)
        .map_err(|e| git_err("find commit", &e))?;
    let new = commit.tree().map_err(|e| git_err("tree", &e))?;
    let old = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let diff = repo
        .diff_tree_to_tree(old.as_ref(), Some(&new), None)
        .map_err(|e| git_err("diff", &e))?;

    Ok(diff
        .deltas()
        .filter_map(|delta| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())?;
            Some(Change {
                path: path.to_str()?.to_owned(),
                kind: match delta.status() {
                    git2::Delta::Added => ChangeKind::Added,
                    git2::Delta::Deleted => ChangeKind::Deleted,
                    git2::Delta::Renamed => ChangeKind::Renamed,
                    _ => ChangeKind::Modified,
                },
            })
        })
        .collect())
}

/// Undoes a revision by appending a commit that reverses it.
///
/// A compensating action, not a deletion. The reverted commit stays in history, which is what
/// makes the timeline able to show that something was undone rather than pretending it never
/// happened.
pub(super) fn revert(root: &std::path::Path, revision: &RevisionId) -> Result<(), BundleError> {
    let repo = open(root)?;
    let commit = repo
        .find_commit(revision.oid()?)
        .map_err(|e| git_err("find commit", &e))?;

    repo.revert(&commit, None)
        .map_err(|e| git_err("revert", &e))?;

    // git2 leaves the revert staged. Committing it is what makes it the compensating action.
    let mut index = repo.index().map_err(|e| git_err("index", &e))?;
    let tree_id = index.write_tree().map_err(|e| git_err("write tree", &e))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|e| git_err("find tree", &e))?;
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| git_err("head", &e))?;

    let sig = signature()?;
    let message = format!(
        "Revert \"{}\"\n\nThis reverts commit {revision}.",
        commit.summary().ok().flatten().unwrap_or("")
    );
    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&head])
        .map_err(|e| git_err("commit revert", &e))?;

    repo.cleanup_state().map_err(|e| git_err("cleanup", &e))?;
    Ok(())
}
