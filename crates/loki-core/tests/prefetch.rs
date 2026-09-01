//! Pre-fetch: one union of consolidated claims and the session in progress (D-043).
//!
//! The promise being tested is that the user never has to say where something was said. A fact
//! from a past session and a sentence from twenty turns ago arrive by the same call.

use jiff::civil::{Date, date};
use loki_core::core::vocab::Locality;
use loki_core::memory::bundle::{Bundle, WORKING_SET};
use loki_core::memory::gate::TierScope;
use loki_core::memory::index::{Index, Origin, Query, Session};
use loki_core::memory::working_set;

fn concept(name: &str, status: &str, claims: &[&str]) -> String {
    let body: String = claims
        .iter()
        .map(|text| {
            format!(
                "- {text}\n  valid_from: 2026-01-01   valid_to: null\n  \
                 learned: 2026-01-01   unlearned: null\n  confidence: high   source: stated\n"
            )
        })
        .collect();
    format!(
        "---\nname: {name}\nstatus: {status}\ngenerated:\n  by: loki/0.1\n  at: 2026-01-01\n\
         okf_version: '0.2'\n---\n\n## Notes\n{body}"
    )
}

struct Store {
    bundle: Bundle,
    index: Index,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str, concepts: &[(&str, &str, &[&str])]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-prefetch-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).await.expect("open");
        {
            let writer = bundle.writer().await;
            for (path, status, claims) in concepts {
                let name = path
                    .rsplit('/')
                    .next()
                    .and_then(|f| f.strip_suffix(".md"))
                    .unwrap_or(path);
                writer
                    .write(path, &concept(name, status, claims))
                    .expect("write concept");
            }
        }
        let index = Index::in_memory().expect("index");
        {
            let reader = bundle.reader().await;
            index.sync(&reader).expect("sync");
        }
        Self { bundle, index, dir }
    }

    fn scope(&self) -> TierScope {
        TierScope::normal(Locality::Cloud)
    }

    fn today(&self) -> Date {
        date(2026, 9, 1)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The whole point. One call, and the caller cannot tell which corpus answered.
#[tokio::test]
async fn recall_spans_past_claims_and_the_live_session_in_one_call() {
    let store = Store::new(
        "union",
        &[("people/meera.md", "stable", &["Meera runs the infra team"])],
    )
    .await;
    store
        .index
        .record_turn("today", 1, "user", "the deploy window is Thursday at four")
        .expect("record");

    let session = Session {
        id: "today",
        window_starts_at: 20,
    };

    let infra = store
        .index
        .recall(
            &Query::prefetch("who runs infra", store.scope(), store.today(), 5).spanning(session),
        )
        .expect("recall");
    assert_eq!(
        infra.first().map(|r| r.origin),
        Some(Origin::Claim),
        "{infra:?}"
    );

    let deploy = store
        .index
        .recall(
            &Query::prefetch("when is the deploy window", store.scope(), store.today(), 5)
                .spanning(session),
        )
        .expect("recall");
    assert_eq!(
        deploy.first().map(|r| r.origin),
        Some(Origin::Session),
        "the session's own turn has to come back: {deploy:?}"
    );
}

/// Turns still in the prompt must not be retrieved into the prompt again.
#[tokio::test]
async fn turns_still_inside_the_window_are_not_recalled() {
    let store = Store::new("window", &[]).await;
    store
        .index
        .record_turn("today", 30, "user", "the deploy window is Thursday")
        .expect("record");

    let found = store
        .index
        .recall(
            &Query::prefetch("deploy window", store.scope(), store.today(), 5).spanning(Session {
                id: "today",
                window_starts_at: 20,
            }),
        )
        .expect("recall");

    assert!(
        found.is_empty(),
        "turn 30 is still in the window: {found:?}"
    );
}

/// D-045. Past sessions are covered by claims, so their raw turns stay out of automatic recall.
#[tokio::test]
async fn another_sessions_turns_are_not_recalled() {
    let store = Store::new("other", &[]).await;
    store
        .index
        .record_turn("yesterday", 1, "user", "the deploy window is Thursday")
        .expect("record");

    let found = store
        .index
        .recall(
            &Query::prefetch("deploy window", store.scope(), store.today(), 5).spanning(Session {
                id: "today",
                window_starts_at: 20,
            }),
        )
        .expect("recall");

    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn without_a_session_only_claims_are_searched() {
    let store = Store::new("claims-only", &[]).await;
    store
        .index
        .record_turn("today", 1, "user", "the deploy window is Thursday")
        .expect("record");

    let found = store
        .index
        .recall(&Query::prefetch(
            "deploy window",
            store.scope(),
            store.today(),
            5,
        ))
        .expect("recall");

    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn a_revised_turn_replaces_itself_rather_than_duplicating() {
    let store = Store::new("revise", &[]).await;
    store
        .index
        .record_turn("today", 1, "user", "deploy on Thursday")
        .expect("first");
    store
        .index
        .record_turn("today", 1, "user", "deploy on Friday")
        .expect("second");

    let found = store
        .index
        .recall(
            &Query::prefetch("deploy", store.scope(), store.today(), 5).spanning(Session {
                id: "today",
                window_starts_at: 20,
            }),
        )
        .expect("recall");

    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].text.contains("Friday"), "{found:?}");
}

#[tokio::test]
async fn a_consolidated_session_is_forgotten_from_the_live_corpus() {
    let store = Store::new("forget", &[]).await;
    store
        .index
        .record_turn("today", 1, "user", "deploy on Thursday")
        .expect("record");
    store.index.forget_session("today").expect("forget");

    let found = store
        .index
        .recall(
            &Query::prefetch("deploy", store.scope(), store.today(), 5).spanning(Session {
                id: "today",
                window_starts_at: 20,
            }),
        )
        .expect("recall");

    assert!(found.is_empty(), "{found:?}");
}

/// The gate, not a status check: nothing draft or deprecated reaches the frozen prefix.
#[tokio::test]
async fn the_working_set_holds_only_what_passed_the_gate() {
    let store = Store::new(
        "gate",
        &[
            ("people/meera.md", "stable", &["Meera runs the infra team"]),
            ("people/dan.md", "draft", &["Dan might like tea"]),
            ("people/old.md", "deprecated", &["Old thing"]),
        ],
    )
    .await;

    let out = working_set::generate(&store.bundle, &store.index, store.scope(), store.today())
        .await
        .expect("generate");

    assert_eq!(out.included, ["people/meera.md"], "{out:?}");

    let reader = store.bundle.reader().await;
    let text = reader.read(WORKING_SET).expect("working set");
    assert!(text.contains("Meera runs the infra team"), "{text}");
    assert!(!text.contains("Dan"), "a draft reached the prefix: {text}");
    assert!(
        !text.contains("Old thing"),
        "a deprecated concept reached the prefix: {text}"
    );
}

/// §3's measured failure: a bootstrap file re-injected at 3 to 5k tokens on every message.
#[tokio::test]
async fn the_working_set_is_capped() {
    let filler: Vec<String> = (0..40)
        .map(|n| format!("a reasonably long claim number {n} about something or other"))
        .collect();
    let claims: Vec<&str> = filler.iter().map(String::as_str).collect();
    let concepts: Vec<(&str, &str, &[&str])> = vec![
        ("people/a.md", "stable", claims.as_slice()),
        ("people/b.md", "stable", claims.as_slice()),
        ("people/c.md", "stable", claims.as_slice()),
    ];
    let store = Store::new("cap", &concepts).await;

    let out = working_set::generate(&store.bundle, &store.index, store.scope(), store.today())
        .await
        .expect("generate");

    assert!(out.chars <= working_set::MAX_CHARS, "{} chars", out.chars);
    assert!(!out.dropped.is_empty(), "something had to fall off the end");
}

#[tokio::test]
async fn an_empty_store_produces_an_empty_working_set() {
    let store = Store::new("empty", &[]).await;

    let out = working_set::generate(&store.bundle, &store.index, store.scope(), store.today())
        .await
        .expect("generate");

    assert!(out.included.is_empty());
    let text = working_set::read(&store.bundle).await.expect("read");
    assert!(text.contains("Working set"), "{text}");
}
