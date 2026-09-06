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
use loki_core::adapters::politeness::{Politeness, Shared};
use loki_core::adapters::reader::Reader;
use loki_core::core::sink::Broadcast;
use loki_core::core::websearch::Search;
use loki_core::ports::egress::{Egress, Outbound};
use tokio_util::sync::CancellationToken;

/// Loki's own browser, launched the way the app launches it.
async fn browser() -> Arc<loki_core::adapters::browser::Browsing> {
    use loki_core::ports::egress::{Delegate, Policy};
    let events = Arc::new(Broadcast::new());
    let http = loki_core::adapters::egress::Http::new(events).expect("egress");
    let delegated = http
        .delegate(Policy::for_target("duckduckgo.com"))
        .await
        .expect("exit");
    Arc::new(
        loki_core::adapters::browser::Browsing::new(
            Arc::new(delegated),
            std::env::temp_dir().join("loki-live-web-profile"),
            9334,
            Arc::new(Politeness::default()),
        )
        .expect("a chromium-family browser"),
    )
}

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
    let browsing = browser().await;
    let search = Search {
        engines: vec![
            Arc::new(loki_core::adapters::duckduckgo::DuckDuckGo::new(
                Arc::clone(&egress),
                Arc::clone(&gate),
            )),
            Arc::clone(&browsing) as Arc<dyn loki_core::ports::search::Discover>,
        ],
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

/// Whether rung 1 discovery works once the client is a returning visitor (§12.2).
///
/// **Cold and warm, because that is the whole question.** Cold is one search request from a client
/// with no history, which is what every earlier measurement did. Warm visits the engine's own home
/// page first and then searches on the same client, which is what a browser does and what a cookie
/// jar is for. If the two differ, the earlier conclusion that every engine refuses this machine was
/// taken against a client missing its most important part.
#[tokio::test]
#[ignore = "needs the network"]
async fn which_engines_answer_this_machine() {
    let engines = [
        (
            "ddg-html",
            "https://duckduckgo.com/",
            "https://html.duckduckgo.com/html/?q=kerala+news+today",
        ),
        (
            "ddg-lite",
            "https://duckduckgo.com/",
            "https://lite.duckduckgo.com/lite/?q=kerala+news+today",
        ),
        (
            "bing",
            "https://www.bing.com/",
            "https://www.bing.com/search?q=kerala+news+today",
        ),
        (
            "google",
            "https://www.google.com/",
            "https://www.google.com/search?q=kerala+news+today",
        ),
        (
            "brave",
            "https://search.brave.com/",
            "https://search.brave.com/search?q=kerala+news+today",
        ),
        (
            "startpage",
            "https://www.startpage.com/",
            "https://www.startpage.com/sp/search?query=kerala+news+today",
        ),
        (
            "mojeek",
            "https://www.mojeek.com/",
            "https://www.mojeek.com/search?q=kerala+news+today",
        ),
        (
            "ecosia",
            "https://www.ecosia.org/",
            "https://www.ecosia.org/search?q=kerala+news+today",
        ),
    ];

    println!(
        "{:12} {:>22}   {:>22}",
        "engine", "cold", "warm (home first)"
    );
    for (name, home, search) in engines {
        // A fresh exit per engine, so each cold run really is cold and the warm run's only
        // advantage is the home page it just visited.
        let cold = report(&exit(), None, search).await;
        let shared = exit();
        let warm = report(&shared, Some(home), search).await;
        println!("{name:12} {cold:>22}   {warm:>22}");
    }
}

/// One measurement: optionally warm the jar, then search, and say what came back.
async fn report(egress: &Arc<dyn Egress>, home: Option<&str>, search: &str) -> String {
    if let Some(home) = home {
        let _ = fetch(egress, home).await;
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }
    match fetch(egress, search).await {
        Some((status, body)) => {
            let host = search.split('/').nth(2).unwrap_or("");
            let root = host.trim_start_matches("www.").trim_start_matches("html.");
            // Links leaving the engine, plus the wrapped ones, which is where the big engines
            // actually keep their results.
            let links = body
                .match_indices("href=\"http")
                .filter(|(at, _)| {
                    let rest = &body[at + 6..];
                    !rest[..rest.find('"').unwrap_or(0)].contains(root)
                })
                .count()
                + body.matches("/ck/a?").count()
                + body.matches("uddg=").count();
            let lowered = body.to_lowercase();
            let challenged = lowered.contains("captcha")
                || lowered.contains("unusual traffic")
                || lowered.contains("are you a robot");
            format!(
                "{status} {:>6}b l={links}{}",
                body.len(),
                if challenged { " CHALLENGED" } else { "" }
            )
        }
        None => "unreachable".to_owned(),
    }
}

async fn fetch(egress: &Arc<dyn Egress>, url: &str) -> Option<(u16, String)> {
    let mut landed = egress
        .send(Outbound::get(url).as_browser(), CancellationToken::new())
        .await
        .ok()?;
    let status = landed.status;
    let mut raw = Vec::new();
    while let Some(chunk) = landed.body.next().await {
        raw.extend_from_slice(&chunk.ok()?);
    }
    Some((status, String::from_utf8_lossy(&raw).into_owned()))
}

/// Rung 1 discovery exactly as the ladder would run it, now that the client keeps cookies.
#[tokio::test]
#[ignore = "needs the network"]
async fn rung_one_discovery_with_a_cookie_jar() {
    use loki_core::adapters::duckduckgo::DuckDuckGo;
    use loki_core::ports::search::Discover;

    let egress = exit();
    let gate: Shared = Arc::new(Politeness::default());
    let ddg = DuckDuckGo::new(Arc::clone(&egress), gate);

    // The home page first, so the jar has whatever the engine hands out.
    let _ = fetch(&egress, "https://duckduckgo.com/").await;

    match ddg
        .search("kerala news today", CancellationToken::new())
        .await
    {
        Ok(hits) => {
            println!("rung 1 returned {} hits", hits.len());
            for hit in hits.iter().take(5) {
                println!("  {} :: {}", hit.title, hit.url);
            }
        }
        Err(e) => println!("rung 1 failed: {e}"),
    }
}

/// Which engines a real browser can actually get results out of.
///
/// Measured through `Discover`, which reads the rendered HTML. Reading `Page::text` instead gives
/// zero for every engine and means nothing: readability is built to keep an article and throw away
/// a list of links, and a results page is a list of links.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the network"]
async fn which_engines_a_browser_can_read() {
    use loki_core::adapters::browser::Engine;
    use loki_core::ports::egress::{Delegate, Policy};
    use loki_core::ports::search::Discover;

    let events = Arc::new(Broadcast::new());
    let http = loki_core::adapters::egress::Http::new(events).expect("egress");
    let delegated = http
        .delegate(Policy::for_target("example.com"))
        .await
        .expect("exit");

    for engine in [
        Engine::BING,
        Engine::GOOGLE,
        Engine::BRAVE,
        Engine::STARTPAGE,
        Engine::MOJEEK,
        Engine::DUCKDUCKGO,
    ] {
        let browsing = loki_core::adapters::browser::Browsing::new(
            Arc::new(
                http.delegate(Policy::for_target(engine.host))
                    .await
                    .expect("exit"),
            ),
            std::env::temp_dir().join("loki-probe-profile"),
            9336,
            Arc::new(Politeness::default()),
        )
        .expect("a chromium-family browser")
        .searching_with(engine);

        let started = std::time::Instant::now();
        match Discover::search(&browsing, "kerala news today", CancellationToken::new()).await {
            Ok(hits) => {
                println!(
                    "{:10} {:>5}ms hits={}",
                    engine.id,
                    started.elapsed().as_millis(),
                    hits.len()
                );
                for hit in hits.iter().take(3) {
                    println!("             {} :: {}", hit.title, hit.url);
                }
            }
            Err(e) => println!(
                "{:10} {:>5}ms {e}",
                engine.id,
                started.elapsed().as_millis()
            ),
        }
    }
    let _ = delegated;
}

/// Where a browser search actually spends its time, step by step.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the network"]
async fn where_the_browser_search_spends_its_time() {
    use loki_core::ports::egress::{Delegate, Policy};
    let mark = std::time::Instant::now();
    let events = Arc::new(Broadcast::new());
    let http = loki_core::adapters::egress::Http::new(events).expect("egress");
    eprintln!("[{:>6}ms] egress up", mark.elapsed().as_millis());

    let delegated = http
        .delegate(Policy::for_target("duckduckgo.com"))
        .await
        .expect("exit");
    eprintln!(
        "[{:>6}ms] proxy listening at {}",
        mark.elapsed().as_millis(),
        delegated.proxy_url()
    );

    let browsing = loki_core::adapters::browser::Browsing::new(
        Arc::new(delegated),
        std::env::temp_dir().join("loki-probe-profile"),
        9335,
        Arc::new(Politeness::default()),
    )
    .expect("a chromium-family browser");
    eprintln!("[{:>6}ms] browser found", mark.elapsed().as_millis());

    // What the page actually contained, before the parser had an opinion about it.
    {
        use loki_core::ports::search::Extract;
        if let Ok(page) = Extract::read(
            &browsing,
            "https://html.duckduckgo.com/html/?q=kerala+news+today",
            CancellationToken::new(),
        )
        .await
        {
            eprintln!(
                "[{:>6}ms] page verdict={:?} text={}b",
                mark.elapsed().as_millis(),
                page.verdict,
                page.text.len()
            );
            eprintln!(
                "---- first 600 chars of readable text ----\n{}",
                page.text.chars().take(600).collect::<String>()
            );
        }
    }

    let hits = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        loki_core::ports::search::Discover::search(
            &browsing,
            "kerala news headlines today",
            CancellationToken::new(),
        ),
    )
    .await;
    eprintln!("[{:>6}ms] search returned", mark.elapsed().as_millis());

    match hits {
        Ok(Ok(hits)) => {
            for hit in hits.iter().take(6) {
                eprintln!("  {} :: {}", hit.title, hit.url);
            }
            assert!(!hits.is_empty());
        }
        Ok(Err(e)) => panic!("search failed: {e}"),
        Err(_) => panic!("search never returned inside 60s"),
    }
}
