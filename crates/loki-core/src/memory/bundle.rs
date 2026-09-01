//! The memory bundle on disk.
//!
//! An OKF directory in a git repository. Files are the record; the index is derived and can be
//! thrown away. Git is what makes losing entity files a recoverable mistake rather than a real
//! one, which matters because entities cannot be rebuilt from episodes.
//!
//! Every path a caller supplies is resolved inside the bundle. A model writes these paths, so
//! escaping the root has to be impossible rather than merely discouraged.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::concept::{self, RawConcept};

/// The layout from section 9.3.
const DIRECTORIES: [&str; 4] = ["people", "projects", "preferences", "episodes"];
/// Agent-owned. Everything here is draft and nothing reaches a prompt.
pub const SCRATCH: &str = "scratch";
/// Chronological history. Feeds the timeline.
pub const LOG: &str = "log.md";
/// Generated, never hand-edited.
pub const WORKING_SET: &str = "working-set.md";
/// Session and persistent instructions.
pub const STANDING: &str = "standing.md";
pub const INDEX: &str = "index.md";

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

/// The bundle root and the operations allowed on it.
#[derive(Debug, Clone)]
pub struct Bundle {
    root: PathBuf,
}

impl Bundle {
    /// `~/Library/Application Support/Loki/memory`.
    ///
    /// # Errors
    /// Fails if there is no application support directory.
    pub fn default_root() -> Result<PathBuf, BundleError> {
        Ok(dirs::data_dir()
            .ok_or(BundleError::NoHome)?
            .join("Loki")
            .join("memory"))
    }

    /// Opens the bundle, creating the layout and the git repository if they are absent.
    ///
    /// # Errors
    /// Fails if the directories cannot be created or git refuses to initialise.
    pub fn open(root: &Path) -> Result<Self, BundleError> {
        let bundle = Self {
            root: root.to_path_buf(),
        };
        bundle.create_layout()?;
        bundle.init_git()?;
        Ok(bundle)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn create_layout(&self) -> Result<(), BundleError> {
        for dir in DIRECTORIES.iter().chain(std::iter::once(&SCRATCH)) {
            let path = self.root.join(dir);
            std::fs::create_dir_all(&path).map_err(|source| BundleError::Io {
                path: dir.to_string(),
                source,
            })?;
        }

        // index.md carries the OKF version, so a consumer knows what it is reading.
        let index = self.root.join(INDEX);
        if !index.exists() {
            self.write(INDEX, "---\nokf_version: '0.2'\n---\n\n# Loki memory\n")?;
        }
        for file in [LOG, WORKING_SET, STANDING] {
            if !self.root.join(file).exists() {
                self.write(file, "")?;
            }
        }
        Ok(())
    }

    /// Resolves a caller-supplied path inside the bundle.
    ///
    /// Rejects absolute paths, `..`, and anything that would land outside the root. A model writes
    /// these, so this is a boundary, not a convenience.
    fn resolve(&self, path: &str) -> Result<PathBuf, BundleError> {
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
        Ok(self.root.join(candidate))
    }
}

/// The seven primitives.
///
/// Ordinary file operations, the same shapes used on a user's documents. One tool surface, two
/// scopes. All are Tier 1 or 2, because a memory write is reversible by git.
impl Bundle {
    /// Reads a file.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle or the file is missing.
    pub fn read(&self, path: &str) -> Result<String, BundleError> {
        let full = self.resolve(path)?;
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

    /// Creates or overwrites a file, making parent directories as needed.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle or the write fails.
    pub fn write(&self, path: &str, content: &str) -> Result<(), BundleError> {
        let full = self.resolve(path)?;
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

    /// Lists a directory, relative to the bundle root, sorted.
    ///
    /// # Errors
    /// Fails if the path escapes the bundle or the directory is missing.
    pub fn ls(&self, dir: &str) -> Result<Vec<String>, BundleError> {
        let full = self.resolve(dir)?;
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

    /// Literal substring search across the bundle.
    ///
    /// # Errors
    /// Fails if `within` escapes the bundle.
    pub fn grep(&self, pattern: &str, within: Option<&str>) -> Result<Vec<Hit>, BundleError> {
        let start = within.unwrap_or(".");
        self.resolve(start)?;
        let mut hits = Vec::new();
        for path in self.markdown_files(start)? {
            let Ok(text) = self.read(&path) else { continue };
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

    /// Ranked search, for when grep is too literal.
    ///
    /// Case-insensitive term matching, ranked by how many query terms a line carries. The FTS5
    /// index replaces this backend in 2c; the shape stays.
    ///
    /// # Errors
    /// Fails if the bundle cannot be walked.
    pub fn search(&self, query: &str) -> Result<Vec<Hit>, BundleError> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|t| t.len() > 1)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(usize, Hit)> = Vec::new();
        for path in self.markdown_files(".")? {
            let Ok(text) = self.read(&path) else { continue };
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

    /// Every markdown file under a directory, as bundle-relative paths.
    fn markdown_files(&self, dir: &str) -> Result<Vec<String>, BundleError> {
        let root = self.resolve(dir)?;
        let mut found = Vec::new();
        walk(&root, &self.root, &mut found);
        found.sort_unstable();
        Ok(found)
    }
}

fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        // `.git` is machinery, not memory.
        if name.to_str().is_some_and(|n| n.starts_with('.')) {
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

/// Concepts, and the git repository holding them.
impl Bundle {
    /// Reads and parses a concept.
    ///
    /// # Errors
    /// Fails if the file is missing or is not a valid OKF document.
    pub fn load_concept(&self, path: &str) -> Result<RawConcept, BundleError> {
        let text = self.read(path)?;
        concept::parse(&text).map_err(|source| BundleError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Writes a concept back as markdown.
    ///
    /// # Errors
    /// Fails if the write fails.
    pub fn save_concept(&self, path: &str, concept: &RawConcept) -> Result<(), BundleError> {
        self.write(path, &concept::render(concept))
    }

    /// Every concept path, excluding scratch and the generated files.
    ///
    /// # Errors
    /// Fails if the bundle cannot be walked.
    pub fn concepts(&self) -> Result<Vec<String>, BundleError> {
        let generated = [INDEX, LOG, WORKING_SET, STANDING];
        Ok(self
            .markdown_files(".")?
            .into_iter()
            .filter(|p| !p.starts_with(SCRATCH) && !generated.contains(&p.as_str()))
            .collect())
    }

    fn init_git(&self) -> Result<(), BundleError> {
        if self.root.join(".git").exists() {
            return Ok(());
        }
        self.git(&["init", "--quiet"])?;
        // Local identity, so a commit never depends on the user's global git config.
        self.git(&["config", "user.name", "Loki"])?;
        self.git(&["config", "user.email", "loki@localhost"])?;
        Ok(())
    }

    /// Stages everything and commits, if anything changed.
    ///
    /// Returns whether a commit was made. Nothing to commit is a normal outcome, not an error.
    ///
    /// # Errors
    /// Fails if git refuses.
    pub fn commit(&self, message: &str) -> Result<bool, BundleError> {
        self.git(&["add", "-A"])?;
        if self.git(&["diff", "--cached", "--quiet"]).is_ok() {
            return Ok(false);
        }
        self.git(&["commit", "--quiet", "-m", message])?;
        Ok(true)
    }

    /// How many commits the bundle has.
    ///
    /// # Errors
    /// Fails if git refuses.
    pub fn commit_count(&self) -> Result<usize, BundleError> {
        match self.git(&["rev-list", "--count", "HEAD"]) {
            Ok(out) => Ok(out.trim().parse().unwrap_or(0)),
            // No commits yet is not a failure.
            Err(_) => Ok(0),
        }
    }

    /// Runs git in the bundle. Shelling out rather than linking libgit2: the surface is init, add,
    /// commit, diff and revert, and git is present on any Mac with the developer tools.
    fn git(&self, args: &[&str]) -> Result<String, BundleError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|e| BundleError::Git {
                operation: args.first().unwrap_or(&"?").to_string(),
                detail: e.to_string(),
            })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(BundleError::Git {
                operation: args.first().unwrap_or(&"?").to_string(),
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}
