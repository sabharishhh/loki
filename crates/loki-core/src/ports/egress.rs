//! The one way out of the process (§7.1, §21.7).
//!
//! **Every outbound request leaves through here.** Not because we expect to leak, but because a
//! promise nobody has exercised is a guess. §9.11's locality tier is enforced by the type system
//! on the paths the compiler can see, and by nothing at all on a path somebody adds later. A
//! single port is somewhere for §21.7's test to stand: the socket is observable, and the event is
//! emitted before the bytes move, so a request the stream did not describe is a failing test
//! rather than a thing nobody notices.
//!
//! **No transport type appears in any signature here.** Ring 1 that names `reqwest` is Ring 1 that
//! cannot be swapped, which is the whole reason the ring exists.

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use tokio_util::sync::CancellationToken;

/// What a request does at the far end.
///
/// Two, because two is what the product makes. §12's ladder and §15's connectors will want more,
/// and adding one is a Ring 1 change on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request, fully decided, before anything leaves.
///
/// The body is bytes rather than a serializable value because the port owns what goes on the wire.
/// A caller that could hand over a value and let the transport serialize it could send a body the
/// event did not count, which is exactly the gap §21.7 measures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Outbound {
    /// A GET, with headers.
    ///
    /// **Headers rather than a bare URL helper.** §15's connectors and §13's HTTP tool both need
    /// an authenticated GET, and a helper that could not carry one would grow a second beside it,
    /// which is how a single exit becomes two functions that drift.
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// The header set a browser sends, in the order a browser sends it.
    ///
    /// **Order is part of the fingerprint, and `Sec-Fetch-*` is the part everyone forgets.** A
    /// missing `Sec-Fetch-Site` is one of the commonest reasons a search engine refuses a request
    /// that is otherwise indistinguishable from a browser's, and it costs nothing to send. The TLS
    /// fingerprint is the other half and belongs to the client (§12.2).
    #[must_use]
    pub fn as_browser(mut self) -> Self {
        let browserish = [
            (
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            ),
            ("accept-language", "en-US,en;q=0.9"),
            ("sec-fetch-site", "none"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-user", "?1"),
            ("sec-fetch-dest", "document"),
            ("upgrade-insecure-requests", "1"),
        ];
        for (name, value) in browserish {
            if !self
                .headers
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
            {
                self.headers.push((name.to_owned(), value.to_owned()));
            }
        }
        self
    }

    /// A POST with a JSON body.
    #[must_use]
    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body,
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Host and path, for the event. Never the query string.
    ///
    /// A query string is where credentials and personal data end up when somebody is in a hurry,
    /// and an event stream is written to a log file. Host and path say who was talked to, which is
    /// what §15.4 and §21.7 need, and nothing more.
    #[must_use]
    pub fn destination(&self) -> (String, String) {
        let rest = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest);
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path = path.split(['?', '#']).next().unwrap_or("");
        (authority.to_owned(), format!("/{path}"))
    }
}

/// The response, with its body still arriving.
///
/// Status is separate from the body because §12.4's honest exhaustion needs to say a page could
/// not be read rather than return it as empty, and that distinction is a status code.
pub struct Landed {
    pub status: u16,
    /// From `Retry-After`, when the far end sent one.
    pub retry_after: Option<Duration>,
    pub body: ByteStream,
}

/// The response body, arriving as it arrives.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, EgressError>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("the request could not be built: {0}")]
    BadRequest(String),
    #[error("transport failed: {0}")]
    Transport(String),
    #[error("cancelled")]
    Cancelled,
}

/// Who composed the request that left (§21.7).
///
/// The exit opens every socket either way. What differs is whether it also built what went down
/// it, and that is the difference between knowing the path and not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// Loki built the request, so the event's byte count and the socket's cannot disagree.
    Composed,
    /// A browser built it and the exit tunnelled it. Host and bytes are known; the path is not.
    Delegated,
}

/// Why the exit refused a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Not the target and third parties are refused for this fetch.
    NotPermitted,
    /// On the blocklist: an ad, a tracker, or the browser's own telemetry.
    Blocked,
}

/// Whether a fetch may reach hosts other than the one it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdParty {
    /// Ordinary pages, which load scripts and data from elsewhere.
    Allow,
    /// Nothing but the target. For a fetch that should touch one host and no other.
    Deny,
}

/// What a delegated exit will let through.
///
/// Default deny is not the rule here and that is deliberate: a page that cannot reach a CDN is a
/// page that did not load, which §21.5 would then have to distinguish from a page that was empty.
/// The blocklist is what does the work, and every refusal is an event (§17.1).
#[derive(Debug, Clone)]
pub struct Policy {
    /// Always reachable, whatever else says.
    pub target: String,
    /// Refused outright. Matched on the host and on any subdomain of it.
    pub blocked: Vec<String>,
    pub third_party: ThirdParty,
}

impl Policy {
    /// A policy for one page, with nothing blocked.
    #[must_use]
    pub fn for_target(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            blocked: Vec::new(),
            third_party: ThirdParty::Allow,
        }
    }

    #[must_use]
    pub fn blocking(mut self, hosts: impl IntoIterator<Item = String>) -> Self {
        self.blocked.extend(hosts);
        self
    }

    #[must_use]
    pub const fn without_third_parties(mut self) -> Self {
        self.third_party = ThirdParty::Deny;
        self
    }

    /// Whether this host may be reached.
    ///
    /// The blocklist wins over the target, so a target that is itself blocked stays blocked. That
    /// ordering matters: the blocklist is the safety rule and the target is the request.
    ///
    /// # Errors
    /// Returns why the host was refused.
    pub fn decide(&self, host: &str) -> Result<(), DenyReason> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.blocked.iter().any(|blocked| covers(blocked, &host)) {
            return Err(DenyReason::Blocked);
        }
        if covers(&self.target, &host) || self.third_party == ThirdParty::Allow {
            return Ok(());
        }
        Err(DenyReason::NotPermitted)
    }
}

/// Whether `rule` covers `host`: the same name, or a subdomain of it.
///
/// Suffix matching alone would let `evil-example.com` through a rule for `example.com`, which is
/// the classic way a blocklist stops blocking.
fn covers(rule: &str, host: &str) -> bool {
    let rule = rule.trim_end_matches('.').to_ascii_lowercase();
    host == rule || host.ends_with(&format!(".{rule}"))
}

/// A running delegated exit, and the only way to reach one.
///
/// **Holding this is what makes a browser legal.** §21.7 requires every socket to be opened by the
/// exit, and a browser opens its own, so the browser is launched pointing at this address and a
/// browser session cannot be constructed without one of these. Dropping it cancels the proxy, so
/// §18.3's interrupt closes the exit and the browser together rather than needing a second guard.
#[derive(Debug)]
pub struct Delegated {
    address: std::net::SocketAddr,
    cancel: CancellationToken,
}

impl Delegated {
    /// Built by the adapter. Public so the adapter can construct it; nothing else should.
    #[must_use]
    pub const fn new(address: std::net::SocketAddr, cancel: CancellationToken) -> Self {
        Self { address, cancel }
    }

    #[must_use]
    pub const fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    /// What to hand a browser's `--proxy-server`.
    #[must_use]
    pub fn proxy_url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for Delegated {
    fn drop(&mut self) {
        // Cancellation, not a dropped join handle. Dropping a handle detaches the task and leaves
        // the listener bound, which would outlive the turn that opened it.
        self.cancel.cancel();
    }
}

/// Opens an exit for a caller that composes its own requests.
///
/// Separate from [`Egress`] so a caller that only sends never sees it, and so the test doubles that
/// implement `Egress` are not obliged to pretend they can open a socket. One exit, two capabilities.
#[async_trait]
pub trait Delegate: Send + Sync {
    /// # Errors
    /// Fails if the local listener cannot be bound.
    async fn delegate(&self, policy: Policy) -> Result<Delegated, EgressError>;
}

/// Sends a request, and emits the event that says so before it does.
///
/// # Errors
/// Fails if the request cannot be built or the transport fails. A non-success status is not an
/// error here: it is a [`Landed`] with a status on it, because the caller is what knows whether a
/// 404 is a failure or an answer.
#[async_trait]
pub trait Egress: Send + Sync {
    async fn send(
        &self,
        request: Outbound,
        cancel: CancellationToken,
    ) -> Result<Landed, EgressError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_is_host_and_path_and_never_the_query() {
        let request = Outbound::post("https://api.openai.com/v1/chat/completions", Vec::new());
        assert_eq!(
            request.destination(),
            (
                "api.openai.com".to_owned(),
                "/v1/chat/completions".to_owned()
            )
        );
    }

    /// The event goes into a log file, and a query string is where a token ends up.
    #[test]
    fn a_query_string_never_reaches_the_event() {
        for url in [
            "https://example.com/search?q=my+medical+history&key=sk-live-1234",
            "https://example.com/search#q=my+medical+history",
        ] {
            let (host, path) = Outbound::post(url, Vec::new()).destination();
            assert_eq!(host, "example.com");
            assert_eq!(path, "/search");
        }
    }

    /// A bare host, no scheme, and a trailing slash all have to land somewhere sensible.
    #[test]
    fn an_odd_url_still_produces_a_destination() {
        for (url, want) in [
            ("http://localhost:8080/v1/x", ("localhost:8080", "/v1/x")),
            ("localhost:8080/v1/x", ("localhost:8080", "/v1/x")),
            ("https://example.com", ("example.com", "/")),
            ("https://example.com/", ("example.com", "/")),
            ("", ("", "/")),
        ] {
            let (host, path) = Outbound::post(url, Vec::new()).destination();
            assert_eq!((host.as_str(), path.as_str()), want, "{url:?}");
        }
    }
}
