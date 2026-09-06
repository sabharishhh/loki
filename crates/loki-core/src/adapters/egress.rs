//! The only transport in the tree (§7.1, §21.7).
//!
//! One `wreq::Client`, and `tests/rings.rs` fails if anything else builds one. That is what
//! makes §21.7's assertion sayable at all: a request nobody can make outside this file is a
//! request the event stream cannot miss.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::core::event::Event;
use crate::core::sink::EventSink;
use crate::ports::egress::{
    ByteStream, Delegate, Delegated, DenyReason, Egress, EgressError, EgressMode, Landed, Method,
    Outbound, Policy,
};

/// One HTTP client for the whole process.
///
/// **`wreq` rather than `reqwest`, and the difference is the TLS handshake.** §12.1's argument for
/// running discovery from the user's own machine is that a residential address at human volume
/// looks like a person with a browser. It only holds if the request looks like one too: a search
/// engine reads the TLS and HTTP/2 fingerprint before it reads a header, and a Rust HTTP client has
/// a fingerprint nothing else on earth shares. `wreq` is a hard fork of reqwest by the author of
/// the crate `primp` binds to, so the API below is the same one that was here before.
///
/// The cost is stated because it is real: it links BoringSSL, which needs `cmake` and about two and
/// a half minutes on a cold build, measured.
pub struct Http {
    client: wreq::Client,
    events: Arc<dyn EventSink>,
}

impl Http {
    /// # Errors
    /// Fails if the client cannot be built.
    pub fn new(events: Arc<dyn EventSink>) -> Result<Self, EgressError> {
        Ok(Self {
            client: wreq::Client::builder()
                // One emulation for the whole process, so every request this app makes looks like
                // the same browser. Rotating it per request is the thing that stands out.
                .emulation(wreq_util::Emulation::Chrome142)
                // **Redirects are followed here, not by the client.** A client that follows three
                // hops internally makes four requests and lets the exit emit one event, and
                // §21.7's byte accounting is then wrong by three requests with nothing to say so.
                // Every hop is its own send, with its own event, through the same door.
                .redirect(wreq::redirect::Policy::none())
                .build()
                .map_err(|e| EgressError::Transport(e.to_string()))?,
            events,
        })
    }
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http").finish_non_exhaustive()
    }
}

#[async_trait]
impl Egress for Http {
    async fn send(
        &self,
        request: Outbound,
        cancel: CancellationToken,
    ) -> Result<Landed, EgressError> {
        // Ten, which is what browsers allow. A redirect loop is otherwise indistinguishable from a
        // slow server, and measured on the live web this matters more than it sounds: Substack
        // answers its own subdomain with a 301 to another host, and a client that does not follow
        // reports every such site as unreadable.
        const HOPS: usize = 10;

        let mut request = request;
        for _ in 0..HOPS {
            let landed = self.once(request.clone(), cancel.clone()).await?;
            let Some(next) = redirected_to(&request.url, &landed) else {
                return Ok(landed);
            };
            // Only a GET is followed. Replaying a body against a location the caller did not choose
            // is how a redirect becomes an unintended write.
            if request.method != Method::Get {
                return Ok(landed);
            }
            request = Outbound {
                url: next,
                ..request
            };
        }
        Err(EgressError::Transport(format!(
            "more than {HOPS} redirects"
        )))
    }
}

impl Http {
    /// One request, one event, no following.
    async fn once(
        &self,
        request: Outbound,
        cancel: CancellationToken,
    ) -> Result<Landed, EgressError> {
        // **Before the send, never after.** An event emitted on the way back describes only the
        // requests that came back, so a call that hangs or is cancelled leaves no trace of having
        // happened. Failure point 88 is that shape.
        let (host, path) = request.destination();
        self.events.emit(&Event::Egress {
            host,
            path: Some(path),
            bytes: request.body.len(),
            mode: EgressMode::Composed,
        });

        let mut built = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            built = built.header(name, value);
        }
        if !request.body.is_empty() {
            built = built.body(request.body);
        }

        let response = tokio::select! {
            () = cancel.cancelled() => return Err(EgressError::Cancelled),
            result = built.send() => result.map_err(|e| EgressError::Transport(e.to_string()))?,
        };

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .map(std::time::Duration::from_secs);
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let status = response.status().as_u16();
        let body: ByteStream = Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|e| EgressError::Transport(e.to_string()))
        }));

        Ok(Landed {
            status,
            retry_after,
            location,
            body,
        })
    }
}

/// Where a response says to go next, resolved against where it came from.
///
/// `None` for anything that is not a redirect, which is how the caller knows it has arrived.
fn redirected_to(from: &str, landed: &Landed) -> Option<String> {
    if !matches!(landed.status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = landed.location.as_deref()?;
    Some(resolve(from, location))
}

/// Resolves a `Location` against the page that sent it.
fn resolve(from: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_owned();
    }
    let scheme = if from.starts_with("http://") {
        "http"
    } else {
        "https"
    };
    let rest = from.split_once("://").map_or(from, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if let Some(bare) = location.strip_prefix("//") {
        return format!("{scheme}://{bare}");
    }
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let parent = rest.rsplit_once('/').map_or(authority, |(head, _)| head);
    format!("{scheme}://{parent}/{location}")
}

/// The delegated exit: a local proxy that is this adapter (§21.7).
///
/// **Why a proxy rather than an exemption.** A browser composes and sends its own requests, so the
/// composed path cannot cover it. What the exit can still own is the socket: the browser is
/// launched pointing here, every connection it opens arrives as a `CONNECT`, and the bytes are
/// counted on the way past. Measured on Brave 152, that is all of them, including the browser's own
/// updater and telemetry traffic.
///
/// What it gives up is the path, because a tunnel is opaque by construction. Terminating TLS here
/// would recover it and is rejected in §22.4: it means a certificate authority on the user's
/// machine so their assistant can decrypt their own traffic.
#[async_trait]
impl Delegate for Http {
    async fn delegate(&self, policy: Policy) -> Result<Delegated, EgressError> {
        // Port zero, so the operating system picks one and two exits can never collide.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| EgressError::Transport(e.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|e| EgressError::Transport(e.to_string()))?;

        let cancel = CancellationToken::new();
        let events = Arc::clone(&self.events);
        let policy = Arc::new(policy);
        let stopping = cancel.clone();

        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stopping.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                let events = Arc::clone(&events);
                let policy = Arc::clone(&policy);
                let stopping = stopping.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        () = stopping.cancelled() => {}
                        () = tunnel(stream, policy, events) => {}
                    }
                });
            }
        });

        Ok(Delegated::new(address, cancel))
    }
}

/// One connection from the browser.
async fn tunnel(mut client: TcpStream, policy: Arc<Policy>, events: Arc<dyn EventSink>) {
    let Some(head) = read_head(&mut client).await else {
        return;
    };
    let Some((method, target)) = request_line(&head) else {
        return;
    };

    // A tunnel is the only shape this exit forwards. Chromium sends `CONNECT` for https and
    // absolute-form for plain http, and plain http is refused rather than rewritten: an https page
    // has its http subresources blocked or upgraded by the browser before they reach here, so what
    // is left is a downgrade nobody asked for. Recorded as a denial so it is visible rather than
    // mysterious.
    if method != "CONNECT" {
        let host = authority_of(&target);
        events.emit(&Event::EgressDenied {
            host,
            reason: DenyReason::NotPermitted,
        });
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
        return;
    }

    let (host, port) = split_authority(&target);
    if let Err(reason) = policy.decide(&host) {
        events.emit(&Event::EgressDenied { host, reason });
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
        return;
    }

    // **Before the send, never after**, exactly as the composed path. Neither byte count exists
    // yet, which is why the totals arrive in `EgressSettled` once the tunnel closes.
    events.emit(&Event::Egress {
        host: host.clone(),
        path: None,
        bytes: 0,
        mode: EgressMode::Delegated,
    });

    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        events.emit(&Event::EgressSettled {
            host,
            bytes_out: 0,
            bytes_in: 0,
        });
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }

    // Settles when both directions have closed, which for a keep-alive connection is when the
    // browser is finished with the host rather than when a page is. The totals are therefore late
    // rather than per-request, and that is the honest granularity a tunnel offers: the alternative
    // is reading inside the TLS, which §22.4 rejects.
    let (bytes_out, bytes_in) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));

    events.emit(&Event::EgressSettled {
        host,
        bytes_out: usize::try_from(bytes_out).unwrap_or(usize::MAX),
        bytes_in: usize::try_from(bytes_in).unwrap_or(usize::MAX),
    });
}

/// Reads up to the end of the request head.
///
/// Bounded, because an unbounded read from a socket is a memory exhaustion waiting for a bad
/// client, and this one is on loopback but the rule does not change for that.
async fn read_head(client: &mut TcpStream) -> Option<String> {
    const LIMIT: usize = 16 * 1024;
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= LIMIT {
            return None;
        }
        match client.read(&mut byte).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => head.push(byte[0]),
        }
    }
    String::from_utf8(head).ok()
}

/// `CONNECT example.com:443 HTTP/1.1` becomes `("CONNECT", "example.com:443")`.
fn request_line(head: &str) -> Option<(String, String)> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
}

/// Splits `host:port`, defaulting to 443 because a `CONNECT` without one is asking for TLS.
fn split_authority(target: &str) -> (String, u16) {
    target.rsplit_once(':').map_or_else(
        || (target.to_ascii_lowercase(), 443),
        |(host, port)| {
            (
                host.to_ascii_lowercase(),
                port.parse::<u16>().unwrap_or(443),
            )
        },
    )
}

/// The host out of an absolute-form target, for the denial event.
fn authority_of(target: &str) -> String {
    let rest = target.split_once("://").map_or(target, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    split_authority(authority).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_location_resolves_against_the_page_that_sent_it() {
        for (from, location, want) in [
            // The case measured on the live web: one host redirecting to another.
            (
                "https://astralcodexten.substack.com/",
                "https://www.astralcodexten.com/",
                "https://www.astralcodexten.com/",
            ),
            ("https://example.com/a/b", "/c", "https://example.com/c"),
            ("https://example.com/a/b", "c", "https://example.com/a/c"),
            (
                "https://example.com/a/b",
                "//cdn.test/x",
                "https://cdn.test/x",
            ),
            ("http://example.com/a", "/b", "http://example.com/b"),
        ] {
            assert_eq!(resolve(from, location), want, "{from} -> {location}");
        }
    }

    fn landed(status: u16, location: Option<&str>) -> Landed {
        Landed {
            status,
            retry_after: None,
            location: location.map(str::to_owned),
            body: Box::pin(futures_util::stream::empty()),
        }
    }

    #[test]
    fn only_a_redirect_with_somewhere_to_go_is_followed() {
        for status in [301, 302, 303, 307, 308] {
            assert!(
                redirected_to("https://example.com", &landed(status, Some("/next"))).is_some(),
                "{status} is a redirect"
            );
            // A redirect with no `Location` is a dead end, not a loop.
            assert!(redirected_to("https://example.com", &landed(status, None)).is_none());
        }
        for status in [200, 204, 404, 429, 500] {
            assert!(
                redirected_to("https://example.com", &landed(status, Some("/next"))).is_none(),
                "{status} has arrived, whatever headers it carries"
            );
        }
    }
}
