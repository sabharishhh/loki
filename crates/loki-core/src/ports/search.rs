//! Finding pages, and reading them (§12.1).
//!
//! **Two capabilities, not one, because they have opposite economics.** Discovery is hard to do
//! free from a datacenter and easy from a laptop: a server aggregating many users' queries from one
//! address looks like a bot farm, and the same request from a residential address at human volume
//! looks like a person with a browser. Extraction is the reverse, expensive to buy and mostly free
//! to do, because most pages have no meaningful protection and only a hard tail needs machinery.
//!
//! One trait with both methods would make an adapter that can only do one of them lie about the
//! other, and §12.2's paid adapter is exactly that shape: it discovers and does not extract.

use async_trait::async_trait;

use crate::core::vocab::{Rung, Verdict};

/// One result from a search engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub url: String,
    pub title: String,
    /// The engine's own summary. Often enough to answer without fetching anything (§12.7).
    pub snippet: String,
}

/// A page, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub title: String,
    /// The readable content, as markdown.
    pub text: String,
    /// Where the site says its icon is, resolved against the page it was found on.
    ///
    /// **A URL, not bytes, and the distinction is honest rather than lazy.** Reading the reference
    /// off the page costs nothing because the page is already here; fetching it is another request
    /// through the gate, and it belongs with the evidence store that will cache it rather than with
    /// the read that found it. What is settled here is that it is never fetched from a favicon
    /// service, which would tell that service every site the user reads (§21.7).
    pub icon: Option<String>,
    pub rung: Rung,
    pub verdict: Verdict,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("the engine could not be reached: {0}")]
    Unreachable(String),
    #[error("the engine answered with something unreadable: {0}")]
    Unreadable(String),
    /// A known-good query returned nothing, which is a failure and not an answer (§21.5).
    #[error("{engine} returned nothing for a query that should always match")]
    SilentlyEmpty { engine: String },
    /// The engine recognised the request and refused it.
    ///
    /// **Distinct from an empty result on purpose.** A soft block is served as a page with a
    /// success-ish status and no results in it, so the shape on the wire is identical to a query
    /// nobody has written about. Reporting it as empty is the silent-empty failure §21.5 exists to
    /// catch, arriving through the code that implements the search.
    #[error("{engine} refused the request, {detail}")]
    Refused { engine: String, detail: String },
    #[error("cancelled")]
    Cancelled,
}

/// Finds pages worth reading.
#[async_trait]
pub trait Discover: Send + Sync {
    /// What the ledger calls this engine.
    fn id(&self) -> &'static str;

    /// # Errors
    /// Fails if the engine cannot be reached or its answer cannot be read.
    async fn search(&self, query: &str, cancel: CancelToken) -> Result<Vec<Hit>, SearchError>;

    /// A query whose answer is never empty, for §21.5's canary.
    ///
    /// **Every engine supplies its own.** A silent empty is the failure this whole section is
    /// written against, and it is indistinguishable from a real empty without a control question.
    fn canary(&self) -> &'static str {
        "wikipedia"
    }
}

/// Reads one page.
#[async_trait]
pub trait Extract: Send + Sync {
    /// Which rung this is, for §12.9's ledger.
    fn rung(&self) -> Rung;

    /// # Errors
    /// Fails only when the page could not be reached at all. A page that was reached and could not
    /// be read comes back as a [`Page`] carrying the verdict that says so, because §12.4 has to
    /// report an unread page rather than an empty one.
    async fn read(&self, url: &str, cancel: CancelToken) -> Result<Page, SearchError>;
}

pub use tokio_util::sync::CancellationToken as CancelToken;
