//! Where Loki keeps things.
//!
//! One owner for the layout in section 9.3. Each subsystem computing its own path is how a
//! directory ends up in two places after a rename.

use std::path::PathBuf;

use crate::error::Error;

/// `~/Library/Application Support/Loki`.
///
/// # Errors
/// Fails if there is no application support directory.
pub fn root() -> Result<PathBuf, Error> {
    dirs::data_dir()
        .map(|base| base.join("Loki"))
        .ok_or_else(|| {
            Error::Runtime(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no application support directory",
            ))
        })
}

/// The OKF bundle. A git repository.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn memory() -> Result<PathBuf, Error> {
    Ok(root()?.join("memory"))
}

/// Derived, disposable, rebuilt from the bundle whenever it is missing or inconsistent.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn index() -> Result<PathBuf, Error> {
    Ok(root()?.join("index.sqlite"))
}

/// Content-addressed fetched pages, with a TTL.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn evidence() -> Result<PathBuf, Error> {
    Ok(root()?.join("evidence"))
}

/// Loki's own browser profile, never the user's (§12.3).
///
/// # Errors
/// Fails if the directory cannot be found.
pub fn browser_profile() -> Result<PathBuf, Error> {
    Ok(root()?.join("browser"))
}

/// The undo journal and its staged originals.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn undo() -> Result<PathBuf, Error> {
    Ok(root()?.join("undo"))
}

/// Per-tool capability grants.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn grants() -> Result<PathBuf, Error> {
    Ok(root()?.join("tools").join("grants.toml"))
}

/// Every model call, tool call, search and extraction.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn ledger() -> Result<PathBuf, Error> {
    Ok(root()?.join("ledger.sqlite"))
}

/// The session journal: every prompt, reply and memory event, in the order they happened.
///
/// Outside `memory/` on purpose. It is a diagnostic, not part of the OKF bundle, and a transcript
/// inside the bundle would be committed to the memory repo and read as a concept.
///
/// # Errors
/// Fails if the root cannot be found.
pub fn journal() -> Result<PathBuf, Error> {
    Ok(root()?.join("loki.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every path the document names, and all of them under one root.
    #[test]
    fn the_layout_matches_section_9_3() {
        let root = root().expect("root");
        for path in [
            memory().unwrap(),
            index().unwrap(),
            evidence().unwrap(),
            undo().unwrap(),
            grants().unwrap(),
            ledger().unwrap(),
        ] {
            assert!(path.starts_with(&root), "{path:?} is outside the root");
        }

        assert!(memory().unwrap().ends_with("memory"));
        assert!(index().unwrap().ends_with("index.sqlite"));
        assert!(evidence().unwrap().ends_with("evidence"));
        assert!(undo().unwrap().ends_with("undo"));
        assert!(grants().unwrap().ends_with("tools/grants.toml"));
        assert!(ledger().unwrap().ends_with("ledger.sqlite"));
    }

    /// Credentials are in the Keychain, not this tree. Section 9.3 says so explicitly.
    #[test]
    fn nothing_here_is_a_credential_store() {
        let named: Vec<String> = [memory(), index(), evidence(), undo(), grants(), ledger()]
            .into_iter()
            .filter_map(|p| Some(p.ok()?.display().to_string().to_lowercase()))
            .collect();
        for path in named {
            assert!(!path.contains("secret"), "{path} looks like a secret store");
            assert!(!path.contains("keychain"), "{path} duplicates the Keychain");
        }
    }
}
