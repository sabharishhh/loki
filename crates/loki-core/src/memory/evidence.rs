//! Where fetched content lives (§12.7, S2).
//!
//! **Beside the bundle, not inside it.** A claim citing a page has to be able to reach the page, and
//! the obvious place is the git-tracked bundle. That is wrong: every fetched page would become a git
//! object and the history would grow without bound, which contradicts §9.2's episode log being small
//! and permanent. So this sits next to `index.sqlite`, derived and disposable for the same reason.
//!
//! **The split §12.7 asks for.** The immutable fact is that a URL was fetched at a time and yielded
//! content with a given hash, and that fact lives in the episode where it is permanent and tiny. The
//! bytes are a cache: bulky, ageing, and re-fetchable. A claim can therefore cite content the store
//! no longer holds, which is the honest tradeoff rather than a gap.
//!
//! **Content-addressed, written once.** Two claims citing one page share one file, and a page whose
//! bytes changed is a different hash rather than an overwrite, so a citation can never silently come
//! to point at different content than it was written against.

use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid};

use crate::core::ids::ContentHash;

/// Where the URL pointers live, kept out of the content shards.
const BY_URL: &str = "by-url";

/// The content store.
#[derive(Debug, Clone)]
pub struct Evidence {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("the evidence store could not be opened: {0}")]
    Unusable(String),
    #[error("could not write evidence: {0}")]
    Write(String),
}

impl Evidence {
    /// Opens, or creates, the store beside the bundle.
    ///
    /// # Errors
    /// Fails if the directory cannot be made.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, EvidenceError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| EvidenceError::Unusable(e.to_string()))?;
        Ok(Self { root })
    }

    /// The default location, `~/Library/Application Support/Loki/evidence`.
    ///
    /// # Errors
    /// Fails if the directory cannot be found or made.
    pub fn default_location() -> Result<Self, EvidenceError> {
        let root = crate::paths::evidence().map_err(|e| EvidenceError::Unusable(e.to_string()))?;
        Self::open(root)
    }

    /// Stores content and returns what to cite it by.
    ///
    /// Writing the same bytes twice is one file and one hash, which is what lets two claims citing
    /// one page share it without either knowing about the other.
    ///
    /// # Errors
    /// Fails if the content cannot be written.
    pub fn put(&self, content: &[u8]) -> Result<ContentHash, EvidenceError> {
        let hash = hash_of(content);
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EvidenceError::Write(e.to_string()))?;
        }
        // Written whole through a temporary, so a crash between the two leaves no file rather than
        // a short one. A truncated file under a hash that promises its contents is worse than a
        // missing one, because nothing would ever check.
        let temporary = path.with_extension("part");
        std::fs::write(&temporary, content).map_err(|e| EvidenceError::Write(e.to_string()))?;
        std::fs::rename(&temporary, &path).map_err(|e| EvidenceError::Write(e.to_string()))?;
        Ok(hash)
    }

    /// Reads content back, or `None` if the store no longer holds it.
    ///
    /// **`None` is expected, not exceptional.** The bytes are a cache with a lifetime shorter than a
    /// claim's, so a citation outliving its content is ordinary and the caller says "I read this at
    /// the time and no longer have it" rather than treating it as an error.
    #[must_use]
    pub fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        std::fs::read(self.path_for(hash)).ok()
    }

    #[must_use]
    pub fn holds(&self, hash: &ContentHash) -> bool {
        self.path_for(hash).exists()
    }

    /// Drops everything nothing cites any more.
    ///
    /// **On consolidation, never on a timer.** §7.1 is one process and §14.5 forbids a poll.
    /// Consolidation already runs at session close, already commits, and already knows which claims
    /// survived, so it is the only place that can tell what is unreferenced without a second
    /// traversal of memory.
    ///
    /// # Errors
    /// Fails if the store cannot be read.
    pub fn sweep(&self, cited: &[ContentHash]) -> Result<usize, EvidenceError> {
        let keep: std::collections::HashSet<&str> = cited.iter().map(ContentHash::as_str).collect();
        let mut dropped = 0;
        let shards =
            std::fs::read_dir(&self.root).map_err(|e| EvidenceError::Unusable(e.to_string()))?;
        for shard in shards.flatten() {
            // Pointers are not content and are not cited by anything. They expire by age, which is
            // the cache's business, not the citation sweep's. A body the sweep does drop is a
            // silent cache miss and one extra fetch, never a wrong answer.
            if shard.file_name() == std::ffi::OsStr::new(BY_URL) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(stem) = name.to_str() else { continue };
                let shard_name = shard.file_name();
                let prefix = shard_name.to_str().unwrap_or("");
                if !keep.contains(format!("{prefix}{stem}").as_str())
                    && std::fs::remove_file(entry.path()).is_ok()
                {
                    dropped += 1;
                }
            }
        }
        Ok(dropped)
    }

    /// Two characters of the hash as a directory, the rest as the name.
    ///
    /// A flat directory of tens of thousands of files is slow to list on every filesystem that    /// Remembers that a URL was fetched and what it yielded.
    ///
    /// **The content stays content-addressed and only a pointer is keyed by URL.** Two URLs that
    /// serve the same bytes still share one file, and a page whose bytes changed is a new hash
    /// rather than an overwrite, so a citation can never come to point at content it was not
    /// written against. What is added here is the one thing a cache needs and content addressing
    /// cannot answer: "have I fetched *this address* lately".
    ///
    /// # Errors
    /// Fails if the content or the pointer cannot be written.
    pub fn remember(&self, url: &str, content: &[u8]) -> Result<ContentHash, EvidenceError> {
        let hash = self.put(content)?;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let pointer = self.pointer_for(url);
        if let Some(parent) = pointer.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EvidenceError::Write(e.to_string()))?;
        }
        std::fs::write(&pointer, format!("{} {at}", hash.as_str()))
            .map_err(|e| EvidenceError::Write(e.to_string()))?;
        Ok(hash)
    }

    /// What this URL yielded, if it was fetched inside `fresh_for` and the content is still held.
    ///
    /// **A miss is silent and always safe.** Every failure here, an unreadable pointer, a swept
    /// body, a clock that went backwards, comes back as `None` and costs one fetch. A cache that
    /// returned something doubtful would be worse than no cache at all, because §21.5's whole
    /// argument is that stale content is indistinguishable from fresh once it is in a prompt.
    #[must_use]
    pub fn recall(&self, url: &str, fresh_for: std::time::Duration) -> Option<Vec<u8>> {
        let pointer = std::fs::read_to_string(self.pointer_for(url)).ok()?;
        let (hash, at) = pointer.split_once(' ')?;
        let at: u64 = at.trim().parse().ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        // A pointer from the future is a clock that moved, not a fresh page.
        let age = now.checked_sub(at)?;
        // Strictly inside the window. A zero-length window has to mean "never from cache", and
        // `<=` served a page written the same second.
        (age < fresh_for.as_secs()).then(|| self.get(&ContentHash::new(hash)))?
    }

    /// Where the pointer for a URL lives. Hashed so a URL's own characters never become a path.
    fn pointer_for(&self, url: &str) -> PathBuf {
        let hash = hash_of(url.as_bytes());
        let hex = hash.as_str();
        let (shard, rest) = hex.split_at(2.min(hex.len()));
        self.root.join(BY_URL).join(shard).join(rest)
    }

    /// matters, and git has used this shape for the same reason for twenty years.
    fn path_for(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.as_str();
        let (shard, rest) = hex.split_at(hex.len().min(2));
        self.root.join(shard).join(rest)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// The hash a piece of content is stored and cited under.
///
/// SHA-1 through `git2`, which is already a dependency because history is a core surface. A crate
/// for a hash we already link a hasher for would be a dependency bought twice.
#[must_use]
pub fn hash_of(content: &[u8]) -> ContentHash {
    let oid = Oid::hash_object(ObjectType::Blob, content)
        .map_or_else(|_| String::new(), |oid| oid.to_string());
    ContentHash::new(oid)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(what: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("loki-evidence-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }

        fn store(&self) -> Evidence {
            Evidence::open(&self.0).expect("store")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_page_fetched_lately_comes_back_without_fetching_it() {
        let dir = Scratch::new("cache");
        let store = dir.store();
        store
            .remember("https://example.com/a", b"<html>a</html>")
            .expect("remember");
        assert_eq!(
            store.recall("https://example.com/a", Duration::from_secs(900)),
            Some(b"<html>a</html>".to_vec())
        );
    }

    /// The whole point of the TTL: yesterday's page must not answer today's question.
    #[test]
    fn a_page_older_than_the_window_is_a_miss() {
        let dir = Scratch::new("cache-stale");
        let store = dir.store();
        store
            .remember("https://example.com/b", b"old")
            .expect("remember");
        assert_eq!(store.recall("https://example.com/b", Duration::ZERO), None);
    }

    #[test]
    fn a_url_never_fetched_is_a_miss_and_not_an_error() {
        let dir = Scratch::new("cache-cold");
        assert_eq!(
            dir.store()
                .recall("https://example.com/never", Duration::from_secs(900)),
            None
        );
    }

    /// A URL whose body the sweep dropped reads as never fetched, which costs one fetch and
    /// never a wrong answer.
    #[test]
    fn a_pointer_to_swept_content_is_a_miss() {
        let dir = Scratch::new("cache-swept");
        let store = dir.store();
        store
            .remember("https://example.com/c", b"body")
            .expect("remember");
        store.sweep(&[]).expect("sweep");
        assert_eq!(
            store.recall("https://example.com/c", Duration::from_secs(900)),
            None
        );
    }

    /// Two addresses serving identical bytes still share one file, which is what content
    /// addressing is for and what a URL-keyed cache must not undo.
    #[test]
    fn two_urls_with_the_same_body_store_it_once() {
        let dir = Scratch::new("cache-shared");
        let store = dir.store();
        let first = store
            .remember("https://a.example/", b"same")
            .expect("remember");
        let second = store
            .remember("https://b.example/", b"same")
            .expect("remember");
        assert_eq!(first.as_str(), second.as_str());
        assert_eq!(
            store.recall("https://b.example/", Duration::from_secs(900)),
            Some(b"same".to_vec())
        );
    }

    #[test]
    fn content_comes_back_byte_for_byte() {
        let scratch = Scratch::new("roundtrip");
        let store = scratch.store();
        // Bytes rather than text: a page is whatever came off the wire, including an icon.
        let page = b"<html>\xef\xbb\xbf caf\xc3\xa9 \x00\x01\x02</html>".to_vec();
        let hash = store.put(&page).expect("put");
        assert_eq!(store.get(&hash).as_deref(), Some(page.as_slice()));
        assert!(store.holds(&hash));
    }

    /// Two claims citing one page share one file, and neither has to know about the other.
    #[test]
    fn the_same_content_stores_once_under_one_hash() {
        let scratch = Scratch::new("shared");
        let store = scratch.store();
        let first = store.put(b"a page").expect("put");
        let second = store.put(b"a page").expect("put again");
        assert_eq!(first, second);

        let mut files = 0;
        for shard in std::fs::read_dir(store.root()).expect("root").flatten() {
            files += std::fs::read_dir(shard.path())
                .map(Iterator::count)
                .unwrap_or(0);
        }
        assert_eq!(files, 1, "one file for one page");
    }

    /// A page whose content changed is a different citation, never a silent overwrite.
    #[test]
    fn different_content_is_a_different_hash() {
        let scratch = Scratch::new("distinct");
        let store = scratch.store();
        assert_ne!(store.put(b"before").unwrap(), store.put(b"after").unwrap());
    }

    /// The expected case, not an error: a citation outliving the bytes it points at.
    #[test]
    fn a_citation_survives_its_content_being_swept() {
        let scratch = Scratch::new("swept");
        let store = scratch.store();
        let kept = store.put(b"still cited by something").expect("put");
        let gone = store.put(b"nothing cites this any more").expect("put");

        assert_eq!(store.sweep(std::slice::from_ref(&kept)).expect("sweep"), 1);
        assert!(store.holds(&kept));
        assert!(!store.holds(&gone));
        // The claim can still say what it read and that it no longer has it.
        assert_eq!(store.get(&gone), None);
    }

    #[test]
    fn a_sweep_that_keeps_everything_drops_nothing() {
        let scratch = Scratch::new("keepall");
        let store = scratch.store();
        let one = store.put(b"one").unwrap();
        let two = store.put(b"two").unwrap();
        assert_eq!(store.sweep(&[one, two]).expect("sweep"), 0);
    }

    #[test]
    fn empty_content_is_still_addressable() {
        let scratch = Scratch::new("empty");
        let store = scratch.store();
        let hash = store.put(b"").expect("put");
        assert!(
            !hash.as_str().is_empty(),
            "a hash of nothing is still a hash"
        );
        assert_eq!(store.get(&hash).as_deref(), Some(&b""[..]));
    }
}
