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
