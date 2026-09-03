//! Entity resolution against a real bundle and a real index.
//!
//! The match call is a scripted stand-in, never a model. A test whose setup is a model call
//! measures two things and fails for the wrong reasons; here the thing under test is the blocking
//! and the decision handling, both of which are ours.

use std::sync::Mutex;

use async_trait::async_trait;
use loki_core::memory::bundle::Bundle;
use loki_core::memory::index::{Blocking, Candidate, Index};
use loki_core::memory::resolve::{
    Decision, Kind, MAX_CANDIDATES, Matcher, Resolution, ResolveError, resolve,
};

fn person(name: &str, aliases: &[&str], tags: &[&str]) -> String {
    let alias_block = if aliases.is_empty() {
        String::new()
    } else {
        let lines: String = aliases.iter().map(|a| format!("- {a}\n")).collect();
        format!("aliases:\n{lines}")
    };
    let tag_block = if tags.is_empty() {
        String::new()
    } else {
        let lines: String = tags.iter().map(|t| format!("- {t}\n")).collect();
        format!("tags:\n{lines}")
    };
    format!(
        "---\nname: {name}\nstatus: stable\ngenerated:\n  by: loki/0.1\n  at: 2026-01-01\n\
         {alias_block}{tag_block}okf_version: '0.2'\n---\n\n\
         ## Role\n- Works somewhere\n  valid_from: 2026-01-01   valid_to: null\n  \
         learned: 2026-01-01   unlearned: null\n  confidence: high   source: stated\n"
    )
}

/// Answers whatever the test told it to, and records what it was asked.
struct Scripted {
    answer: Decision,
    seen: Mutex<Vec<Vec<Candidate>>>,
}

impl Scripted {
    fn new(answer: Decision) -> Self {
        Self {
            answer,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.seen.lock().expect("lock").len()
    }

    fn last(&self) -> Vec<Candidate> {
        self.seen
            .lock()
            .expect("lock")
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Matcher for Scripted {
    async fn decide(
        &self,
        _surface: &str,
        _claim: &str,
        candidates: &[Candidate],
    ) -> Result<Decision, ResolveError> {
        self.seen.lock().expect("lock").push(candidates.to_vec());
        Ok(self.answer.clone())
    }
}

struct Store {
    index: Index,
    dir: std::path::PathBuf,
}

impl Store {
    async fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "loki-resolve-{}-{label}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::open(&dir).await.expect("open");
        {
            let writer = bundle.writer().await;
            writer
                .write(
                    "people/meera.md",
                    &person("Meera", &["Meera Raghunathan", "M"], &["person", "infra"]),
                )
                .expect("meera");
            writer
                .write(
                    "people/meera-shah.md",
                    &person("Meera Shah", &[], &["person", "design"]),
                )
                .expect("meera shah");
            writer
                .write("projects/loki.md", &person("Loki", &[], &["project"]))
                .expect("loki");
            writer
                .write("people/dan.md", &person("Dan", &[], &["person"]))
                .expect("dan");
        }
        let index = Index::in_memory().expect("index");
        {
            let reader = bundle.reader().await;
            index.sync(&reader).expect("sync");
        }
        Self { index, dir }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn an_exact_name_blocks_strongest() {
    let store = Store::new("exact").await;
    let found = store
        .index
        .candidates("Meera", &[], MAX_CANDIDATES)
        .expect("block");

    assert_eq!(
        found.first().map(|c| c.path.as_str()),
        Some("people/meera.md")
    );
    assert_eq!(found.first().map(|c| c.why), Some(Blocking::ExactName));
}

#[tokio::test]
async fn an_alias_finds_the_entity_its_name_does_not() {
    let store = Store::new("alias").await;
    let found = store
        .index
        .candidates("Meera Raghunathan", &[], MAX_CANDIDATES)
        .expect("block");

    let hit = found
        .iter()
        .find(|c| c.path == "people/meera.md")
        .expect("meera by alias");
    assert_eq!(hit.why, Blocking::Alias);
}

#[tokio::test]
async fn a_near_miss_still_blocks() {
    let store = Store::new("near").await;
    let found = store
        .index
        .candidates("Meara", &[], MAX_CANDIDATES)
        .expect("block");

    assert!(
        found.iter().any(|c| c.path == "people/meera.md"),
        "a typo should still surface the entity: {found:?}"
    );
}

#[tokio::test]
async fn a_shared_tag_blocks_when_the_name_does_not_match() {
    let store = Store::new("tags").await;
    let found = store
        .index
        .candidates(
            "Someone Entirely New",
            &["design".to_string()],
            MAX_CANDIDATES,
        )
        .expect("block");

    assert_eq!(
        found.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["people/meera-shah.md"]
    );
    assert_eq!(found[0].why, Blocking::SharedTags);
}

#[tokio::test]
async fn blocking_never_exceeds_five_candidates() {
    let store = Store::new("cap").await;
    let found = store
        .index
        .candidates(
            "Meera",
            &["person".to_string(), "project".to_string()],
            MAX_CANDIDATES,
        )
        .expect("block");

    assert!(found.len() <= MAX_CANDIDATES, "{found:?}");
}

/// The case that makes import affordable: nothing to compare against, so nothing to pay for.
#[tokio::test]
async fn an_unknown_entity_costs_no_model_call() {
    let store = Store::new("unknown").await;
    let matcher = Scripted::new(Decision::New);

    let out = resolve(
        "Priya Venkatesan",
        &[],
        "Priya runs the platform team",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    assert_eq!(
        matcher.calls(),
        0,
        "blocking found nothing, so nothing to ask"
    );
    assert_eq!(
        out,
        Resolution::New {
            path: "people/priya-venkatesan.md".to_string(),
            aliases: vec!["Priya Venkatesan".to_string()],
        }
    );
}

#[tokio::test]
async fn a_match_resolves_into_the_existing_file() {
    let store = Store::new("match").await;
    let matcher = Scripted::new(Decision::Existing(0));

    let out = resolve(
        "Meera",
        &[],
        "Meera moved to the infra team",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    assert_eq!(matcher.calls(), 1);
    assert!(
        matcher.last().len() <= MAX_CANDIDATES,
        "the match call must see a bounded set"
    );
    assert_eq!(
        out,
        Resolution::Existing {
            path: "people/meera.md".to_string()
        }
    );
}

/// §9.4's known failure. Two people with the same name, so create neither.
#[tokio::test]
async fn a_genuine_tie_creates_neither() {
    let store = Store::new("tie").await;
    let matcher = Scripted::new(Decision::Tie(vec![0, 1]));

    let out = resolve(
        "Meera",
        &[],
        "Meera said she would send it over",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    let Resolution::Ambiguous { between } = out else {
        panic!("expected an ambiguous resolution, got {out:?}");
    };
    assert_eq!(between.len(), 2, "{between:?}");
    assert!(
        between.iter().all(|p| p.starts_with("people/")),
        "{between:?}"
    );
}

/// Merging onto the wrong entity is the mistake §21.2 measures, so a nonsense index has to fail
/// towards a new file rather than towards a merge.
#[tokio::test]
async fn an_out_of_range_match_becomes_a_new_entity() {
    let store = Store::new("oob").await;
    let matcher = Scripted::new(Decision::Existing(99));

    let out = resolve(
        "Meera",
        &[],
        "Meera moved teams",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    assert!(matches!(out, Resolution::New { .. }), "{out:?}");
}

#[tokio::test]
async fn a_tie_that_names_one_candidate_is_not_a_tie() {
    let store = Store::new("thin-tie").await;
    let matcher = Scripted::new(Decision::Tie(vec![0]));

    let out = resolve(
        "Meera",
        &[],
        "Meera moved teams",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    assert!(matches!(out, Resolution::New { .. }), "{out:?}");
}

#[tokio::test]
async fn an_empty_surface_form_is_refused() {
    let store = Store::new("empty").await;
    let matcher = Scripted::new(Decision::New);

    let out = resolve(
        "   ",
        &[],
        "something",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await;

    assert!(matches!(out, Err(ResolveError::NoSurfaceForm)), "{out:?}");
}

/// A renamed or deleted entity must stop blocking, or resolution keeps offering a file that is
/// no longer there.
#[tokio::test]
async fn a_removed_entity_stops_being_a_candidate() {
    let store = Store::new("removed").await;
    assert!(
        !store
            .index
            .candidates("Dan", &[], MAX_CANDIDATES)
            .expect("block")
            .is_empty()
    );

    std::fs::remove_file(store.dir.join("people/dan.md")).expect("remove");
    let bundle = Bundle::open(&store.dir).await.expect("reopen");
    {
        let reader = bundle.reader().await;
        store.index.sync(&reader).expect("sync");
    }

    assert!(
        store
            .index
            .candidates("Dan", &[], MAX_CANDIDATES)
            .expect("block")
            .is_empty(),
        "a deleted entity is still blocking"
    );
}

/// B-29, and §9.4's "identity is the entity, not the directory". The same surface extracted once
/// as a project and once as a person must not become two files, because blocking would then see
/// two candidates for one thing and the store would look like it writes a file per fact.
#[tokio::test]
async fn one_surface_under_two_kinds_resolves_to_one_file() {
    let store = Store::new("cross-kind").await;
    // The matcher declines: `Loki` already exists as a project, and the incoming claim reads like
    // a person fact, so the question it was asked has a defensible "no".
    let matcher = Scripted::new(Decision::New);

    let resolved = resolve(
        "Loki",
        &[],
        "Loki prefers short replies",
        Kind::Person,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    assert_eq!(
        resolved,
        Resolution::Existing {
            path: "projects/loki.md".to_string()
        },
        "an exact name already on disk is the same entity, whatever directory it landed in"
    );
}

/// The override is about identity, not about the claim. A name nothing matches still creates.
#[tokio::test]
async fn a_name_nothing_matches_still_creates_its_own_file() {
    let store = Store::new("cross-kind-distinct").await;
    let matcher = Scripted::new(Decision::New);

    let resolved = resolve(
        "Meera",
        &[],
        "Meera works on infra",
        Kind::Preference,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");

    // `Meera` is an exact name under `people/`, so identity wins there too.
    assert_eq!(
        resolved,
        Resolution::Existing {
            path: "people/meera.md".to_string()
        }
    );

    let novel = resolve(
        "Quarterly review",
        &[],
        "Sabharish runs it on Fridays",
        Kind::Project,
        None,
        &store.index,
        &matcher,
    )
    .await
    .expect("resolve");
    assert!(matches!(novel, Resolution::New { .. }), "{novel:?}");
}
