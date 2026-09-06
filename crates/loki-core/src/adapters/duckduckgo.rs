//! Discovery, from the user's own machine (§12.1, §12.2).
//!
//! **DuckDuckGo, and the reason is measurement rather than preference.** From one residential
//! address in July 2026, Google returned nothing parseable and Brave and Startpage suspended the
//! request; DuckDuckGo answered. That is the whole basis, and it is the kind of fact that changes,
//! which is why §21.5's canary runs from the first commit rather than being added when somebody
//! notices the results have gone quiet.
//!
//! The HTML endpoint rather than the JavaScript one, because a page that needs a browser to produce
//! its own search results would put rung 2 underneath discovery, and discovery is supposed to be
//! the cheap half.

use async_trait::async_trait;
use futures_util::StreamExt as _;
use std::sync::Arc;

use super::politeness::{Shared as Politeness, host_of};
use crate::ports::egress::{Egress, Outbound};
use crate::ports::search::{CancelToken, Discover, Hit, SearchError};

/// Searches DuckDuckGo's HTML endpoint.
pub struct DuckDuckGo {
    egress: Arc<dyn Egress>,
    /// Shared with everything else that reaches the network, so discovery and extraction cannot
    /// each spend the same budget.
    gate: Politeness,
}

impl DuckDuckGo {
    #[must_use]
    pub const fn new(egress: Arc<dyn Egress>, gate: Politeness) -> Self {
        Self { egress, gate }
    }
}

#[async_trait]
impl Discover for DuckDuckGo {
    fn id(&self) -> &'static str {
        "duckduckgo"
    }

    async fn search(&self, query: &str, cancel: CancelToken) -> Result<Vec<Hit>, SearchError> {
        let url = format!("https://html.duckduckgo.com/html/?q={}", encode(query));
        // Before the request, never after. Four in quick succession is what earned a block from
        // this endpoint, measured, and a gate consulted afterwards would have let all four go.
        self.gate.wait_for(&host_of(&url)).await;
        let request = Outbound::get(url).as_browser();
        let mut landed = self
            .egress
            .send(request, cancel)
            .await
            .map_err(|e| SearchError::Unreachable(e.to_string()))?;

        let status = landed.status;
        let mut html = Vec::new();
        while let Some(chunk) = landed.body.next().await {
            html.extend_from_slice(&chunk.map_err(|e| SearchError::Unreachable(e.to_string()))?);
        }
        let html = String::from_utf8_lossy(&html);

        if let Some(detail) = refusal(status, &html) {
            return Err(SearchError::Refused {
                engine: self.id().to_owned(),
                detail,
            });
        }
        Ok(parse(&html))
    }
}

/// Whether the engine refused, and what it said.
///
/// **A soft block does not arrive as an error status.** Measured against the live endpoint after
/// four requests in quick succession from one address: HTTP **202**, fourteen kilobytes of page,
/// no results in it, and the word "anomaly" where the results should be. A `200..300` check passes
/// that, and a parser handed it returns an empty list, which is indistinguishable from a query
/// nobody has written about. That is the exact shape §21.5 calls a silent empty, so it is caught
/// here rather than reported as an answer.
fn refusal(status: u16, html: &str) -> Option<String> {
    // Only 200 is a results page. 202 is the one this engine actually uses to say no.
    if status != 200 {
        return Some(format!("answered {status}"));
    }
    let lowered = html.to_lowercase();
    for marker in ["anomaly", "unusual traffic", "captcha", "are you a robot"] {
        if lowered.contains(marker) {
            return Some(format!("the page says {marker}"));
        }
    }
    None
}

/// Pulls results out of the results page.
///
/// **A narrow scrape on purpose.** The markup around a result changes often and the shape of a
/// result does not: a link with a class naming it a result, then a snippet. Matching the smallest
/// thing that identifies a result is what makes this survive a redesign, and returning nothing when
/// it does not is what §21.5's canary is for.
fn parse(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for block in html.split("result__body").skip(1) {
        let Some(url) = attribute(block, "result__a", "href").and_then(unwrap_redirect) else {
            continue;
        };
        let title = text_after(block, "result__a").unwrap_or_default();
        let snippet = text_after(block, "result__snippet").unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        hits.push(Hit {
            url,
            title,
            snippet,
        });
    }
    hits
}

/// The `href` of the first anchor carrying `class`.
fn attribute(block: &str, class: &str, name: &str) -> Option<String> {
    let at = block.find(class)?;
    let tag_start = block[..at].rfind('<')?;
    let tag_end = block[tag_start..].find('>')? + tag_start;
    let tag = &block[tag_start..tag_end];
    let key = format!("{name}=\"");
    let value_at = tag.find(&key)? + key.len();
    let value_end = tag[value_at..].find('"')? + value_at;
    Some(tag[value_at..value_end].to_owned())
}

/// The engine wraps results in its own redirector. The citation has to be the page, not the hop.
fn unwrap_redirect(href: String) -> Option<String> {
    let href = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href
    };
    let Some(query) = href.split_once("uddg=").map(|(_, rest)| rest) else {
        return href.starts_with("http").then_some(href);
    };
    let encoded = query.split('&').next().unwrap_or(query);
    let decoded = decode(encoded);
    decoded.starts_with("http").then_some(decoded)
}

/// The visible text of the element with `class`, tags stripped.
fn text_after(block: &str, class: &str) -> Option<String> {
    let at = block.find(class)?;
    let open = block[at..].find('>')? + at + 1;
    let close = block[open..].find('<')? + open;
    let raw = &block[open..close];
    let text = strip_entities(raw).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn strip_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn encode(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 8);
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        at += 3;
                    }
                    Err(_) => {
                        out.push(bytes[at]);
                        at += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
    <div class="result results_links">
      <div class="result__body">
        <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&amp;rut=x">Example &amp; Co</a>
        <a class="result__snippet">A page about things.</a>
      </div>
    </div>
    <div class="result results_links">
      <div class="result__body">
        <a rel="nofollow" class="result__a" href="https://direct.test/b">Direct</a>
        <a class="result__snippet">No redirector here.</a>
      </div>
    </div>"#;

    #[test]
    fn results_come_back_as_the_pages_they_point_at() {
        let hits = parse(PAGE);
        assert_eq!(hits.len(), 2);
        // The citation has to be the page a person can open, never the engine's own hop.
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].title, "Example & Co");
        assert_eq!(hits[0].snippet, "A page about things.");
        assert_eq!(hits[1].url, "https://direct.test/b");
    }

    /// The failure §21.5 exists to catch: a page that parses to nothing must return nothing rather
    /// than something empty, so the canary can tell them apart.
    #[test]
    fn a_page_that_does_not_look_like_results_yields_none() {
        for not_results in [
            "",
            "<html><body>Sorry, we had to block this request.</body></html>",
            "<div class=\"result__body\"></div>",
        ] {
            assert!(parse(not_results).is_empty(), "{not_results}");
        }
    }

    #[test]
    fn a_query_survives_the_round_trip_through_the_url() {
        for query in ["rust tls", "what is 2+2?", "a&b", "naïve", "c++ lifetimes"] {
            assert_eq!(decode(&encode(query)), query, "{query}");
        }
    }

    /// A redirector that carries something other than a page must not become a citation.
    /// The measured refusal, which looks like success to everything that only reads the status.
    #[test]
    fn a_soft_block_is_a_refusal_and_never_an_empty_result() {
        // The real one: HTTP 202, a full page, no results, the word "anomaly" in it.
        assert!(refusal(202, "<html>...</html>").is_some());
        assert!(refusal(200, "<html>anomaly detected</html>").is_some());
        assert!(refusal(200, "<p>Please solve the CAPTCHA</p>").is_some());
        assert!(refusal(429, "").is_some());
        // A real results page is not a refusal, and neither is prose that happens to discuss one.
        assert_eq!(refusal(200, PAGE), None);
    }

    #[test]
    fn a_redirect_to_nowhere_is_dropped() {
        assert_eq!(
            unwrap_redirect("//duckduckgo.com/l/?uddg=javascript%3Aalert(1)".to_owned()),
            None
        );
        assert_eq!(unwrap_redirect("/settings".to_owned()), None);
        assert_eq!(unwrap_redirect(String::new()), None);
    }
}
