//! One request at a time per host, with a pause between (§12.2).
//!
//! **This is not manners, it is what makes discovery work at all.** Measured against DuckDuckGo's
//! live endpoint: four requests in quick succession from one residential address earned a soft
//! block that outlasted the burst, served as HTTP 202 with an otherwise ordinary page and no
//! results in it. §12.1's whole argument is that a residential address at human volume looks like a
//! person with a browser, and four requests in two seconds is not human volume. The gate is the
//! part that makes the argument true rather than merely stated.
//!
//! **One gate, not one per capability.** Discovery and extraction both reach the same hosts, and
//! two limiters that do not know about each other are one limiter with twice the rate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Paces requests so one host never sees a burst.
#[derive(Debug, Default)]
pub struct Politeness {
    /// When each host may next be touched. Keyed by host, because a rate limit is per host and a
    /// per-URL gate walks straight into the next refusal from the same one.
    next: Mutex<HashMap<String, Instant>>,
    spacing: Spacing,
}

/// How long to leave between two requests to one host.
#[derive(Debug, Clone, Copy)]
pub struct Spacing {
    pub least: Duration,
    /// Added on top, varying per request.
    ///
    /// **Jitter, because a perfectly regular interval is itself a signature.** A person does not
    /// issue a request every 1,200 milliseconds exactly, and a limiter that does has replaced one
    /// tell with another.
    pub jitter: Duration,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            least: Duration::from_millis(1_200),
            jitter: Duration::from_millis(900),
        }
    }
}

impl Politeness {
    #[must_use]
    pub fn new(spacing: Spacing) -> Self {
        Self {
            next: Mutex::new(HashMap::new()),
            spacing,
        }
    }

    /// Waits until this host may be touched, then reserves the next slot.
    ///
    /// The reservation happens before the wait returns, so two callers racing for one host queue
    /// behind each other rather than both deciding the host is free.
    pub async fn wait_for(&self, host: &str) {
        let sleep_until = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let ready = next.get(host).copied().unwrap_or(now).max(now);
            next.insert(host.to_owned(), ready + self.gap());
            ready
        };
        tokio::time::sleep_until(sleep_until).await;
    }

    /// Backs a host off for longer, after it has said so (§12.2's `RateLimited`).
    ///
    /// Marks the host, never the URL: a per-URL backoff walks into the next refusal from the same
    /// host, which is how a soft flag becomes a hard one.
    pub async fn back_off(&self, host: &str, how_long: Duration) {
        let mut next = self.next.lock().await;
        let until = Instant::now() + how_long;
        next.entry(host.to_owned())
            .and_modify(|at| *at = (*at).max(until))
            .or_insert(until);
    }

    fn gap(&self) -> Duration {
        let spread = self.spacing.jitter.as_millis().max(1);
        // From the standard library's hasher seed, which the OS randomises per process. A mask
        // that only has to be irregular does not need a crate.
        let mut hasher = std::hash::BuildHasher::build_hasher(&std::hash::RandomState::new());
        std::hash::Hasher::write_u8(&mut hasher, 0);
        let roll = u128::from(std::hash::Hasher::finish(&hasher)) % spread;
        self.spacing.least + Duration::from_millis(u64::try_from(roll).unwrap_or(0))
    }
}

/// The host of a URL, for keying the gate.
#[must_use]
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
        .to_ascii_lowercase()
}

/// Shared by everything that reaches the network.
pub type Shared = Arc<Politeness>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_keyed_without_its_scheme_port_or_path() {
        for (url, want) in [
            ("https://Example.com/a/b?c", "example.com"),
            ("http://example.com:8080/x", "example.com"),
            (
                "https://html.duckduckgo.com/html/?q=x",
                "html.duckduckgo.com",
            ),
            ("example.com", "example.com"),
        ] {
            assert_eq!(host_of(url), want, "{url}");
        }
    }

    /// Two URLs on one host share a gate. A per-URL gate is the mistake that looks like a gate.
    #[test]
    fn two_paths_on_one_host_are_one_key() {
        assert_eq!(
            host_of("https://example.com/a"),
            host_of("https://example.com/b")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_request_to_one_host_waits() {
        let gate = Politeness::new(Spacing {
            least: Duration::from_millis(1_000),
            jitter: Duration::from_millis(1),
        });
        let began = Instant::now();
        gate.wait_for("example.com").await;
        gate.wait_for("example.com").await;
        assert!(
            began.elapsed() >= Duration::from_millis(1_000),
            "the burst that earned a block took {:?}",
            began.elapsed()
        );
    }

    /// Different hosts are unrelated, or one slow site would pace the whole search.
    #[tokio::test(start_paused = true)]
    async fn different_hosts_do_not_wait_on_each_other() {
        let gate = Politeness::default();
        let began = Instant::now();
        gate.wait_for("one.test").await;
        gate.wait_for("two.test").await;
        gate.wait_for("three.test").await;
        assert!(began.elapsed() < Duration::from_millis(100));
    }

    /// §12.2: a rate limit marks the host for the session, not the URL that happened to hit it.
    #[tokio::test(start_paused = true)]
    async fn a_backoff_holds_the_whole_host() {
        let gate = Politeness::default();
        gate.back_off("example.com", Duration::from_secs(30)).await;
        let began = Instant::now();
        gate.wait_for("example.com").await;
        assert!(began.elapsed() >= Duration::from_secs(30));
    }
}
