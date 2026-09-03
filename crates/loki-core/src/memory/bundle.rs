//! The memory bundle on disk.
//!
//! An OKF directory in a git repository. Files are the record; the index is derived and can be
//! thrown away. Git is what makes losing entity files a recoverable mistake rather than a real
//! one, which matters because entities cannot be rebuilt from episodes.
//!
//! Every path a caller supplies is resolved inside the bundle. A model writes these paths, so
//! escaping the root has to be impossible rather than merely discouraged.
//!
//! Section 7.2: any number of concurrent readers, or one writer, never both. That is enforced by
//! which type you hold rather than by remembering. [`Bundle::reader`] hands out a [`Reader`] and
//! [`Bundle::writer`] a [`Writer`], each holding its guard for as long as the handle lives. A
//! consolidation rewriting many files takes one writer for the whole pass, so nothing observes it
//! half-done.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::concept::{self, RawConcept};
use super::history::{self, Change, Revision, RevisionId};

/// The layout from section 9.3.
const DIRECTORIES: [&str; 4] = ["people", "projects", "preferences", "episodes"];
/// The session buffer (§9.3). Everything in it is a candidate, and it is cleared on consolidation.
///
/// Replaced `scratch/` in v0.9. A directory of draft files that nothing could read into a prompt
/// produced a hole: promotion "on use without correction" could never fire, because a draft was
/// never retrievable. One append-only file readable during the session removes a directory, a
/// status transition, and that whole class of bug.
///
/// It is also what stops re-extraction. Consolidation reads the buffer rather than the episode, so
/// a second close in one session sees only what was said since the first. Reading the episode
/// meant every close re-extracted the whole day and the extractor, being a model, worded each fact
/// differently every time.
pub const CURRENT: &str = "current.md";
/// Chronological history. Feeds the timeline.
pub const LOG: &str = "log.md";
/// Generated, never hand-edited.
pub const WORKING_SET: &str = "working-set.md";
/// Session and persistent instructions.
pub const STANDING: &str = "standing.md";
pub const INDEX: &str = "index.md";

/// The owner's card. Seeded before the first turn, so an "I" always has somewhere to land (§9.4).
///
/// The path is fixed and the name is not: it opens as "you" and is renamed the moment a name is
/// learned. §11.3's import depends on this existing beforehand, or every export writes a new
/// person for the same "I".
pub const OWNER: &str = "people/you.md";

/// Loki's own card, so a fact about the assistant is not a fact about a person it knows.
pub const ASSISTANT: &str = "people/loki.md";

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("no application support directory")]
    NoHome,
    #[error("{path} escapes the memory bundle")]
    OutsideBundle { path: String },
    #[error("{path} not found")]
    NotFound { path: String },
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: concept::ParseError,
    },
    #[error("git {operation} failed: {detail}")]
    Git { operation: String, detail: String },
    #[error("{path} does not contain the text to replace")]
    NoMatch { path: String },
    #[error("{path} contains that text more than once, so the edit is ambiguous")]
    Ambiguous { path: String },
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// The bundle root, and the lock deciding who may touch it.
///
/// Cheap to clone. Clones share the lock, which is the point: two handles to one bundle must
/// exclude each other.
#[derive(Debug, Clone)]
pub struct Bundle {
    root: PathBuf,
    lock: Arc<RwLock<()>>,
}

impl Bundle {
    /// `~/Library/Application Support/Loki/memory`.
    ///
    /// # Errors
    /// Fails if there is no application support directory.
    pub fn default_root() -> Result<PathBuf, BundleError> {
        crate::paths::memory().map_err(|_| BundleError::NoHome)
    }

    /// Opens the bundle, creating the layout and the git repository if absent.
    ///
    /// # Errors
    /// Fails if the directories cannot be created or git refuses to initialise.
    pub async fn open(root: &Path) -> Result<Self, BundleError> {
        let bundle = Self {
            root: root.to_path_buf(),
            lock: Arc::new(RwLock::new(())),
        };
        {
            let writer = bundle.writer().await;
            writer.create_layout()?;
        }
        history::init(root)?;
        Ok(bundle)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Takes a read guard. Any number may be held at once.
    pub async fn reader(&self) -> Reader<'_> {
        Reader {
            root: &self.root,
            _guard: self.lock.read().await,
        }
    }

    /// Takes the write guard. Excludes every reader while it is held.
    ///
    /// Hold one across a whole consolidation rather than per file, or a reader can land between
    /// two writes and see a bundle that was never true.
    pub async fn writer(&self) -> Writer<'_> {
        Writer {
            root: &self.root,
            _guard: self.lock.write().await,
        }
    }
}

/// Read access. Several may exist at once.
#[derive(Debug)]
pub struct Reader<'a> {
    root: &'a Path,
    _guard: RwLockReadGuard<'a, ()>,
}

/// Write access. Exactly one may exist, and no reader may exist alongside it.
#[derive(Debug)]
pub struct Writer<'a> {
    root: &'a Path,
    _guard: RwLockWriteGuard<'a, ()>,
}

/// Resolves a caller-supplied path inside the bundle.
///
/// Rejects absolute paths, `..`, and anything landing outside the root. A model writes these, so
/// this is a boundary, not a convenience.
fn resolve(root: &Path, path: &str) -> Result<PathBuf, BundleError> {
    let candidate = Path::new(path);
    let outside = || BundleError::OutsideBundle {
        path: path.to_owned(),
    };

    if candidate.is_absolute() {
        return Err(outside());
    }
    for part in candidate.components() {
        match part {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(outside());
            }
        }
    }
    Ok(root.join(candidate))
}

fn read_file(root: &Path, path: &str) -> Result<String, BundleError> {
    let full = resolve(root, path)?;
    std::fs::read_to_string(&full).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            BundleError::NotFound {
                path: path.to_owned(),
            }
        } else {
            BundleError::Io {
                path: path.to_owned(),
                source,
            }
        }
    })
}

fn write_file(root: &Path, path: &str, content: &str) -> Result<(), BundleError> {
    let full = resolve(root, path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BundleError::Io {
            path: path.to_owned(),
            source,
        })?;
    }
    std::fs::write(&full, content).map_err(|source| BundleError::Io {
        path: path.to_owned(),
        source,
    })
}

fn list(root: &Path, dir: &str) -> Result<Vec<String>, BundleError> {
    let full = resolve(root, dir)?;
    let entries = std::fs::read_dir(&full).map_err(|source| BundleError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_unstable();
    Ok(names)
}

fn markdown_files(root: &Path, dir: &str) -> Result<Vec<String>, BundleError> {
    let start = resolve(root, dir)?;
    let mut found = Vec::new();
    walk(&start, root, &mut found);
    found.sort_unstable();
    Ok(found)
}

fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `.git` is machinery, not memory.
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            walk(&path, root, found);
        } else if path.extension().is_some_and(|e| e == "md")
            && let Ok(relative) = path.strip_prefix(root)
            && let Some(text) = relative.to_str()
        {
            found.push(text.to_owned());
        }
    }
}

fn grep_in(root: &Path, pattern: &str, within: Option<&str>) -> Result<Vec<Hit>, BundleError> {
    let start = within.unwrap_or(".");
    let mut hits = Vec::new();
    for path in markdown_files(root, start)? {
        let Ok(text) = read_file(root, &path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                hits.push(Hit {
                    path: path.clone(),
                    line: number + 1,
                    text: line.trim().to_owned(),
                });
            }
        }
    }
    Ok(hits)
}

fn search_in(root: &Path, query: &str) -> Result<Vec<Hit>, BundleError> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| t.len() > 1)
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored: Vec<(usize, Hit)> = Vec::new();
    for path in markdown_files(root, ".")? {
        let Ok(text) = read_file(root, &path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            let lowered = line.to_lowercase();
            let score = terms.iter().filter(|t| lowered.contains(*t)).count();
            if score > 0 {
                scored.push((
                    score,
                    Hit {
                        path: path.clone(),
                        line: number + 1,
                        text: line.trim().to_owned(),
                    },
                ));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    Ok(scored.into_iter().map(|(_, hit)| hit).collect())
}

fn concept_paths(root: &Path) -> Result<Vec<String>, BundleError> {
    let generated = [INDEX, LOG, WORKING_SET, STANDING, CURRENT];
    Ok(markdown_files(root, ".")?
        .into_iter()
        .filter(|p| !generated.contains(&p.as_str()))
        .collect())
}

fn parse_concept(path: &str, text: &str) -> Result<RawConcept, BundleError> {
    concept::parse(text).map_err(|source| BundleError::Parse {
        path: path.to_owned(),
        source,
    })
}

/// Everything a reader can do. A writer can do all of it too.
macro_rules! read_ops {
    () => {
        /// Reads a file.
        ///
        /// # Errors
        /// Fails if the path escapes the bundle or the file is missing.
        pub fn read(&self, path: &str) -> Result<String, BundleError> {
            read_file(self.root, path)
        }

        /// Lists a directory, sorted, relative to the bundle root.
        ///
        /// # Errors
        /// Fails if the path escapes the bundle or the directory is missing.
        pub fn ls(&self, dir: &str) -> Result<Vec<String>, BundleError> {
            list(self.root, dir)
        }

        /// Literal substring search.
        ///
        /// # Errors
        /// Fails if `within` escapes the bundle.
        pub fn grep(&self, pattern: &str, within: Option<&str>) -> Result<Vec<Hit>, BundleError> {
            grep_in(self.root, pattern, within)
        }

        /// Ranked search, for when grep is too literal.
        ///
        /// Term-count ranking for now. The FTS5 index replaces the backend in 2c; the shape stays.
        ///
        /// # Errors
        /// Fails if the bundle cannot be walked.
        pub fn search(&self, query: &str) -> Result<Vec<Hit>, BundleError> {
            search_in(self.root, query)
        }

        /// Reads and parses a concept.
        ///
        /// # Errors
        /// Fails if the file is missing or is not a valid OKF document.
        pub fn load_concept(&self, path: &str) -> Result<RawConcept, BundleError> {
            parse_concept(path, &read_file(self.root, path)?)
        }

        /// Every concept path, excluding scratch and the generated files.
        ///
        /// # Errors
        /// Fails if the bundle cannot be walked.
        pub fn concepts(&self) -> Result<Vec<String>, BundleError> {
            concept_paths(self.root)
        }

        /// Concept paths under `scratch/`, which are all `draft` and never reach a prompt.
        ///
        /// The bundle root, for callers that need to stat a file rather than read it.
        pub const fn root(&self) -> &Path {
            self.root
        }

        /// Commits newest first. What the memory timeline is built from.
        ///
        /// # Errors
        /// Fails if the history cannot be read.
        pub fn history(&self, limit: usize) -> Result<Vec<Revision>, BundleError> {
            history::history(self.root, limit)
        }

        /// How many commits the bundle has.
        ///
        /// # Errors
        /// Fails if the history cannot be read.
        pub fn commit_count(&self) -> Result<usize, BundleError> {
            Ok(history::history(self.root, usize::MAX)?.len())
        }

        /// A file as it stood at a revision.
        ///
        /// Answers what Loki believed at a point in time, which one timeline of the present
        /// cannot.
        ///
        /// # Errors
        /// Fails if the revision or the path is unknown.
        pub fn read_at(&self, path: &str, revision: &RevisionId) -> Result<String, BundleError> {
            resolve(self.root, path)?;
            history::read_at(self.root, path, revision)
        }

        /// A concept as it stood at a revision.
        ///
        /// # Errors
        /// Fails if the revision is unknown or the file does not parse.
        pub fn load_concept_at(
            &self,
            path: &str,
            revision: &RevisionId,
        ) -> Result<RawConcept, BundleError> {
            parse_concept(path, &self.read_at(path, revision)?)
        }

        /// What a revision changed.
        ///
        /// # Errors
        /// Fails if the revision is unknown.
        pub fn changed_in(&self, revision: &RevisionId) -> Result<Vec<Change>, BundleError> {
            history::changed_in(self.root, revision)
        }

        /// The current head, for a checkpoint to record what memory looked like.
        ///
        /// `None` on a bundle with no commits.
        ///
        /// # Errors
        /// Fails if the history cannot be read.
        pub fn snapshot(&self) -> Result<Option<crate::core::ids::SnapshotId>, BundleError> {
            Ok(history::history(self.root, 1)?
                .first()
                .map(|r| crate::core::ids::SnapshotId::new(r.id.as_str())))
        }
    };
}

impl Reader<'_> {
    read_ops!();
}

impl Writer<'_> {
    read_ops!();

    fn create_layout(&self) -> Result<(), BundleError> {
        for dir in &DIRECTORIES {
            std::fs::create_dir_all(self.root.join(dir)).map_err(|source| BundleError::Io {
                path: (*dir).to_string(),
                source,
            })?;
        }
        if !self.root.join(INDEX).exists() {
            self.write(INDEX, "---\nokf_version: '0.2'\n---\n\n# Loki memory\n")?;
        }
        for file in [LOG, WORKING_SET, STANDING, CURRENT] {
            if !self.root.join(file).exists() {
                self.write(file, "")?;
            }
        }
        Ok(())
    }

    /// Creates or overwrites a file, making parent directories as needed.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle or the write fails.
    pub fn write(&self, path: &str, content: &str) -> Result<(), BundleError> {
        write_file(self.root, path, content)
    }

    /// Deletes a file.
    ///
    /// Not one of §10.3's seven primitives and deliberately not exposed to the model: the only
    /// caller is consolidation clearing the scratch sources it promoted, so the directory listing
    /// matches what is live (§9.8). Git history keeps the content either way.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle. A file that is already gone is not an error.
    pub fn remove(&self, path: &str) -> Result<(), BundleError> {
        let full = resolve(self.root, path)?;
        match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(BundleError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Appends to a file, creating it if absent.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle or the write fails.
    pub fn append(&self, path: &str, content: &str) -> Result<(), BundleError> {
        let existing = match self.read(path) {
            Ok(text) => text,
            Err(BundleError::NotFound { .. }) => String::new(),
            Err(e) => return Err(e),
        };
        self.write(path, &(existing + content))
    }

    /// Replaces one occurrence of `old` with `new`.
    ///
    /// # Errors
    /// Fails if the text is absent, or present more than once. An ambiguous edit is refused rather
    /// than guessed, because nothing may silently overwrite anything.
    pub fn edit(&self, path: &str, old: &str, new: &str) -> Result<(), BundleError> {
        let text = self.read(path)?;
        match text.matches(old).count() {
            0 => Err(BundleError::NoMatch {
                path: path.to_owned(),
            }),
            1 => self.write(path, &text.replacen(old, new, 1)),
            _ => Err(BundleError::Ambiguous {
                path: path.to_owned(),
            }),
        }
    }

    /// Writes a concept back as markdown.
    ///
    /// # Errors
    /// Fails if the write fails.
    pub fn save_concept(&self, path: &str, concept: &RawConcept) -> Result<(), BundleError> {
        self.write(path, &concept::render(concept))
    }

    /// Stages everything and commits, if anything changed.
    ///
    /// Returns whether a commit was made. Nothing to commit is a normal outcome, not an error.
    ///
    /// # Errors
    /// Fails if git refuses.
    pub fn commit(&self, message: &str) -> Result<bool, BundleError> {
        history::commit(self.root, message)
    }

    /// Undoes a revision by appending a commit that reverses it.
    ///
    /// Section 14.3: memory undo is git revert on a consolidation commit. A compensating action,
    /// never a deletion, so the timeline can still show that something was undone.
    ///
    /// # Errors
    /// Fails if the revision is unknown or the revert conflicts.
    pub fn revert(&self, revision: &RevisionId) -> Result<(), BundleError> {
        history::revert(self.root, revision)
    }

    /// Throws away everything written since the last commit (§9.8 step 5).
    ///
    /// # Errors
    /// Fails if the repository cannot be reset.
    pub fn discard_changes(&self) -> Result<(), BundleError> {
        history::discard_changes(self.root)
    }
}
