//! Reading HTML the way a person would, shared by every rung that gets some (§12.2).
//!
//! **Shared infrastructure, not a provider.** Both rungs end up holding a page's markup: rung 1
//! fetches it, rung 2 renders it, and what "the main content" means does not change between them.
//! Two copies would drift, and the one that drifted would be the one nobody was looking at. This
//! sits beside `sse` and `politeness` in the ring rule for the same reason they do.

use super::politeness::host_of;

/// The main content, as markdown, with its title.
pub fn readable(url: &str, html: &str) -> (String, String) {
    let Ok(mut readability) = dom_smoothie::Readability::new(html, Some(url), None) else {
        return (String::new(), String::new());
    };
    let Ok(article) = readability.parse() else {
        return (String::new(), String::new());
    };
    let markdown = html2md::rewrite_html(&article.content, false);
    (article.title.to_string(), markdown.trim().to_owned())
}

/// Where the site says its icon is.
///
/// **Read off the page that was already fetched.** Fetching it from a favicon service would tell
/// that service every site the user reads, and fetching it from the interface would open a second
/// way out of the process (§21.7). The URL is resolved here; the bytes are fetched through the same
/// exit as everything else.
pub fn icon_url(page: &str, html: &str) -> Option<String> {
    let lowered = html.to_lowercase();
    // **Largest first, which is the opposite of what this did.** `rel="icon"` is usually the
    // 16 pixel favicon, and drawing that at 18 points on a Retina display asks for 36 pixels from
    // 16 and gets the stair-stepping B-70 fixed for the mark. `apple-touch-icon` is 180 square by
    // convention, and downscaling 180 to 36 is the case resampling is good at.
    for rel in [
        "rel=\"apple-touch-icon-precomposed\"",
        "rel=\"apple-touch-icon\"",
        "rel=\"icon\"",
        "rel=\"shortcut icon\"",
    ] {
        let Some(at) = lowered.find(rel) else {
            continue;
        };
        let tag_start = lowered[..at].rfind('<')?;
        let tag_end = lowered[tag_start..].find('>')? + tag_start;
        let tag = &html[tag_start..tag_end];
        if let Some(href) = attribute(tag, "href") {
            return Some(absolute(page, &href));
        }
    }
    // Every site has one here whether it says so or not.
    Some(absolute(page, "/favicon.ico"))
}

pub fn attribute(tag: &str, name: &str) -> Option<String> {
    let lowered = tag.to_lowercase();
    let key = format!("{name}=\"");
    let at = lowered.find(&key)? + key.len();
    let end = tag[at..].find('"')? + at;
    Some(tag[at..end].to_owned())
}

/// Resolves a page-relative reference against the page it came from.
pub fn absolute(page: &str, href: &str) -> String {
    if href.starts_with("http") {
        return href.to_owned();
    }
    let scheme = if page.starts_with("http://") {
        "http"
    } else {
        "https"
    };
    let host = host_of(page);
    if let Some(rest) = href.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    if href.starts_with('/') {
        return format!("{scheme}://{host}{href}");
    }
    format!("{scheme}://{host}/{href}")
}
