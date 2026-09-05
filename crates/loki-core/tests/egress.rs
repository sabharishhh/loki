//! §21.7: the locality tier, asserted against a socket rather than against a type.
//!
//! §21.4 rehearses a control against local state, which works because undo's effect is on disk.
//! A locality tier's effect is on a socket, so nothing on disk can prove it holds. §9.11 says a
//! `private` claim is never pre-fetched and §22.1 makes `Locality` a type, and neither fact says
//! anything about a second path added later by somebody in a hurry.
//!
//! **The proxy is a real listener, not a fake `Egress`.** A fake would prove the port carries the
//! bytes the port was given, which is the circularity this section is written against. What has to
//! be true is that the bytes on the wire are the bytes the event stream accounted for, and only a
//! socket can say that.
//!
//! **What the assertion is, precisely.** §9.11 does not say a `private` claim never leaves the
//! machine. It says it is never pre-fetched, and that it still transits when the task explicitly
//! needs it. An assertion that the marker never appears would contradict the tier it tests and
//! would pass only by never exercising the case that matters. So: two turns, two different
//! answers, and both are the same rule.

use std::sync::{Arc, Mutex};

use jiff::civil::{Date, date};
use loki_core::adapters::clock::SystemClock;
use loki_core::adapters::egress::Http;
use loki_core::adapters::openai::Openai;
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, NullTokens};
use loki_core::core::event::Event;
use loki_core::core::prompt::Prefix;
use loki_core::core::sink::Broadcast;
use loki_core::core::sink::EventSink;
use loki_core::core::vocab::{Cents, Locality};
use loki_core::memory::bundle::Bundle;
use loki_core::memory::claim::{Claim, Origin, Privacy};
use loki_core::memory::concept::{Frontmatter, RawConcept, Status, render};
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::Index;
use loki_core::ports::egress::Egress;
use loki_core::ports::model::ModelProvider;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn today() -> Date {
    date(2026, 9, 4)
}

/// A marker no ordinary sentence would ever contain, so finding it means it was sent.
const MARKER: &str = "ZZQX-PRIVATE-MARKER-7741";

/// A listener that records every request body and answers with a canned SSE reply.
///
/// Minimal HTTP on purpose. It reads the headers, reads exactly `content-length` bytes of body,
/// and writes a fixed response. A real client library here would put the thing under test on both
/// sides of the test.
struct Proxy {
    port: u16,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Proxy {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buffer = [0u8; 4096];
                    // Read until the headers are complete, then until the declared body is.
                    let body_at = loop {
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        raw.extend_from_slice(&buffer[..read]);
                        if let Some(at) = find(&raw, b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let length = content_length(&raw[..body_at]);
                    while raw.len() - body_at < length {
                        let Ok(read) = socket.read(&mut buffer).await else {
                            break;
                        };
                        if read == 0 {
                            break;
                        }
                        raw.extend_from_slice(&buffer[..read]);
                    }
                    recorded.lock().expect("lock").push(raw[body_at..].to_vec());

                    let payload = concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n",
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// Every request body the socket actually received.
    fn bodies(&self) -> Vec<Vec<u8>> {
        self.seen.lock().expect("lock").clone()
    }

    fn bytes(&self) -> usize {
        self.bodies().iter().map(Vec::len).sum()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

#[derive(Default)]
struct Collector(Mutex<Vec<Event>>);

impl EventSink for Collector {
    fn emit(&self, event: &Event) {
        self.0.lock().expect("lock").push(event.clone());
    }
}

impl Collector {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("lock").clone()
    }

    /// Bytes the stream said were sent, which §21.7 compares against what the socket read.
    fn accounted_bytes(&self) -> usize {
        self.events()
            .iter()
            .filter_map(|event| match event {
                Event::Egress { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .sum()
    }
}

/// A store holding one `private` claim carrying the marker, plus one ordinary claim.
async fn store(dir: &std::path::Path, scope: TierScope) -> Arc<Memory> {
    let _ = std::fs::remove_dir_all(dir);

    let mut front = Frontmatter::new("Sabharish", today());
    front.status = Status::Stable;
    let mut concept = RawConcept::new(front);

    let mut open = Claim::new("The user's name is Sabharish", Origin::Stated, today());
    open.attribute = "name".to_owned();
    concept.add("name", open);

    let mut secret = Claim::new(
        format!("The user's recovery phrase is {MARKER}"),
        Origin::Stated,
        today(),
    );
    secret.attribute = "recovery_phrase".to_owned();
    secret.privacy = Privacy::Private;
    concept.add("recovery_phrase", secret);
    // Written before the store opens, so the open-time index sync picks it up and the working set
    // is generated from a store that already holds it.
    {
        let bundle = Bundle::open(dir).await.expect("bundle");
        let writer = bundle.writer().await;
        writer
            .write("people/sabharish.md", &render(&concept))
            .expect("write");
    }

    let memory = Arc::new(
        Memory::open(
            dir,
            Index::in_memory().expect("index"),
            "wire",
            today(),
            scope,
            Arc::new(Broadcast::new()),
        )
        .await
        .expect("memory"),
    );
    memory
        .refresh_working_set(today())
        .await
        .expect("working set");
    memory
}

/// Runs one turn against the proxy and hands back what the socket and the stream each saw.
async fn one_turn(label: &str, scope: TierScope) -> (Proxy, Arc<Collector>) {
    let proxy = Proxy::start().await;
    let events = Arc::new(Collector::default());
    let egress: Arc<dyn Egress> =
        Arc::new(Http::new(Arc::clone(&events) as Arc<dyn EventSink>).expect("http"));
    let provider: Arc<dyn ModelProvider> =
        Arc::new(Openai::new(egress, "test-key").with_base_url(proxy.url()));

    let dir = std::env::temp_dir().join(format!(
        "loki-egress-{}-{label}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let memory = store(&dir, scope).await;

    let mut core = Loop::new(
        provider,
        Arc::clone(&events) as Arc<dyn EventSink>,
        Arc::new(NullTokens),
        Arc::new(SystemClock),
        Prefix::new("You are Loki."),
        Budget::new(Cents::new(1_000_000)),
    );
    core.attach_memory(memory).await.expect("attach");
    core.turn_with("what is my name", CancellationToken::new())
        .await
        .expect("turn");

    let _ = std::fs::remove_dir_all(&dir);
    (proxy, events)
}

/// The ordinary turn. This is the pre-fetch guarantee, and it is the one that would catch a
/// second path added later.
#[tokio::test]
async fn a_private_claim_does_not_reach_the_wire_on_an_ordinary_turn() {
    let (proxy, events) = one_turn("ordinary", TierScope::normal(Locality::Cloud)).await;

    let bodies = proxy.bodies();
    assert!(!bodies.is_empty(), "the turn has to have sent something");
    for body in &bodies {
        let text = String::from_utf8_lossy(body);
        assert!(
            !text.contains(MARKER),
            "a private claim reached the wire on a turn that did not ask for it: {text}"
        );
        // And the store was genuinely readable, or the test proves nothing.
        assert!(text.contains("Sabharish"), "the prefix carried nothing");
    }
    assert!(
        events
            .events()
            .iter()
            .any(|event| matches!(event, Event::Egress { .. })),
        "a request that emitted no event is a request nothing can account for"
    );
}

/// The deliberate turn. A tier that permits a transit has to make that transit visible, or §17.1's
/// stream is describing a different program from the one running.
#[tokio::test]
async fn a_private_claim_reaches_the_wire_once_when_the_task_asked_for_it() {
    let (proxy, events) =
        one_turn("deliberate", TierScope::including_private(Locality::Cloud)).await;

    let sent = String::from_utf8_lossy(&proxy.bodies().concat()).into_owned();
    assert!(
        sent.contains(MARKER),
        "a tier is about how a claim reaches a prompt, not whether: {sent}"
    );
    assert_eq!(
        sent.matches(MARKER).count(),
        1,
        "once, not once per zone: {sent}"
    );

    let egressed = events
        .events()
        .into_iter()
        .filter(|event| matches!(event, Event::Egress { .. }))
        .count();
    assert!(egressed > 0, "the transit has to be on the stream");
}

/// §21.7's byte accounting. A gap is a defect, not a rounding difference.
///
/// Grok Build's gap was 27,800 to 1 and nobody inside the product saw it, which is the whole
/// argument for measuring rather than reviewing.
#[tokio::test]
async fn every_byte_on_the_wire_is_a_byte_the_stream_accounted_for() {
    let (proxy, events) = one_turn("accounting", TierScope::normal(Locality::Cloud)).await;

    assert_eq!(
        proxy.bytes(),
        events.accounted_bytes(),
        "bytes the socket read against bytes the events declared"
    );
    assert_eq!(
        proxy.bodies().len(),
        events
            .events()
            .iter()
            .filter(|event| matches!(event, Event::Egress { .. }))
            .count(),
        "one request, one event"
    );
}

// The delegated exit (§21.7). A browser composes its own requests, so the composed path cannot
// cover it. What the exit still owns is the socket, and these say so against real ones.

mod delegated {
    use super::{Arc, Mutex};
    use loki_core::adapters::egress::Http;
    use loki_core::core::event::Event;
    use loki_core::core::sink::EventSink;
    use loki_core::ports::egress::{Delegate as _, DenyReason, EgressMode, Policy, ThirdParty};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<Event>>,
    }

    impl EventSink for Recorder {
        fn emit(&self, event: &Event) {
            self.seen
                .lock()
                .expect("recorder poisoned")
                .push(event.clone());
        }
    }

    impl Recorder {
        fn events(&self) -> Vec<Event> {
            self.seen.lock().expect("recorder poisoned").clone()
        }
    }

    /// A server that answers anything with a fixed body, so the tunnel has two ends.
    async fn upstream(body: &'static [u8]) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = stream.read(&mut scratch).await;
                let _ = stream.write_all(body).await;
            }
        });
        port
    }

    /// Speaks just enough of a proxy client to open a tunnel and talk through it.
    async fn through(
        proxy: std::net::SocketAddr,
        authority: &str,
        send: &[u8],
    ) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(proxy).await.expect("reach the exit");
        stream
            .write_all(
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
            )
            .await
            .expect("write connect");
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte).await {
                Ok(0) | Err(_) => break,
                Ok(_) => head.push(byte[0]),
            }
        }
        let status = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned();
        let mut body = Vec::new();
        if status.contains("200") && !send.is_empty() {
            stream.write_all(send).await.expect("write through");
            // Half-close, because a tunnel settles when both directions have. A client that holds
            // its write side open forever is a tunnel that never reports its totals, which is
            // correct behaviour and would hang this test.
            stream.shutdown().await.expect("half close");
            let _ = stream.read_to_end(&mut body).await;
        }
        (status, body)
    }

    /// The whole point: bytes cross a socket the exit opened, and the stream accounted for them.
    #[tokio::test]
    async fn a_tunnel_carries_bytes_and_the_stream_says_so() {
        let port = upstream(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").await;
        let events = Arc::new(Recorder::default());
        let http = Http::new(events.clone()).expect("client");
        let exit = http
            .delegate(Policy::for_target("127.0.0.1"))
            .await
            .expect("open the exit");

        let (status, body) = through(
            exit.address(),
            &format!("127.0.0.1:{port}"),
            b"GET / HTTP/1.1\r\n\r\n",
        )
        .await;
        assert!(status.contains("200"), "the tunnel opened: {status}");
        assert!(!body.is_empty(), "bytes came back through it");

        let seen = events.events();
        // Announced before anything moved, with no path, because a tunnel has none to know.
        assert!(
            seen.iter().any(|event| matches!(
                event,
                Event::Egress {
                    path: None,
                    bytes: 0,
                    mode: EgressMode::Delegated,
                    ..
                }
            )),
            "the connect was announced before the send: {seen:?}"
        );
        let mut settled = None;
        for _ in 0..50 {
            settled = events.events().iter().find_map(|event| match event {
                Event::EgressSettled {
                    bytes_out,
                    bytes_in,
                    ..
                } => Some((*bytes_out, *bytes_in)),
                _ => None,
            });
            if settled.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (out, back) = settled.expect("the tunnel settled");
        assert!(
            out > 0 && back > 0,
            "both directions counted: out={out} in={back}"
        );
    }

    /// A refused host is an act, and the bytes never move.
    #[tokio::test]
    async fn a_blocked_host_is_refused_and_recorded() {
        let port = upstream(b"HTTP/1.1 200 OK\r\n\r\n").await;
        let events = Arc::new(Recorder::default());
        let http = Http::new(events.clone()).expect("client");
        let exit = http
            .delegate(Policy::for_target("example.com").blocking(["127.0.0.1".to_owned()]))
            .await
            .expect("open the exit");

        let (status, _) = through(exit.address(), &format!("127.0.0.1:{port}"), b"").await;
        assert!(status.contains("403"), "refused at the exit: {status}");

        let seen = events.events();
        assert!(
            seen.iter().any(|event| matches!(
                event,
                Event::EgressDenied {
                    reason: DenyReason::Blocked,
                    ..
                }
            )),
            "the refusal is in the stream: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, Event::Egress { .. })),
            "nothing was announced as leaving: {seen:?}"
        );
    }

    /// **The handle is the lifetime.** §18.3's interrupt drops it, and if that does not close the
    /// listener the exit outlives the turn that opened it and a browser can keep using it.
    #[tokio::test]
    async fn dropping_the_handle_closes_the_exit() {
        let events = Arc::new(Recorder::default());
        let http = Http::new(events).expect("client");
        let exit = http
            .delegate(Policy::for_target("example.com"))
            .await
            .expect("open");
        let address = exit.address();

        assert!(TcpStream::connect(address).await.is_ok(), "open while held");
        drop(exit);

        // The accept loop wakes on cancellation rather than on a timer, but the socket closing is
        // still a scheduling step away.
        let mut refused = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if TcpStream::connect(address).await.is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused, "the listener is gone once the handle is");
    }

    /// Plain HTTP is refused rather than rewritten, and visibly.
    #[tokio::test]
    async fn an_untunnelled_request_is_refused() {
        let events = Arc::new(Recorder::default());
        let http = Http::new(events.clone()).expect("client");
        let exit = http
            .delegate(Policy::for_target("example.com"))
            .await
            .expect("open");

        let mut stream = TcpStream::connect(exit.address()).await.expect("reach");
        stream
            .write_all(b"GET http://example.com/thing HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .await
            .expect("write");
        let mut head = Vec::new();
        let _ = stream.read_to_end(&mut head).await;
        assert!(String::from_utf8_lossy(&head).contains("403"));
        assert!(
            events.events().iter().any(|event| matches!(
                event,
                Event::EgressDenied { host, .. } if host == "example.com"
            )),
            "the host is named in the refusal"
        );
    }

    // The policy itself, which is arithmetic and worth checking exhaustively rather than through
    // a socket.

    #[test]
    fn a_target_and_its_subdomains_are_reachable() {
        let policy = Policy::for_target("example.com").without_third_parties();
        assert_eq!(policy.decide("example.com"), Ok(()));
        assert_eq!(policy.decide("cdn.example.com"), Ok(()));
        assert_eq!(policy.decide("EXAMPLE.COM"), Ok(()));
        assert_eq!(policy.decide("example.com."), Ok(()));
        assert_eq!(policy.decide("other.test"), Err(DenyReason::NotPermitted));
    }

    /// The classic way a blocklist stops blocking: suffix matching that ignores the dot.
    #[test]
    fn a_lookalike_domain_is_not_the_domain() {
        let policy = Policy::for_target("example.com").without_third_parties();
        for lookalike in [
            "notexample.com",
            "evil-example.com",
            "example.com.attacker.test",
        ] {
            assert_eq!(
                policy.decide(lookalike),
                Err(DenyReason::NotPermitted),
                "{lookalike} is not example.com"
            );
        }
    }

    /// **The blocklist wins over the target.** The blocklist is the safety rule and the target is
    /// only the request, so a target that is itself blocked stays blocked rather than being waved
    /// through by the thing that asked for it.
    #[test]
    fn the_blocklist_beats_the_target() {
        let policy = Policy::for_target("tracker.test").blocking(["tracker.test".to_owned()]);
        assert_eq!(policy.decide("tracker.test"), Err(DenyReason::Blocked));
        assert_eq!(
            policy.decide("pixel.tracker.test"),
            Err(DenyReason::Blocked)
        );
    }

    #[test]
    fn third_parties_are_allowed_by_default_and_the_blocklist_does_the_work() {
        let policy = Policy::for_target("example.com").blocking(["ads.test".to_owned()]);
        assert_eq!(policy.third_party, ThirdParty::Allow);
        assert_eq!(policy.decide("cdn.other.test"), Ok(()));
        assert_eq!(policy.decide("ads.test"), Err(DenyReason::Blocked));
    }
}
