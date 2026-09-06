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
