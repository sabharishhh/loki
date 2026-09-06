//! Rung 1: fetch a page and read it (§12.2).
//!
//! **The cheap rung, and the one that answers most fetches.** No browser, no rendering, one request
//! with a browser's fingerprint and headers. What it cannot do is run JavaScript, which is not the
//! compromise it sounds like: nine of the fourteen major crawlers do not either, and the ladder
//! exists precisely so the minority that need it get rung 2 rather than everything paying for one.
//!
//! **Readability rather than a selector.** §12.7 wants clean article text with a citable span, not
//! a named field, so there is nothing to break when a site is redesigned and nothing to heal.
//! `dom_smoothie` was the only crate in a thirteen-crate comparison that found the main content on
//! every test page, at F1 0.865.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;

use super::politeness::{Shared as Politeness, host_of};
use super::readability::{icon_url, readable};
use crate::core::vocab::{Rung, Verdict};
use crate::ports::egress::{Egress, Outbound};
use crate::ports::search::{CancelToken, Extract, Page, SearchError};

/// Below this, a page that returned 200 gave back a shell (§21.5).
///
/// Rung 1 cannot tell a shell from a page that needs a browser, so anything under the threshold
/// escalates rather than failing. That is the whole reason rung 2 exists.
const MINIMUM_TEXT: usize = 200;

/// Reads a page over HTTP.
pub struct Reader {
    egress: Arc<dyn Egress>,
    gate: Politeness,
}

impl Reader {
    #[must_use]
    pub const fn new(egress: Arc<dyn Egress>, gate: Politeness) -> Self {
        Self { egress, gate }
    }
}

#[async_trait]
impl Extract for Reader {
    fn rung(&self) -> Rung {
        Rung::Direct
    }

    async fn read(&self, url: &str, cancel: CancelToken) -> Result<Page, SearchError> {
        self.gate.wait_for(&host_of(url)).await;
        let request = Outbound::get(url).as_browser();
        let mut landed = self
            .egress
            .send(request, cancel)
            .await
            .map_err(|e| SearchError::Unreachable(e.to_string()))?;

        let status = landed.status;
        let retry_after = landed.retry_after;
        let mut body = Vec::new();
        while let Some(chunk) = landed.body.next().await {
            body.extend_from_slice(&chunk.map_err(|e| SearchError::Unreachable(e.to_string()))?);
        }

        // A rate limit marks the host for the session, and it does so before anything else looks at
        // the body: escalating here would send a browser at a host that just asked to be left
        // alone, which reads as evasion rather than retrieval.
        if status == 429 {
            let wait = retry_after.unwrap_or(std::time::Duration::from_secs(60));
            self.gate.back_off(&host_of(url), wait).await;
        }

        let html = String::from_utf8_lossy(&body);
        Ok(assemble(url, &html, status))
    }
}

/// Turns a fetched page into what the ladder passes on.
///
/// Split out from the fetch so the judgement can be tested without a socket, which is the half that
/// is easy to get wrong.
fn assemble(url: &str, html: &str, status: u16) -> Page {
    let verdict = verdict_for(status, html);
    let (title, text) = if verdict == Verdict::Ok {
        readable(url, html)
    } else {
        (String::new(), String::new())
    };
    // The threshold is applied after reading, not before: a page can be large and say nothing.
    let verdict = if verdict == Verdict::Ok && text.chars().count() < MINIMUM_TEXT {
        Verdict::JsRequired
    } else {
        verdict
    };
    Page {
        url: url.to_owned(),
        title,
        text,
        icon: icon_url(url, html),
        rung: Rung::Direct,
        verdict,
    }
}

/// What the status says, before anything reads the content.
fn verdict_for(status: u16, html: &str) -> Verdict {
    match status {
        404 | 410 => Verdict::NotFound,
        401 | 403 => Verdict::Blocked,
        429 => Verdict::RateLimited,
        code if code >= 500 => Verdict::Exhausted,
        code if !(200..300).contains(&code) => Verdict::Exhausted,
        _ if looks_like_a_challenge(html) => Verdict::Blocked,
        _ => Verdict::Ok,
    }
}

/// An interstitial served as a success, which is how every challenge arrives.
fn looks_like_a_challenge(html: &str) -> bool {
    let lowered = html.to_lowercase();
    [
        "cf-challenge",
        "challenges.cloudflare.com",
        "just a moment",
        "enable javascript and cookies",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::readability::absolute;

    const ARTICLE: &str = r#"<html><head><title>A Real Page</title>
        <link rel="icon" href="/static/fav.png"></head><body>
        <nav>home about contact</nav>
        <article><h1>A Real Page</h1>
        <p>Rust is a systems programming language focused on safety and performance, and this
        paragraph exists to carry enough words that the extractor has something to find and the
        threshold has something to measure. It continues for a while so that the readable text
        comfortably clears two hundred characters, which is the bar a shell would fail.</p>
        <p>A second paragraph, so the article has structure worth extracting.</p></article>
        <footer>copyright</footer></body></html>"#;

    #[test]
    fn a_real_page_reads_as_content_with_a_title() {
        let page = assemble("https://example.com/a", ARTICLE, 200);
        assert_eq!(page.verdict, Verdict::Ok);
        assert!(
            page.title.contains("Real Page"),
            "title was {:?}",
            page.title
        );
        assert!(
            page.text.contains("systems programming"),
            "text was {:?}",
            page.text
        );
        // Readability's job: the chrome around the article does not come with it.
        assert!(!page.text.contains("copyright"));
        assert!(!page.text.contains("home about contact"));
    }

    /// The case rung 2 exists for: a 200 with nothing in it escalates rather than failing.
    #[test]
    fn a_shell_asks_for_a_browser_rather_than_giving_up() {
        let shell =
            "<html><body><div id=\"root\"></div><script src=\"/app.js\"></script></body></html>";
        let page = assemble("https://example.com", shell, 200);
        assert_eq!(page.verdict, Verdict::JsRequired);
        assert!(page.verdict.should_escalate());
    }

    /// A status is read before the content, because no rung makes a page exist.
    #[test]
    fn a_status_decides_before_the_content_does() {
        for (status, want) in [
            (404, Verdict::NotFound),
            (410, Verdict::NotFound),
            (403, Verdict::Blocked),
            (429, Verdict::RateLimited),
            (503, Verdict::Exhausted),
        ] {
            assert_eq!(
                assemble("https://example.com", ARTICLE, status).verdict,
                want
            );
        }
    }

    /// An interstitial arrives as a 200 and is not a page.
    #[test]
    fn a_challenge_served_as_success_is_still_a_block() {
        for challenge in [
            "<html><body>Just a moment...<script src=\"https://challenges.cloudflare.com/x\"></script></body></html>",
            "<html><body>Enable JavaScript and cookies to continue</body></html>",
        ] {
            let page = assemble("https://example.com", challenge, 200);
            assert_eq!(page.verdict, Verdict::Blocked, "{challenge}");
            assert!(page.verdict.should_escalate());
        }
    }

    #[test]
    fn an_icon_is_found_or_guessed_but_never_fetched_from_elsewhere() {
        let declared = icon_url("https://example.com/a/b", ARTICLE);
        assert_eq!(
            declared.as_deref(),
            Some("https://example.com/static/fav.png")
        );
        // Every site has one at the root whether it says so or not.
        let guessed = icon_url("https://example.com/a", "<html></html>");
        assert_eq!(guessed.as_deref(), Some("https://example.com/favicon.ico"));
    }

    #[test]
    fn references_resolve_against_the_page_they_came_from() {
        for (href, want) in [
            ("/a.png", "https://example.com/a.png"),
            ("a.png", "https://example.com/a.png"),
            ("//cdn.test/a.png", "https://cdn.test/a.png"),
            ("https://other.test/a.png", "https://other.test/a.png"),
        ] {
            assert_eq!(absolute("https://example.com/x/y", href), want, "{href}");
        }
    }
}
