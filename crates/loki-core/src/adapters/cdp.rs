//! A minimal Chrome DevTools Protocol client (§12.10).
//!
//! **Hand-rolled, and the reason is in the dependency tree.** `chromiumoxide` is the obvious
//! answer and it brings 150 crates including `reqwest`, `hyper` and `tungstenite`: a second HTTP
//! stack inside a process whose central architectural claim is that there is one way out (§21.7).
//! It only ever dials loopback, so it is arguably fine, and "it only talks to localhost" is the
//! shape of exemption that erodes a guarantee. `tests/rings.rs` also cannot see inside a
//! dependency, so the rule would stop being checkable exactly where it started mattering.
//!
//! What rung 2 needs is a handful of commands over one socket. That is what this is.
//!
//! **Built to grow.** [`Cdp::call`] takes a method name and a JSON value and returns one, so a
//! command this file has never heard of is one line at the call site. The typed wrappers in
//! [`page`] exist to make the common ones read well, not to gate anything.
//!
//! **It refuses to dial anything but loopback**, checked in [`Cdp::connect`] rather than left to a
//! test. CDP is a control channel to a process we launched ourselves, not a way out of the
//! machine, and the difference has to be enforced somewhere a later change cannot quietly widen.

use std::collections::VecDeque;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// One connection to a browser.
#[derive(Debug)]
pub struct Cdp {
    stream: TcpStream,
    next: u64,
    /// Events that arrived while a reply was being waited for. Never dropped: a page load event
    /// routinely lands between a command and its answer, and discarding it would mean waiting for
    /// one that has already happened.
    seen: VecDeque<Value>,
}

impl Cdp {
    /// Opens the protocol socket for a browser's page target.
    ///
    /// # Errors
    /// Fails if the address is not loopback, if the browser is not listening, or if the handshake
    /// is refused.
    pub async fn connect(address: SocketAddr, path: &str) -> Result<Self, CdpError> {
        // Enforced here, not asserted in a test. A control channel that can be pointed at a
        // remote host is an egress path that never passes the exit (§21.7).
        if !is_loopback(&address) {
            return Err(CdpError::NotLoopback(address.to_string()));
        }
        let mut stream = TcpStream::connect(address)
            .await
            .map_err(|e| CdpError::Unreachable(e.to_string()))?;
        handshake(&mut stream, address, path).await?;
        Ok(Self {
            stream,
            next: 0,
            seen: VecDeque::new(),
        })
    }

    /// Sends a command and waits for its reply.
    ///
    /// This is the whole protocol surface. A command with no wrapper in [`page`] is still one call.
    ///
    /// # Errors
    /// Fails on a transport error, or when the browser answers with one.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.next += 1;
        let id = self.next;
        let request = json!({ "id": id, "method": method, "params": params });
        send_text(&mut self.stream, &request.to_string()).await?;

        loop {
            let message = self.read_message().await?;
            // Not ours: a command reply for an id we are not waiting on, or an event. Events are
            // kept; a stray reply is not, because nothing will ever ask for it again.
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(CdpError::Refused(error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if message.get("method").is_some() {
                self.seen.push_back(message);
            }
        }
    }

    /// Waits for an event, or gives up.
    ///
    /// Checks what already arrived before reading, since the event being waited for is very often
    /// the one that landed while the command asking for it was being answered.
    ///
    /// # Errors
    /// Fails on a transport error, or [`CdpError::TimedOut`] if the event does not arrive.
    pub async fn wait_for(&mut self, method: &str, within: Duration) -> Result<Value, CdpError> {
        if let Some(index) = self
            .seen
            .iter()
            .position(|event| event.get("method").and_then(Value::as_str) == Some(method))
        {
            return Ok(self.seen.remove(index).unwrap_or(Value::Null));
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CdpError::TimedOut(method.to_owned()));
            }
            let message = match tokio::time::timeout(remaining, self.read_message()).await {
                Ok(message) => message?,
                Err(_) => return Err(CdpError::TimedOut(method.to_owned())),
            };
            if message.get("method").and_then(Value::as_str) == Some(method) {
                return Ok(message);
            }
            if message.get("method").is_some() {
                self.seen.push_back(message);
            }
        }
    }

    async fn read_message(&mut self) -> Result<Value, CdpError> {
        let text = receive_text(&mut self.stream).await?;
        serde_json::from_str(&text).map_err(|e| CdpError::Protocol(e.to_string()))
    }
}

/// The commands rung 2 actually uses.
///
/// Thin on purpose. Each one is a name and a shape, so the call sites read as intent and the
/// protocol stays in one place. Adding a sixth is four lines.
pub mod page {
    use super::{Cdp, CdpError, Duration, Value, json};

    /// Turns on the page domain, which is what makes load events arrive at all.
    ///
    /// # Errors
    /// Fails if the browser refuses.
    pub async fn enable(cdp: &mut Cdp) -> Result<(), CdpError> {
        cdp.call("Page.enable", json!({})).await.map(|_| ())
    }

    /// # Errors
    /// Fails if the browser refuses.
    pub async fn navigate(cdp: &mut Cdp, url: &str) -> Result<(), CdpError> {
        cdp.call("Page.navigate", json!({ "url": url }))
            .await
            .map(|_| ())
    }

    /// Waits for the load event.
    ///
    /// **Not the same thing as ready.** §12.10 wants DOM, then network idle, then a content
    /// threshold; this is only the first of the three, and a caller that stops here is the thin
    /// content failure §21.5 exists to catch.
    ///
    /// # Errors
    /// Fails if the page does not load within `within`.
    pub async fn wait_for_load(cdp: &mut Cdp, within: Duration) -> Result<(), CdpError> {
        cdp.wait_for("Page.loadEventFired", within)
            .await
            .map(|_| ())
    }

    /// The rendered document, after scripts have had their say.
    ///
    /// # Errors
    /// Fails if the browser refuses or returns something unexpected.
    pub async fn html(cdp: &mut Cdp) -> Result<String, CdpError> {
        let result = cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "document.documentElement.outerHTML",
                    "returnByValue": true,
                }),
            )
            .await?;
        result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CdpError::Protocol("evaluate returned no string".to_owned()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("the protocol socket may only be opened on loopback, not {0}")]
    NotLoopback(String),
    #[error("the browser is not listening: {0}")]
    Unreachable(String),
    #[error("the browser refused the handshake: {0}")]
    Handshake(String),
    #[error("the browser refused the command: {0}")]
    Refused(String),
    #[error("the connection failed: {0}")]
    Transport(String),
    #[error("the browser sent something unreadable: {0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    TimedOut(String),
}

fn is_loopback(address: &SocketAddr) -> bool {
    match address.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// The opening HTTP request that turns the socket into a websocket.
///
/// **The server's `Sec-WebSocket-Accept` is not verified.** That check proves the peer understood
/// the key rather than being an HTTP server that happens to answer 101, and it costs a SHA-1
/// implementation. Here the peer is a process this app launched, on a loopback port it chose, so
/// there is nothing between the two ends to be fooled by. If this ever dials something it did not
/// start, the check goes in first.
async fn handshake(
    stream: &mut TcpStream,
    address: SocketAddr,
    path: &str,
) -> Result<(), CdpError> {
    let key = base64(&nonce());
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {address}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))?;

    let mut head = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return Err(CdpError::Handshake("response head never ended".to_owned()));
        }
        match stream.read(&mut byte).await {
            Ok(0) | Err(_) => return Err(CdpError::Handshake("connection closed".to_owned())),
            Ok(_) => head.push(byte[0]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    if !head.starts_with("HTTP/1.1 101") {
        let line = head.lines().next().unwrap_or("no status line");
        return Err(CdpError::Handshake(line.to_owned()));
    }
    Ok(())
}

/// Sixteen bytes for the handshake key.
///
/// From the standard library's hasher seed, which the operating system randomises per process. The
/// key's job is to stop a cache answering a websocket upgrade from its store; it is not a secret,
/// and there is no intermediary on loopback to fool. A crate for this would be a dependency for
/// sixteen bytes.
fn nonce() -> [u8; 16] {
    let mut out = [0_u8; 16];
    for chunk in out.chunks_mut(8) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(chunk.as_ptr() as usize);
        chunk.copy_from_slice(&hasher.finish().to_ne_bytes()[..chunk.len()]);
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..group.len()].copy_from_slice(group);
        let packed = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for slot in 0..4 {
            if slot <= group.len() {
                let index = (packed >> (18 - 6 * slot)) & 0x3F;
                out.push(ALPHABET[index as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Writes one text frame, masked.
///
/// A client must mask every frame it sends. The mask is not a secret either: it exists so a
/// proxy cannot be tricked into caching a crafted response, which is why the specification requires
/// it even where, as here, there is no proxy.
async fn send_text(stream: &mut TcpStream, text: &str) -> Result<(), CdpError> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81); // FIN, text
    let mask_bit = 0x80_u8;
    match payload.len() {
        length if length < 126 => frame.push(mask_bit | u8::try_from(length).unwrap_or(125)),
        length if length <= u16::MAX as usize => {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&u16::try_from(length).unwrap_or(u16::MAX).to_be_bytes());
        }
        length => {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    let mask = &nonce()[..4];
    frame.extend_from_slice(mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(at, byte)| byte ^ mask[at % 4]),
    );
    stream
        .write_all(&frame)
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))
}

/// Reads one complete text message, following continuations and answering pings.
async fn receive_text(stream: &mut TcpStream) -> Result<String, CdpError> {
    let mut message = Vec::new();
    loop {
        let (opcode, final_frame, payload) = read_frame(stream).await?;
        match opcode {
            0x0 | 0x1 => {
                message.extend_from_slice(&payload);
                if final_frame {
                    return String::from_utf8(message)
                        .map_err(|e| CdpError::Protocol(e.to_string()));
                }
            }
            // A ping unanswered is a connection the browser will close underneath us.
            0x9 => send_pong(stream, &payload).await?,
            0xA => {}
            0x8 => {
                return Err(CdpError::Transport(
                    "the browser closed the socket".to_owned(),
                ));
            }
            other => return Err(CdpError::Protocol(format!("opcode {other:#x}"))),
        }
    }
}

async fn read_frame(stream: &mut TcpStream) -> Result<(u8, bool, Vec<u8>), CdpError> {
    let mut header = [0_u8; 2];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))?;
    let final_frame = header[0] & 0x80 != 0;
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;

    let length = match header[1] & 0x7F {
        126 => {
            let mut wide = [0_u8; 2];
            stream
                .read_exact(&mut wide)
                .await
                .map_err(|e| CdpError::Transport(e.to_string()))?;
            u64::from(u16::from_be_bytes(wide))
        }
        127 => {
            let mut wide = [0_u8; 8];
            stream
                .read_exact(&mut wide)
                .await
                .map_err(|e| CdpError::Transport(e.to_string()))?;
            u64::from_be_bytes(wide)
        }
        short => u64::from(short),
    };
    // A frame length is attacker-controlled in the general case, and allocating on it unchecked is
    // how a peer exhausts memory. Chrome's replies are large but not this large.
    if length > 64 * 1024 * 1024 {
        return Err(CdpError::Protocol(format!("frame of {length} bytes")));
    }

    let mut mask = [0_u8; 4];
    if masked {
        stream
            .read_exact(&mut mask)
            .await
            .map_err(|e| CdpError::Transport(e.to_string()))?;
    }
    let mut payload = vec![0_u8; usize::try_from(length).unwrap_or(0)];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))?;
    if masked {
        for (at, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[at % 4];
        }
    }
    Ok((opcode, final_frame, payload))
}

async fn send_pong(stream: &mut TcpStream, payload: &[u8]) -> Result<(), CdpError> {
    let mut frame = vec![0x8A, 0x80 | u8::try_from(payload.len()).unwrap_or(0)];
    let mask = &nonce()[..4];
    frame.extend_from_slice(mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(at, byte)| byte ^ mask[at % 4]),
    );
    stream
        .write_all(&frame)
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// The guarantee `tests/rings.rs` exempts this file on the strength of.
    #[tokio::test]
    async fn nothing_but_loopback_may_be_dialled() {
        for address in [
            "93.184.216.34:9222",
            "10.0.0.5:9222",
            "[2606:2800:220:1:248:1893:25c8:1946]:9222",
        ] {
            let parsed: SocketAddr = address.parse().expect("address");
            let refused = Cdp::connect(parsed, "/devtools/browser/x").await;
            assert!(
                matches!(refused, Err(CdpError::NotLoopback(_))),
                "{address} must be refused before a socket is opened"
            );
        }
    }

    #[tokio::test]
    async fn both_loopback_families_are_allowed_through_to_the_socket() {
        // Nothing is listening, so the expected failure is the *next* one. That is the assertion:
        // the address check passed and a connection was actually attempted.
        for address in ["127.0.0.1:1", "[::1]:1"] {
            let parsed: SocketAddr = address.parse().expect("address");
            let refused = Cdp::connect(parsed, "/x").await;
            assert!(
                matches!(refused, Err(CdpError::Unreachable(_))),
                "{address} is loopback and should reach the socket, got {refused:?}"
            );
        }
    }

    #[test]
    fn base64_matches_the_alphabet_and_pads() {
        // The three padding cases, which is where a hand-rolled encoder goes wrong.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_handshake_key_is_sixteen_bytes_and_not_a_constant() {
        assert_eq!(base64(&nonce()).len(), 24);
        let first = nonce();
        assert!(
            (0..8).any(|_| nonce() != first),
            "the key must vary between connections"
        );
    }

    /// A frame this client wrote has to be readable by a server that follows the specification:
    /// masked, with the length in the right one of the three encodings.
    #[tokio::test]
    async fn a_written_frame_is_masked_and_lengths_use_the_right_form() {
        for size in [5_usize, 125, 126, 200, 70_000] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
            let address = listener.local_addr().expect("addr");
            let text = "x".repeat(size);
            let expected = text.clone();

            let server = tokio::spawn(async move {
                let (mut accepted, _) = listener.accept().await.expect("accept");
                let (opcode, final_frame, payload) =
                    read_frame(&mut accepted).await.expect("frame");
                assert_eq!(opcode, 0x1, "text");
                assert!(final_frame);
                assert_eq!(String::from_utf8(payload).expect("utf8"), expected);
            });

            let mut client = TcpStream::connect(address).await.expect("connect");
            send_text(&mut client, &text).await.expect("send");
            server.await.expect("server");
        }
    }

    /// The header says masked, so an unmasked read would return the mask key as content.
    #[tokio::test]
    async fn a_client_frame_sets_the_mask_bit() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut accepted, _) = listener.accept().await.expect("accept");
            let mut header = [0_u8; 2];
            accepted.read_exact(&mut header).await.expect("header");
            header
        });
        let mut client = TcpStream::connect(address).await.expect("connect");
        send_text(&mut client, "hello").await.expect("send");
        let header = server.await.expect("server");
        assert_eq!(header[0], 0x81, "FIN and text opcode");
        assert!(header[1] & 0x80 != 0, "a client frame must be masked");
        assert_eq!(header[1] & 0x7F, 5, "payload length");
    }

    /// An enormous declared length must be refused rather than allocated.
    #[tokio::test]
    async fn an_absurd_frame_length_is_refused_before_allocating() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut accepted, _) = listener.accept().await.expect("accept");
            // FIN + text, unmasked, 64-bit length of 8 exabytes.
            let mut frame = vec![0x81, 127];
            frame.extend_from_slice(&u64::MAX.to_be_bytes());
            let _ = accepted.write_all(&frame).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let mut client = TcpStream::connect(address).await.expect("connect");
        let read = receive_text(&mut client).await;
        assert!(
            matches!(read, Err(CdpError::Protocol(_))),
            "an 18 exabyte frame is a protocol error, not an allocation, got {read:?}"
        );
    }
}
