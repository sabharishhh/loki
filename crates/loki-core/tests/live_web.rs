//! The web as it actually answers this machine (§21.5's canary, run by hand).
//!
//! Ignored by default: it needs the network and it is not deterministic. It exists because
//! discovery is the half that fails silently. An engine that soft blocks returns a page with a
//! success-ish status and no results in it, which is indistinguishable from a query nobody has
//! written about unless something looks.
//!
//! `cargo test -p loki-core --test live_web -- --ignored --nocapture`

use std::sync::Arc;

use futures_util::StreamExt;
use loki_core::adapters::clock::SystemClock;
use loki_core::adapters::duckduckgo::DuckDuckGo;
use loki_core::adapters::politeness::{Politeness, Shared};
use loki_core::adapters::reader::Reader;
use loki_core::core::sink::Broadcast;
use loki_core::core::websearch::Search;
use loki_core::ports::egress::{Egress, Outbound};
use tokio_util::sync::CancellationToken;

fn exit() -> Arc<dyn Egress> {
    let events = Arc::new(Broadcast::new());
    Arc::new(loki_core::adapters::egress::Http::new(events).expect("egress"))
}

/// What the app does on a question about today, end to end.
#[tokio::test]
#[ignore = "needs the network"]
async fn a_question_about_today_comes_back_with_sources() {
    let egress = exit();
    let gate: Shared = Arc::new(Politeness::default());
    let search = Search {
        discover: Arc::new(DuckDuckGo::new(Arc::clone(&egress), Arc::clone(&gate))),
        rungs: vec![Arc::new(Reader::new(
            Arc::clone(&egress),
            Arc::clone(&gate),
        ))],
        clock: Arc::new(SystemClock),
        budget: loki_core::core::attempt::Budget::of_steps(6)
            .within(std::time::Duration::from_secs(30)),
        reads: 3,
        evidence: None,
        egress: Some(egress),
    };

    let found = search
        .run("kerala news headlines today", CancellationToken::new())
        .await
        .expect("the search ran");

    for source in &found.sources {
        println!("read={} {} {}", source.read, source.title, source.url);
    }
    assert!(!found.sources.is_empty(), "a live search returned nothing");
}

/// Which engines will talk to this address at all.
///
/// Run this first when a search starts failing. Offsite links are the only honest sign of results:
/// a challenge page is the right size, returns 200, and links only to itself.
#[tokio::test]
#[ignore = "needs the network"]
async fn which_engines_answer_this_machine() {
    let egress = exit();
    let engines = [
        (
            "ddg-html",
            "https://html.duckduckgo.com/html/?q=kerala+news+today",
        ),
        (
            "ddg-lite",
            "https://lite.duckduckgo.com/lite/?q=kerala+news+today",
        ),
        (
            "mojeek",
            "https://www.mojeek.com/search?q=kerala+news+today",
        ),
        (
            "brave",
            "https://search.brave.com/search?q=kerala+news+today",
        ),
        ("bing", "https://www.bing.com/search?q=kerala+news+today"),
        (
            "yandex",
            "https://yandex.com/search/?text=kerala+news+today",
        ),
    ];

    for (name, url) in engines {
        match egress
            .send(Outbound::get(url).as_browser(), CancellationToken::new())
            .await
        {
            Ok(mut landed) => {
                let status = landed.status;
                let mut raw = Vec::new();
                while let Some(chunk) = landed.body.next().await {
                    if let Ok(chunk) = chunk {
                        raw.extend_from_slice(&chunk);
                    }
                }
                let body = String::from_utf8_lossy(&raw);
                let root = url
                    .split('/')
                    .nth(2)
                    .unwrap_or("")
                    .trim_start_matches("www.");
                let offsite = body
                    .match_indices("href=\"http")
                    .filter(|(at, _)| {
                        let rest = &body[at + 6..];
                        !rest[..rest.find('"').unwrap_or(0)].contains(root)
                    })
                    .count();
                println!(
                    "{name:12} {status} {:>7}b offsite_links={offsite}",
                    body.len()
                );
            }
            Err(e) => println!("{name:12} unreachable: {e}"),
        }
    }
}
