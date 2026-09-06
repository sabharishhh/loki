//! The browser rung: finding one, launching it, driving it, and reading what it shows (§12.3,
//! §12.10).
//!
//! **One module because it is one thing, and because the ring rule is right.** Launching a browser
//! and speaking its protocol are useless apart: a session nobody drives is a process, and a
//! protocol client with nothing to connect to is a socket. Splitting them put one Ring 2 adapter
//! in another's imports, which `tests/rings.rs` forbids and which merging `render` into here
//! already answered once this session.
//!
//! **Family, not Chrome.** Every Chromium fork speaks CDP, so naming a vendor would exclude
//! somebody running another and buy nothing. Verified against Brave 152.
//!
//! **Nothing here opens a socket to the web.** It starts a process, points it at the exit, and
//! talks to it over loopback. The browser's own traffic is the thing being governed, and it is
//! governed by the exit it is pointed at (§21.7).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::ports::egress::{Delegate, Delegated};

/// A Chromium-family browser on this machine.
///
/// **Family, not Chrome.** What rung 2 needs is CDP, and every Chromium fork speaks it. Naming one
/// vendor would exclude somebody who deliberately runs another and buy nothing. Verified against
/// Brave 152, which answers `/json/version` as `Chrome/152.0.7977.83` on protocol 1.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chromium {
    /// What to call it when the ladder reports which browser answered.
    pub name: &'static str,
    pub binary: PathBuf,
}

impl Chromium {
    /// The first installed browser, in preference order.
    ///
    /// Brave leads on purpose rather than alphabetically: it sends a Chrome user agent, has a very
    /// large real user base, and ships fingerprint randomisation and ad blocking of its own, so it
    /// is a better default than Chrome rather than a fallback from it.
    #[must_use]
    pub fn detect() -> Option<Self> {
        Self::detect_under(Path::new("/Applications"))
    }

    /// `detect`, rooted somewhere else. For tests, which must not depend on what is installed.
    #[must_use]
    pub fn detect_under(applications: &Path) -> Option<Self> {
        Self::candidates()
            .into_iter()
            .map(|(name, app, binary)| Self {
                name,
                binary: applications.join(app).join("Contents/MacOS").join(binary),
            })
            .find(|found| found.binary.is_file())
    }

    /// Name, application bundle, and the executable inside it.
    #[must_use]
    pub const fn candidates() -> [(&'static str, &'static str, &'static str); 7] {
        [
            ("Brave", "Brave Browser.app", "Brave Browser"),
            ("Chrome", "Google Chrome.app", "Google Chrome"),
            ("Edge", "Microsoft Edge.app", "Microsoft Edge"),
            ("Vivaldi", "Vivaldi.app", "Vivaldi"),
            ("Chromium", "Chromium.app", "Chromium"),
            ("Arc", "Arc.app", "Arc"),
            ("Opera", "Opera.app", "Opera"),
        ]
    }
}

/// Flags that keep the byte accounting honest (§21.7).
///
/// **These are specification, not tuning.** Measured on Brave 152: an ordinary launch made 31
/// requests across 8 hosts to load one page, of which 2 were the page. The rest was the updater,
/// component fetches, variations and telemetry, and all of it arrives at the exit as Loki's egress.
/// With these set the same load made 2 requests. Whatever survives is denied at the exit and
/// recorded, which is the honest outcome rather than a silent one.
///
/// A browser's own telemetry is also a privacy fact and not only an accounting one: the user did
/// not ask their assistant to tell their browser vendor they are running.
const QUIET: [&str; 8] = [
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--disable-domain-reliability",
    "--disable-breakpad",
    "--no-pings",
    "--metrics-recording-only",
    "--no-default-browser-check",
];

/// Flags every launch needs regardless of accounting.
const BASE: [&str; 3] = [
    "--no-first-run",
    "--homepage=about:blank",
    "--password-store=basic",
];

/// A running browser.
///
/// **It cannot be constructed without an exit.** §21.7 requires every outbound socket to be opened
/// by one exit, and a browser opens its own. Holding the exit is what makes this legal, so `open`
/// takes one and the session keeps it alive for as long as the browser runs. There is no
/// constructor that takes a bare binary path, which is the difference between a rule somebody
/// remembers and one the compiler enforces.
#[derive(Debug)]
pub struct Session {
    child: Child,
    port: u16,
    /// Kept so the exit outlives the browser pointed at it. Never read.
    _exit: Arc<Delegated>,
}

impl Session {
    /// Launches the browser, pointed at the exit.
    ///
    /// `profile` is Loki's own directory and never the user's. A browser already running cannot be
    /// given these flags, and attaching to a live profile would hand Loki the user's cookies,
    /// history and logged-in sessions for a job that needs none of them (§12.3).
    ///
    /// # Errors
    /// Fails if the browser cannot be started.
    pub fn open(
        browser: &Chromium,
        exit: Arc<Delegated>,
        profile: &Path,
        port: u16,
    ) -> Result<Self, BrowserError> {
        let child = Command::new(&browser.binary)
            .args(BASE)
            .args(QUIET)
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--proxy-server={}", exit.proxy_url()))
            // Nothing bypasses the exit. Loopback is exempt by default and stays that way, which
            // is what lets the browser reach the exit at all.
            .arg("--proxy-bypass-list=<-loopback>")
            // WebRTC can open connections that ignore a configured proxy, which is a second door
            // out of the process. Off here as an egress requirement, not a fingerprinting
            // preference (§21.7, failure point 106).
            .arg("--webrtc-ip-handling-policy=disable_non_proxied_udp")
            .arg("--force-webrtc-ip-handling-policy")
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--headless=new")
            .arg("about:blank")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| BrowserError::CouldNotStart(e.to_string()))?;

        Ok(Self {
            child,
            port,
            _exit: exit,
        })
    }

    /// Where the protocol client connects. Loopback, always.
    #[must_use]
    pub fn control_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/version", self.port)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Killed rather than asked. §18.3 drops guards on the cancel path and runs no cleanup code
        // there, and a browser that ignores a polite shutdown would hold the exit open past the
        // turn that opened it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chromium-family browser is installed")]
    NotInstalled,
    #[error("the browser could not be started: {0}")]
    CouldNotStart(String),
}

#[cfg(test)]
mod launching {
    use super::*;

    /// A scratch directory of our own, matching how `ledger.rs` and `journal.rs` already do it
    /// rather than adding a crate for four lines.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(what: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("loki-browser-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch");
            Self(path)
        }

        fn install(&self, app: &str, binary: &str) {
            let dir = self.0.join(app).join("Contents/MacOS");
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join(binary), b"").expect("write");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_accounting_flags_are_all_present() {
        // Two of these are the difference between 31 requests and 2 on one page load, so a silent
        // removal is a silent regression in the byte accounting rather than a tidy-up.
        for required in [
            "--disable-background-networking",
            "--disable-component-update",
            "--no-pings",
        ] {
            assert!(QUIET.contains(&required), "{required} is not optional");
        }
    }

    #[test]
    fn brave_is_preferred_and_every_candidate_names_a_bundle() {
        let candidates = Chromium::candidates();
        assert_eq!(candidates[0].0, "Brave");
        for (name, app, binary) in candidates {
            assert!(app.ends_with(".app"), "{name} names a bundle");
            assert!(!binary.is_empty(), "{name} names an executable");
        }
    }

    /// Detection reads the filesystem, so it is tested against one rather than against the machine.
    #[test]
    fn detection_finds_the_first_installed_and_nothing_when_none_are() {
        let root = Scratch::new("order");
        assert_eq!(Chromium::detect_under(root.path()), None);

        // Second in the order, so this also proves the order is a preference and not an accident.
        root.install("Google Chrome.app", "Google Chrome");
        assert_eq!(
            Chromium::detect_under(root.path()).map(|found| found.name),
            Some("Chrome")
        );

        // Brave arriving later still wins, because the order is the preference.
        root.install("Brave Browser.app", "Brave Browser");
        assert_eq!(
            Chromium::detect_under(root.path()).map(|found| found.name),
            Some("Brave")
        );
    }

    /// A directory named like the executable is not the executable.
    #[test]
    fn a_bundle_without_an_executable_is_not_installed() {
        let root = Scratch::new("hollow");
        std::fs::create_dir_all(
            root.path()
                .join("Brave Browser.app/Contents/MacOS/Brave Browser"),
        )
        .expect("mkdir");
        assert_eq!(Chromium::detect_under(root.path()), None);
    }
}

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

    /// The next event, or `None` if none arrives in time.
    ///
    /// **The primitive a page reader is built on.** Waiting for one named event is not enough once
    /// the browser expects answers: an intercepted request that is never failed or continued hangs
    /// the page, and counting requests in flight means seeing all of them. So the caller drives the
    /// loop, inspects whatever turns up, and issues commands in response.
    ///
    /// Anything buffered by an earlier `call` comes out first, because the event a caller is about
    /// to look for has very often already arrived while a command was being answered.
    ///
    /// # Errors
    /// Fails on a transport error. A timeout is `Ok(None)`, since waiting and finding nothing is an
    /// answer rather than a failure.
    pub async fn next_event(&mut self, within: Duration) -> Result<Option<Value>, CdpError> {
        if let Some(event) = self.seen.pop_front() {
            return Ok(Some(event));
        }
        loop {
            match tokio::time::timeout(within, self.read_message()).await {
                Err(_) => return Ok(None),
                Ok(message) => {
                    let message = message?;
                    if message.get("method").is_some() {
                        return Ok(Some(message));
                    }
                }
            }
        }
    }

    async fn read_message(&mut self) -> Result<Value, CdpError> {
        let text = receive_text(&mut self.stream).await?;
        serde_json::from_str(&text).map_err(|e| CdpError::Protocol(e.to_string()))
    }
}

/// The commands rung 2 actually uses, and what it does with them.
///
/// **Reading a page lives with the protocol rather than beside it.** It was briefly its own
/// adapter and `tests/rings.rs` was right to reject that: one Ring 2 adapter importing another is
/// a coupling the architecture does not allow, and the boundary was a file-organisation
/// preference rather than a real seam. Rung 2 is one adapter. When §12's extraction port lands it
/// is what this sits behind, and that is where the seam actually is.
pub mod page {
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::{Cdp, CdpError};
    use crate::core::vocab::Verdict;

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

    /// When a page counts as ready, and when it counts as readable.
    #[derive(Debug, Clone, Copy)]
    pub struct Readiness {
        /// How long the network must stay quiet before the page is settled.
        pub quiet: Duration,
        /// The whole budget for one page, load and settle together.
        pub budget: Duration,
        /// Characters of visible text below which this was not a page (§21.5).
        pub minimum_text: usize,
    }

    impl Default for Readiness {
        fn default() -> Self {
            Self {
                // Half a second of silence is the figure every headless stack converges on: long
                // enough that one late XHR does not read as settled, short enough not to be felt.
                quiet: Duration::from_millis(500),
                budget: Duration::from_secs(20),
                // Measured rather than picked. Visible text in characters, from real fetches:
                //
                //      97   an HTTP 429 error page
                //     104   an HTTP 403 error page
                //     129   example.com, the IANA placeholder
                //     134   an HTTP 404 error page
                //    1499   a page whose content is written entirely by script
                //   73365   a Wikipedia article
                //
                // Stubs and error pages cluster under 140, real content starts above 1400, and
                // anything in that valley separates them. 200 sits in it with room on both sides,
                // which is the most that can be claimed until §26 has a distribution from use.
                minimum_text: 200,
            }
        }
    }

    /// What a page turned out to be.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Rendered {
        pub html: String,
        /// Visible text length, which is what the threshold was judged on.
        pub text: usize,
        pub settled: bool,
        /// The status of the document itself, not of a subresource.
        ///
        /// `None` when the browser never reported one, which happens for a navigation that failed
        /// before a response. Absent rather than defaulted to 200: a status nobody saw is not a
        /// success, and §12.2 has a verdict for not knowing.
        pub status: Option<u16>,
    }

    /// Resource types worth never fetching.
    ///
    /// Chrome's own names, since they go over the wire as-is. `Stylesheet` is deliberately absent and
    /// `Script` doubly so: blocking scripts on the rung whose entire purpose is running them would be
    /// a page that renders to nothing.
    const NEVER_FETCH: [&str; 5] = ["Image", "Font", "Media", "Ping", "CSPViolationReport"];

    /// Loads a URL and reads what it turned into.
    ///
    /// # Errors
    /// Fails on a protocol error. A page that loads but says nothing is not an error here: it comes
    /// back with `settled` and a text count, and the caller decides the verdict.
    pub async fn read(cdp: &mut Cdp, url: &str, want: Readiness) -> Result<Rendered, CdpError> {
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Network.enable", json!({})).await?;
        block_unread_resources(cdp).await?;
        navigate(cdp, url).await?;

        let (settled, status) = settle(cdp, want).await?;
        let html = html(cdp).await?;
        let text = visible_text(cdp).await?;
        Ok(Rendered {
            html,
            text,
            settled,
            status,
        })
    }

    /// Turns off the fetches nobody reads.
    ///
    /// Interception rather than a URL pattern list, because the thing being matched is what a resource
    /// *is*, and a CDN image with no extension is still an image. Only the blocked types are
    /// intercepted, so nothing else pays for the pause.
    async fn block_unread_resources(cdp: &mut Cdp) -> Result<(), CdpError> {
        let patterns: Vec<Value> = NEVER_FETCH
            .iter()
            .map(|kind| json!({ "urlPattern": "*", "resourceType": kind, "requestStage": "Request" }))
            .collect();
        cdp.call("Fetch.enable", json!({ "patterns": patterns }))
            .await
            .map(|_| ())
    }

    /// Waits for the document, then for the network to fall quiet.
    ///
    /// Returns whether it settled inside the budget. A page that never settles is not an error: it
    /// is a page, and §12.2's verdict is the caller's to draw.
    ///
    /// **Quiet means no traffic, not zero requests outstanding.** Counting requests in flight is
    /// the obvious definition and it does not survive the real web: measured on a Wikipedia
    /// article, 49 requests were sent and 48 completed, so a counter sits at one forever and the
    /// page never settles though it has been finished for twenty seconds. One connection that
    /// stays open is normal, and a definition that a single long poll defeats is the wrong
    /// definition. Silence is observable and a balanced ledger is not.
    async fn settle(cdp: &mut Cdp, want: Readiness) -> Result<(bool, Option<u16>), CdpError> {
        let deadline = Instant::now() + want.budget;
        let mut loaded = false;
        let mut last_activity = Instant::now();
        let mut status = None;

        loop {
            if loaded && last_activity.elapsed() >= want.quiet {
                return Ok((true, status));
            }
            let Some(remaining) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                return Ok((false, status));
            };
            // Never wait longer than the quiet period, or a page that fell silent would sit here
            // until something else happened to arrive.
            let Some(event) = cdp.next_event(remaining.min(want.quiet)).await? else {
                continue;
            };

            match event.get("method").and_then(Value::as_str) {
                Some("Page.loadEventFired" | "Page.domContentEventFired") => loaded = true,
                // An intercepted request that is never answered hangs the page, so this is the one
                // event that has to be acted on rather than merely noticed.
                Some("Fetch.requestPaused") => {
                    refuse(cdp, &event).await?;
                    last_activity = Instant::now();
                }
                Some("Network.responseReceived") => {
                    // The document's own status, not a subresource's. A page whose analytics
                    // script 404s is not a 404, and taking the last status seen would say it was.
                    if event.pointer("/params/type").and_then(Value::as_str) == Some("Document")
                        && status.is_none()
                    {
                        status = event
                            .pointer("/params/response/status")
                            .and_then(Value::as_u64)
                            .and_then(|code| u16::try_from(code).ok());
                    }
                    last_activity = Instant::now();
                }
                Some(method) if method.starts_with("Network.") => last_activity = Instant::now(),
                _ => {}
            }
        }
    }

    /// Fails one intercepted request.
    async fn refuse(cdp: &mut Cdp, event: &Value) -> Result<(), CdpError> {
        let Some(id) = event.pointer("/params/requestId").and_then(Value::as_str) else {
            return Ok(());
        };
        // A refusal the page can recognise. Aborting without a reason shows up as a network error in
        // the console and some scripts retry it, which is the opposite of saving the fetch.
        cdp.call(
            "Fetch.failRequest",
            json!({ "requestId": id, "errorReason": "BlockedByClient" }),
        )
        .await
        .map(|_| ())
    }

    /// The kind of interstitial a page is showing, if any (§12.10).
    ///
    /// Read off the page's own content rather than guessed from a status, because a challenge is
    /// served as a 200 and looks like a successful fetch to everything upstream of here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Challenge {
        /// A wait, not a puzzle. Clears itself given a few seconds.
        NonInteractive,
        /// A widget that expects a click.
        Interactive,
    }

    impl Challenge {
        /// What the page is showing, if it is showing one.
        ///
        /// Markers rather than a parse. The interstitial's shape changes often and its script URL
        /// does not, so the narrow check is the durable one.
        #[must_use]
        pub fn detect(html: &str) -> Option<Self> {
            for marker in ["cType: 'non-interactive'", "cType: \"non-interactive\""] {
                if html.contains(marker) {
                    return Some(Self::NonInteractive);
                }
            }
            for marker in [
                "cType: 'managed'",
                "cType: 'interactive'",
                "challenges.cloudflare.com/turnstile",
                "cf-challenge",
            ] {
                if html.contains(marker) {
                    return Some(Self::Interactive);
                }
            }
            None
        }
    }

    /// One attempt at getting past an interstitial.
    ///
    /// **One, and then the page goes to the reader.** A non-interactive challenge is waited out,
    /// which is not evasion: it is the page not being ready, and it folds into the readiness rule
    /// above. An interactive one is attempted once inside `budget`, and if that does not clear it
    /// the answer is the `needs you` state (§14.2) in a window the reader is already looking at,
    /// not another attempt. Retrying is what hardens a host against this address, and the reader
    /// can clear a widget in a second that no amount of retrying will.
    ///
    /// The honest note, since §12.5 sets the precedent for writing these down: attempting a
    /// challenge programmatically is a stronger claim than presenting a browser-shaped request. It
    /// is bounded here to a single attempt with a person as the fallback, so the failure mode is
    /// asking rather than hammering.
    ///
    /// # Errors
    /// Fails on a protocol error. A challenge that does not clear is `Ok(false)`, not an error.
    pub async fn clear_challenge(
        cdp: &mut Cdp,
        kind: Challenge,
        budget: Duration,
    ) -> Result<bool, CdpError> {
        let deadline = Instant::now() + budget;
        if kind == Challenge::NonInteractive {
            return wait_out(cdp, deadline).await;
        }

        // The widget lives in an iframe, so the click has to land in page coordinates. Asking the
        // page where it is beats guessing, and a page that will not say where it is has not
        // rendered one yet.
        let Some((x, y)) = widget_centre(cdp).await? else {
            return wait_out(cdp, deadline).await;
        };
        for event in ["mouseMoved", "mousePressed", "mouseReleased"] {
            let mut params = json!({ "type": event, "x": x, "y": y });
            if event != "mouseMoved" {
                params["button"] = json!("left");
                params["clickCount"] = json!(1);
            }
            cdp.call("Input.dispatchMouseEvent", params).await?;
        }
        wait_out(cdp, deadline).await
    }

    /// Waits for the interstitial to go, or gives up.
    async fn wait_out(cdp: &mut Cdp, deadline: Instant) -> Result<bool, CdpError> {
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(600)).await;
            if Challenge::detect(&html(cdp).await?).is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Where the widget is, in page coordinates.
    async fn widget_centre(cdp: &mut Cdp) -> Result<Option<(f64, f64)>, CdpError> {
        let found = cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "(() => { \
                        const f = document.querySelector('iframe[src*=\"challenges.cloudflare.com\"]') \
                            || document.querySelector('#cf-turnstile, .cf-turnstile, #cf_turnstile'); \
                        if (!f) return null; \
                        const r = f.getBoundingClientRect(); \
                        return r.width ? [r.left + 30, r.top + r.height / 2] : null; })()",
                    "returnByValue": true,
                }),
            )
            .await?;
        let Some(pair) = found.pointer("/result/value").and_then(Value::as_array) else {
            return Ok(None);
        };
        let (Some(x), Some(y)) = (
            pair.first().and_then(Value::as_f64),
            pair.get(1).and_then(Value::as_f64),
        ) else {
            return Ok(None);
        };
        Ok(Some((x, y)))
    }

    /// How much text a reader would actually see.
    ///
    /// `innerText`, not the markup length: a page can be a hundred kilobytes of script and empty divs.
    /// Asked of the browser because the browser is the only thing that knows what is displayed.
    async fn visible_text(cdp: &mut Cdp) -> Result<usize, CdpError> {
        let result = cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "(document.body && document.body.innerText || '').length",
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .pointer("/result/value")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0))
    }

    impl Rendered {
        /// Whether this cleared §21.5's bar.
        ///
        /// Both halves matter and for different reasons. A page that never settled was cut off, so
        /// what is here is partial. A page that settled and still said nothing returned a shell, which
        /// is the silent empty that makes a memory system record silence as fact.
        #[must_use]
        pub const fn is_readable(&self, want: Readiness) -> bool {
            self.settled && self.text >= want.minimum_text
        }

        /// What this page was, in the ladder's own words (§12.2).
        ///
        /// **The status is read before the content.** A 404 with a helpful error page is content
        /// rich and still a 404, and no rung can make a page exist, so judging on content first
        /// would climb a ladder that cannot reach anything.
        ///
        /// Rung 2 never returns `JsRequired`: it *is* the answer to that, so a shell that survives
        /// a browser is exhausted rather than escalated.
        #[must_use]
        pub fn verdict(&self, want: Readiness) -> Verdict {
            match self.status {
                Some(404 | 410) => Verdict::NotFound,
                Some(401 | 403) => Verdict::Blocked,
                Some(429) => Verdict::RateLimited,
                Some(code) if code >= 500 => Verdict::Exhausted,
                _ if self.is_readable(want) => Verdict::Ok,
                _ => Verdict::Exhausted,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stylesheets_and_scripts_are_never_in_the_block_list() {
            // Blocking either is how a speed optimisation becomes the thin-content failure this
            // module exists to catch. Scripts especially: rung 2 exists to run them.
            assert!(!NEVER_FETCH.contains(&"Stylesheet"));
            assert!(!NEVER_FETCH.contains(&"Script"));
            assert!(!NEVER_FETCH.contains(&"XHR"));
            assert!(!NEVER_FETCH.contains(&"Fetch"));
            assert!(!NEVER_FETCH.contains(&"Document"));
        }

        #[test]
        fn the_bytes_nobody_reads_are_all_in_it() {
            for kind in ["Image", "Font", "Media"] {
                assert!(NEVER_FETCH.contains(&kind), "{kind} is never read");
            }
        }

        /// A challenge is served as a 200, so nothing upstream of the page content can see it.
        #[test]
        fn an_interstitial_is_told_apart_from_a_page() {
            assert_eq!(
                Challenge::detect("<script>window._cf = { cType: 'non-interactive' }</script>"),
                Some(Challenge::NonInteractive)
            );
            for interactive in [
                "<script>cType: 'managed'</script>",
                "<script>cType: 'interactive'</script>",
                "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script>",
                "<div class=\"cf-challenge\"></div>",
            ] {
                assert_eq!(
                    Challenge::detect(interactive),
                    Some(Challenge::Interactive),
                    "{interactive}"
                );
            }
        }

        /// The expensive false positive: treating a real page as a challenge means waiting the
        /// whole challenge budget on a page that was already finished.
        #[test]
        fn an_ordinary_page_is_not_mistaken_for_a_challenge() {
            for ordinary in [
                "<html><body><h1>Example Domain</h1></body></html>",
                // Words that appear in prose about the subject, which is where a loose substring
                // match would fire.
                "<p>We migrated off Cloudflare last year and the interactive demo is below.</p>",
                "<p>This article explains how a managed challenge works.</p>",
                "",
            ] {
                assert_eq!(Challenge::detect(ordinary), None, "{ordinary}");
            }
        }

        fn reading(status: Option<u16>, text: usize, settled: bool) -> Rendered {
            Rendered {
                html: String::new(),
                text,
                settled,
                status,
            }
        }

        /// The whole table, because each verdict routes differently and one wrong row sends the
        /// ladder somewhere it cannot come back from.
        #[test]
        fn a_status_decides_the_verdict_before_the_content_does() {
            let want = Readiness::default();
            // A 404 with a friendly error page is content-rich and still a 404. Reading the
            // content first would climb a ladder that cannot reach anything.
            assert_eq!(
                reading(Some(404), 5_000, true).verdict(want),
                Verdict::NotFound
            );
            assert_eq!(
                reading(Some(410), 5_000, true).verdict(want),
                Verdict::NotFound
            );
            assert_eq!(
                reading(Some(403), 5_000, true).verdict(want),
                Verdict::Blocked
            );
            assert_eq!(
                reading(Some(401), 5_000, true).verdict(want),
                Verdict::Blocked
            );
            assert_eq!(
                reading(Some(429), 5_000, true).verdict(want),
                Verdict::RateLimited
            );
            assert_eq!(
                reading(Some(503), 5_000, true).verdict(want),
                Verdict::Exhausted
            );
            assert_eq!(reading(Some(200), 5_000, true).verdict(want), Verdict::Ok);
        }

        /// Rung 2 is the answer to `JsRequired`, so it can never be the one asking for it.
        #[test]
        fn a_shell_that_survived_a_browser_is_exhausted_rather_than_escalated() {
            let want = Readiness::default();
            let shell = reading(Some(200), 12, true);
            assert_eq!(shell.verdict(want), Verdict::Exhausted);
            assert!(
                !shell.verdict(want).should_escalate(),
                "there is nowhere above this rung to go"
            );
        }

        #[test]
        fn an_unknown_status_falls_back_to_the_content() {
            let want = Readiness::default();
            assert_eq!(reading(None, 5_000, true).verdict(want), Verdict::Ok);
            assert_eq!(
                reading(None, 5_000, false).verdict(want),
                Verdict::Exhausted
            );
        }

        #[test]
        fn only_two_verdicts_climb_and_none_retry() {
            for verdict in [Verdict::JsRequired, Verdict::Blocked] {
                assert!(verdict.should_escalate(), "{verdict:?} climbs");
            }
            // Each terminal for its own reason: the page is gone, the host asked to be left
            // alone, the ladder is spent, or the rung that would help does not exist yet.
            for verdict in [
                Verdict::Ok,
                Verdict::RateLimited,
                Verdict::NotFound,
                Verdict::InteractionRequired,
                Verdict::Exhausted,
            ] {
                assert!(!verdict.should_escalate(), "{verdict:?} does not climb");
            }
            for verdict in [Verdict::Blocked, Verdict::RateLimited, Verdict::Ok] {
                assert!(
                    !verdict.may_retry(),
                    "{verdict:?} is never retried on its own rung"
                );
            }
        }

        #[test]
        fn a_page_that_did_not_settle_is_not_readable_however_much_it_said() {
            let want = Readiness::default();
            let cut_off = reading(None, 100_000, false);
            assert!(
                !cut_off.is_readable(want),
                "a page cut off mid-load is partial, whatever arrived first"
            );
        }

        /// The §21.5 case: a 200 that settled and still said nothing.
        #[test]
        fn a_shell_that_settled_is_not_readable_either() {
            let want = Readiness::default();
            let shell = reading(Some(200), 12, true);
            assert!(!shell.is_readable(want));

            let real = reading(Some(200), want.minimum_text, true);
            assert!(real.is_readable(want), "exactly at the threshold counts");
        }

        #[test]
        fn the_defaults_are_the_ones_that_were_reasoned_about() {
            let want = Readiness::default();
            assert_eq!(want.quiet, Duration::from_millis(500));
            assert!(
                want.quiet < want.budget,
                "a quiet period longer than the budget can never be observed"
            );
        }
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
mod protocol {
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

/// Waits for the browser to start listening on its control port.
///
/// Spawning returns before the socket is up, and a connect that races it fails for a reason that
/// has nothing to do with the page being asked for.
async fn wait_until_listening(port: u16) -> Result<(), CdpError> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..100 {
        if TcpStream::connect(address).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(CdpError::Unreachable(format!(
        "the browser never listened on {port}"
    )))
}

/// A plain GET on loopback.
///
/// Not through the exit: this is the control channel to a process we launched, and routing it
/// through the egress adapter would file the browser's own address as web traffic.
///
/// **Read to `Content-Length`, never to end of stream.** The browser ignores `Connection: close`
/// and holds the socket open, so `read_to_end` waits for a close that never comes and the whole
/// rung hangs before it has asked for a page. The timeout is a backstop for a browser that answers
/// with neither a length nor a close.
async fn loopback_get(port: u16, path: &str) -> Result<String, CdpError> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|e| CdpError::Unreachable(e.to_string()))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| CdpError::Transport(e.to_string()))?;

    let reading = async {
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream
                .read(&mut buffer)
                .await
                .map_err(|e| CdpError::Transport(e.to_string()))?;
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..read]);

            let text = String::from_utf8_lossy(&raw);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            // Present on every `/json` answer. Without it there is nothing to wait for, so what
            // has arrived is what there is.
            let Some(length) = content_length(head) else {
                return Ok(body.to_owned());
            };
            if body.len() >= length {
                return Ok(body[..length].to_owned());
            }
        }
        Ok(String::from_utf8_lossy(&raw).into_owned())
    };

    tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .map_err(|_| CdpError::TimedOut(format!("the browser did not answer {path}")))?
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_owned)
        })
        .and_then(|value| value.parse().ok())
}

/// The socket for a *page*, which is not the socket for the browser.
///
/// **`/json/version` is the wrong endpoint and it fails silently.** It answers with the browser's
/// own debugger URL, which accepts a connection and a handshake and then ignores every `Page.*`
/// command sent to it, so a navigation on it never completes and never errors either. The page
/// targets are on `/json/list`, and this takes the first of them: the session launches with one
/// blank tab, so there is always exactly one to drive.
async fn page_socket(port: u16) -> Result<String, CdpError> {
    let body = loopback_get(port, "/json/list").await?;
    let targets: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| CdpError::Protocol(e.to_string()))?;
    let url = targets
        .as_array()
        .and_then(|targets| {
            targets
                .iter()
                .find(|target| target["type"] == "page")
                .and_then(|target| target["webSocketDebuggerUrl"].as_str())
        })
        .ok_or_else(|| CdpError::Protocol("the browser has no page to drive".to_owned()))?;
    Ok(url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map_or_else(|| "/".to_owned(), |(_, path)| format!("/{path}")))
}

/// The browser rung, as the two ports the search ladder speaks (§12.1, §12.2).
///
/// **One warm process, not one per search.** Launching a browser costs about a second and reading
/// a page costs a fraction of that, so a rung that started a browser per query would spend most of
/// its time on the part that is not the work. The session is opened on first use and kept until
/// the app stops.
///
/// **Discovery and extraction are the same act here.** Finding pages means rendering a results page
/// and reading its links; reading a page means rendering it and taking its text. Both are
/// `page::read` with a different URL and a different thing wanted from the result, which is why
/// there is no separate search client and nothing to keep in step.
pub struct Browsing {
    browser: Chromium,
    profile: PathBuf,
    port: u16,
    /// The exit is opened with the browser and dropped with it, so an idle Loki holds no bound
    /// socket either. Holding a live `Delegated` here would keep a listener up for a search nobody
    /// asked for.
    exit: Arc<dyn Delegate>,
    gate: super::politeness::Shared,
    want: page::Readiness,
    /// `None` until the first page is asked for, and again once the reaper has been round.
    warm: Arc<tokio::sync::Mutex<Option<Warm>>>,
    engine: Engine,
    idle: Duration,
}

/// A running browser and when it was last wanted.
struct Warm {
    /// Held for its `Drop`, which kills the browser and releases the exit with it. Never read.
    _session: Session,
    used: std::time::Instant,
}

/// How long a browser stays up with nothing to do.
///
/// **The whole cost of this rung is that it is a process.** §1 asks for an idle Loki to cost
/// almost nothing, and a browser held open for a question asked once is the opposite of that. Two
/// and a half minutes keeps a follow-up instant and lets a session that is genuinely over end.
const IDLE: Duration = Duration::from_secs(150);

/// How long to wait before looking again, or `None` when it has been idle long enough to close.
///
/// Split out so the arithmetic is testable without a browser: the reaper itself needs a real
/// process to watch, and the part that gets an off-by-one wrong is this.
fn still_wanted(used: std::time::Instant, after: Duration) -> Option<Duration> {
    let idle = used.elapsed();
    (idle < after).then(|| after - idle)
}

/// Closes the browser once it has been idle, then stops.
///
/// **The task exits when it reaps, so an idle Loki runs no timer.** A ticker that lives for the
/// life of the app to notice something that happens once is the shape this is avoiding.
async fn reap(warm: Arc<tokio::sync::Mutex<Option<Warm>>>, after: Duration) {
    loop {
        let remaining = {
            let held = warm.lock().await;
            match held.as_ref() {
                // Closed by something else, so there is nothing left to watch.
                None => return,
                Some(warm) => still_wanted(warm.used, after),
            }
        };
        match remaining {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => {
                let mut held = warm.lock().await;
                // Checked again under the lock, because a render may have started while it was
                // free and killing a browser mid-page is worse than keeping one too long.
                if held
                    .as_ref()
                    .is_some_and(|warm| still_wanted(warm.used, after).is_none())
                {
                    *held = None;
                    return;
                }
            }
        }
    }
}

impl Browsing {
    /// # Errors
    /// Fails if no Chromium-family browser is installed.
    pub fn new(
        exit: Arc<dyn Delegate>,
        profile: PathBuf,
        port: u16,
        gate: super::politeness::Shared,
    ) -> Result<Self, BrowserError> {
        Ok(Self {
            browser: Chromium::detect().ok_or(BrowserError::NotInstalled)?,
            profile,
            port,
            exit,
            gate,
            want: page::Readiness::default(),
            warm: Arc::new(tokio::sync::Mutex::new(None)),
            engine: Engine::DEFAULT,
            idle: IDLE,
        })
    }

    /// How long the browser stays up with nothing to do.
    ///
    /// A knob rather than a constant so the reaper can be exercised against a real browser without
    /// a test that waits two and a half minutes.
    #[must_use]
    pub const fn reaping_after(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Points discovery at a different engine.
    ///
    /// **Which engine is a deployment fact, not a design one.** Whether a given engine will talk to
    /// a given address changes without notice and differs per machine, so this is a knob rather
    /// than a constant. What does not change is that a real browser is what makes any of them
    /// possible.
    #[must_use]
    pub fn searching_with(mut self, engine: Engine) -> Self {
        self.engine = engine;
        self
    }

    /// Opens a page and hands back what the browser rendered.
    ///
    /// The session is launched on the first call and reused after it. A browser that has died is
    /// replaced rather than reported: the process is ours and its lifetime is not the caller's
    /// problem.
    async fn render(&self, url: &str) -> Result<page::Rendered, CdpError> {
        self.gate.wait_for(&super::politeness::host_of(url)).await;

        let mut held = self.warm.lock().await;
        if held.is_none() {
            // Opened with the browser rather than held for its lifetime, so nothing is bound while
            // no search is running.
            let exit = self
                .exit
                .delegate(crate::ports::egress::Policy::for_target(self.engine.host))
                .await
                .map_err(|e| CdpError::Unreachable(e.to_string()))?;
            let session = Session::open(&self.browser, Arc::new(exit), &self.profile, self.port)
                .map_err(|e| CdpError::Unreachable(e.to_string()))?;
            // The browser needs a moment between spawning and listening, and a connect that races
            // it fails for a reason that has nothing to do with the page.
            wait_until_listening(self.port).await?;
            *held = Some(Warm {
                _session: session,
                used: std::time::Instant::now(),
            });
            tokio::spawn(reap(Arc::clone(&self.warm), self.idle));
        }

        let target = page_socket(self.port).await?;
        let address = SocketAddr::from(([127, 0, 0, 1], self.port));
        let mut cdp = Cdp::connect(address, &target).await?;
        let rendered = page::read(&mut cdp, url, self.want).await;

        // A dead browser is the one failure worth retrying, because it is ours and it is invisible
        // to the caller. Once, then the error stands.
        if matches!(
            rendered,
            Err(CdpError::Unreachable(_) | CdpError::Transport(_))
        ) {
            *held = None;
        } else if let Some(warm) = held.as_mut() {
            // Stamped after the page, not before it: a slow read is still the browser being used.
            warm.used = std::time::Instant::now();
        }
        rendered
    }
}

#[async_trait::async_trait]
impl crate::ports::search::Extract for Browsing {
    fn rung(&self) -> crate::core::vocab::Rung {
        crate::core::vocab::Rung::Rendered
    }

    async fn read(
        &self,
        url: &str,
        _cancel: crate::ports::search::CancelToken,
    ) -> Result<crate::ports::search::Page, crate::ports::search::SearchError> {
        let rendered = self
            .render(url)
            .await
            .map_err(|e| crate::ports::search::SearchError::Unreachable(e.to_string()))?;
        let verdict = rendered.verdict(self.want);
        let (title, text) = super::readability::readable(url, &rendered.html);
        Ok(crate::ports::search::Page {
            url: url.to_owned(),
            title,
            text,
            icon: super::readability::icon_url(url, &rendered.html),
            rung: crate::core::vocab::Rung::Rendered,
            verdict,
        })
    }
}

#[async_trait::async_trait]
impl crate::ports::search::Discover for Browsing {
    fn id(&self) -> &'static str {
        self.engine.id
    }

    async fn search(
        &self,
        query: &str,
        _cancel: crate::ports::search::CancelToken,
    ) -> Result<Vec<crate::ports::search::Hit>, crate::ports::search::SearchError> {
        let url = self.engine.query_url(query);
        let rendered = self
            .render(&url)
            .await
            .map_err(|e| crate::ports::search::SearchError::Unreachable(e.to_string()))?;

        if page::Challenge::detect(&rendered.html).is_some() {
            return Err(crate::ports::search::SearchError::Refused {
                engine: self.engine.id.to_owned(),
                detail: "the engine asked for a challenge a person has to answer".to_owned(),
            });
        }
        let hits = results_in(&rendered.html, self.engine.host);
        if hits.is_empty() {
            // A results page that rendered and offered nothing is the silent empty §21.5 is
            // written against, not an answer.
            return Err(crate::ports::search::SearchError::SilentlyEmpty {
                engine: self.engine.id.to_owned(),
            });
        }
        Ok(hits)
    }
}

/// Where a browser-driven search asks its question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    pub id: &'static str,
    /// The engine's own host, so its navigation is not mistaken for its results.
    pub host: &'static str,
    /// The results URL, with `{q}` where the encoded query goes.
    pub query: &'static str,
}

impl Engine {
    pub const BING: Self = Self {
        id: "bing",
        host: "bing.com",
        query: "https://www.bing.com/search?q={q}",
    };
    pub const GOOGLE: Self = Self {
        id: "google",
        host: "google.com",
        query: "https://www.google.com/search?q={q}",
    };
    pub const BRAVE: Self = Self {
        id: "brave",
        host: "search.brave.com",
        query: "https://search.brave.com/search?q={q}",
    };
    pub const STARTPAGE: Self = Self {
        id: "startpage",
        host: "startpage.com",
        query: "https://www.startpage.com/sp/search?query={q}",
    };
    pub const MOJEEK: Self = Self {
        id: "mojeek",
        host: "mojeek.com",
        query: "https://www.mojeek.com/search?q={q}",
    };
    pub const DUCKDUCKGO: Self = Self {
        id: "duckduckgo",
        host: "duckduckgo.com",
        query: "https://html.duckduckgo.com/html/?q={q}",
    };

    /// What the app uses unless told otherwise.
    pub const DEFAULT: Self = Self::BING;

    #[must_use]
    pub fn query_url(&self, query: &str) -> String {
        self.query.replace("{q}", &encode(query))
    }
}

/// Percent-encodes a query for a URL.
fn encode(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 8);
    for byte in query.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The links a rendered results page is offering, in the order it offered them.
///
/// **Read as links, not as an engine's markup.** Every engine renames its result class every few
/// months and a selector is what makes a scraper rot. What does not change is that a results page
/// is a list of anchors pointing somewhere else, so that is what this reads: outbound links, in
/// document order, one per destination, with the anchor's own text as the title.
fn results_in(html: &str, engine_host: &str) -> Vec<crate::ports::search::Hit> {
    let mut best: Vec<(String, String)> = Vec::new();
    let mut from = 0;

    while let Some(at) = html[from..].find("<a ") {
        let start = from + at;
        let Some(close) = html[start..].find('>') else {
            break;
        };
        let head = &html[start..start + close];
        from = start + close + 1;

        let Some(href) = attribute(head, "href") else {
            continue;
        };
        let href = unwrap_redirect(&href);
        if !href.starts_with("http") || href.contains(engine_host) {
            continue;
        }
        let Some(end) = html[from..].find("</a>") else {
            continue;
        };
        let title = strip_tags(&html[from..from + end]);
        if title.len() < 12 || looks_like_a_url(&title) {
            continue;
        }
        // **The best title for a link, not the first.** Every engine prints a result twice, once as
        // the headline and once as a breadcrumb under it, and keeping whichever came first gave
        // citations titled "wikipedia.orghttps://en.wikipedia.org › wiki › Kerala".
        match best.iter_mut().find(|(url, _)| *url == href) {
            Some((_, held)) if held.len() < title.len() => *held = title,
            Some(_) => {}
            None => best.push((href, title)),
        }
    }

    best.into_iter()
        .map(|(url, title)| crate::ports::search::Hit {
            url,
            title,
            snippet: String::new(),
        })
        .collect()
}

/// Whether a string is a link's address rather than its name.
///
/// A breadcrumb reads as a title to a parser and as noise to a reader.
fn looks_like_a_url(title: &str) -> bool {
    title.contains("://") || title.contains('›') || title.starts_with("www.")
}

/// Follows an engine's own redirector to the page it points at, without asking for it.
///
/// **Every large engine wraps its results, and dropping the engine's host drops the results.** Bing
/// serves `bing.com/ck/a?...&u=a1<base64url>`, Google serves `/url?q=<escaped>`, DuckDuckGo serves
/// `/l/?uddg=<escaped>`. A parser that filtered out the engine's domain to skip its navigation
/// threw away every result on the page and reported the page as empty, which reads exactly like a
/// block and is not one.
///
/// Unwrapping here rather than following the redirect later keeps the citation pointing at the
/// publisher instead of at the engine, and saves a request per result.
fn unwrap_redirect(href: &str) -> String {
    // The href comes out of the document with its entities intact, so the parameter separators are
    // `&amp;` and every match below would miss. This cost a whole afternoon reading it as a block.
    let href = &href.replace("&amp;", "&");
    for key in ["&u=a1", "?u=a1"] {
        if let Some(at) = href.find(key) {
            let encoded = &href[at + key.len()..];
            let encoded = encoded.split('&').next().unwrap_or(encoded);
            if let Some(decoded) = from_base64_url(encoded) {
                return decoded;
            }
        }
    }
    for key in ["?q=", "&q=", "?uddg=", "&uddg=", "?url=", "&url="] {
        if let Some(at) = href.find(key) {
            let value = &href[at + key.len()..];
            let value = value.split('&').next().unwrap_or(value);
            let decoded = percent_decode(value);
            if decoded.starts_with("http") {
                return decoded;
            }
        }
    }
    href.clone()
}

/// Base64url without padding, which is what Bing uses for the wrapped target.
fn from_base64_url(text: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bits = 0_u32;
    let mut held = 0_u8;
    let mut out = Vec::new();
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|c| *c == byte)? as u32;
        bits = (bits << 6) | value;
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push(u8::try_from((bits >> held) & 0xFF).ok()?);
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    decoded.starts_with("http").then_some(decoded)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[at + 1..at + 3], 16)
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(if bytes[at] == b'+' { b' ' } else { bytes[at] });
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn attribute(head: &str, key: &str) -> Option<String> {
    let at = head.find(&format!("{key}=\""))? + key.len() + 2;
    let end = head[at..].find('"')? + at;
    Some(head[at..end].to_owned())
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for ch in html.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod discovery {
    use super::*;

    /// The defect that made every engine look blocked: Bing wraps each result in its own
    /// redirector, the href arrives with its entities intact, and a matcher looking for `&u=a1`
    /// never sees `&amp;u=a1`. Twenty results read as zero, which is indistinguishable from a
    /// block and is not one.
    #[test]
    fn a_wrapped_result_is_unwrapped_through_its_entities() {
        let href = "https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9lbi53aWtpcGVkaWEub3JnL3dpa2kvS2VyYWxh&amp;ntb=1";
        assert_eq!(
            unwrap_redirect(href),
            "https://en.wikipedia.org/wiki/Kerala"
        );
    }

    #[test]
    fn a_google_style_wrapper_is_unwrapped_too() {
        assert_eq!(
            unwrap_redirect("/url?q=https%3A%2F%2Fexample.com%2Fa&sa=U"),
            "https://example.com/a"
        );
    }

    /// A plain link is left exactly as it is. Unwrapping something that is not wrapped is how a
    /// citation ends up pointing somewhere the page never linked to.
    #[test]
    fn an_ordinary_link_is_left_alone() {
        let plain = "https://www.thehindu.com/news/national/kerala/";
        assert_eq!(unwrap_redirect(plain), plain);
    }

    /// A query parameter that merely looks like a wrapper must not be mistaken for one.
    #[test]
    fn a_search_query_is_not_a_redirect() {
        let href = "https://example.com/search?q=how+to+make+bread";
        assert_eq!(unwrap_redirect(href), href);
    }

    #[test]
    fn results_are_read_off_the_page_in_order_and_once_each() {
        let html = "<html><a href=\"/settings\">Settings</a>\
            <a href=\"https://a.example/one\">The first result headline</a>\
            <a href=\"https://b.example/two\">The second result headline</a>\
            <a href=\"https://a.example/one\">The first result headline</a>\
            <a href=\"https://c.example\">short</a></html>";
        let hits = results_in(html, "engine.example");
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].url, "https://a.example/one");
        assert_eq!(hits[1].title, "The second result headline");
    }

    /// The headline, not the breadcrumb Bing prints under it.
    #[test]
    fn the_best_title_for_a_link_wins_not_the_first() {
        let html = "<a href=\"https://en.wikipedia.org/wiki/Kerala\">wikipedia.org > wiki > Kerala</a>\
            <a href=\"https://en.wikipedia.org/wiki/Kerala\">Kerala, a state on India's Malabar Coast</a>";
        let hits = results_in(html, "bing.com");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Kerala, a state on India's Malabar Coast");
    }

    #[test]
    fn a_breadcrumb_is_never_a_title() {
        let html = "<a href=\"https://example.com/a\">https://example.com › docs › guide</a>";
        assert!(results_in(html, "bing.com").is_empty());
    }

    /// The engine's own navigation is not a result.
    #[test]
    fn the_engines_own_links_are_not_results() {
        let html = "<a href=\"https://www.bing.com/images/search\">Images for this search</a>";
        assert!(results_in(html, "bing.com").is_empty());
    }

    #[test]
    fn a_query_with_punctuation_survives_the_url() {
        assert_eq!(encode("c++ & rust?"), "c%2B%2B+%26+rust%3F");
        assert_eq!(
            Engine::BING.query_url("kerala news"),
            "https://www.bing.com/search?q=kerala+news"
        );
    }
}

#[cfg(test)]
mod idling {
    use super::*;

    #[test]
    fn a_browser_just_used_is_still_wanted() {
        let left = still_wanted(std::time::Instant::now(), Duration::from_secs(150));
        assert!(left.is_some_and(|left| left > Duration::from_secs(148)));
    }

    #[test]
    fn a_browser_idle_past_the_timeout_is_not() {
        let long_ago = std::time::Instant::now() - Duration::from_secs(200);
        assert_eq!(still_wanted(long_ago, Duration::from_secs(150)), None);
    }

    /// The boundary, because this is where a reaper either spins on a zero sleep or never fires.
    #[test]
    fn exactly_at_the_timeout_is_closed_rather_than_waited_on() {
        let exactly = std::time::Instant::now() - Duration::from_secs(150);
        assert_eq!(still_wanted(exactly, Duration::from_secs(150)), None);
    }

    /// A render that lands mid-wait pushes the deadline out rather than being killed under it.
    #[test]
    fn using_it_again_buys_the_whole_window_back() {
        let after = Duration::from_secs(150);
        let nearly = std::time::Instant::now() - Duration::from_secs(149);
        assert!(still_wanted(nearly, after).is_some_and(|left| left < Duration::from_secs(2)));

        let used_again = std::time::Instant::now();
        assert!(
            still_wanted(used_again, after).is_some_and(|left| left > Duration::from_secs(148))
        );
    }
}
