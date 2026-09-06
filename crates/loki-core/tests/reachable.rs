//! Everything shipped has a caller, and the exceptions are written down.
//!
//! **Seven times in one session, something was built, tested, and never called.** A rail nothing
//! wrote to (B-73), a marker nothing parsed (B-74), the entire web subsystem (B-76), four citation
//! views (B-81), the citation checker, rung 2's `Extract` impl, and the evidence store. Every one
//! compiled. Every one had unit tests that passed. Not one of them ran in the product.
//!
//! **The compiler cannot see this and it is not its fault.** `loki-core` is a library, so a `pub`
//! item with no callers is presumed to have them outside the crate and `dead_code` stays silent.
//! In a binary it would have caught most of these on the first build.
//!
//! **Tests are what made it invisible.** Each of the seven had a test exercising it, so the item
//! was referenced, the suite was green, and nothing anywhere said the product could not reach it.
//! So the rule is not "has a caller", which they all had. It is **"has a caller that is not a
//! test"**.
//!
//! The list below is the point of the check rather than an escape from it. Something in it is
//! built ahead of its caller on purpose, and saying which and why is a sentence. Something missing
//! from it is a subsystem nobody can use.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Built ahead of its caller, on purpose, with the reason.
///
/// Deliberately literal, like `rings.rs`. A check nobody can read is a check somebody deletes.
const AHEAD_OF_ITS_CALLER: [(&str, &str); 30] = [
    // Test doubles. Being used only by tests is their entire job.
    ("advance", "FakeClock's hand, for tests"),
    ("utc", "FakeClock's constructor, for tests"),
    // Rung 3, designed and not shipped (§12.4). It drives the session directly rather than
    // through `page::read`, which is why these exist with nothing calling them yet.
    ("clear_challenge", "rung 3, §12.4"),
    ("wait_for_load", "rung 3, §12.4"),
    ("searching_with", "the engine is a deployment knob, D-092"),
    // Phase 4: §13's tool tiers and §15's connectors.
    ("tier_of", "Phase 4, §13.3"),
    ("missing", "Phase 4, §13.3's grant check"),
    // Surfaces whose screen is not built. Each is the query a screen in `docs/` will ask.
    ("by_day", "the spend screen, §19.2"),
    ("by_task", "the spend screen, §19.2"),
    ("spent_since", "the spend screen, §19.2"),
    ("commit_count", "the trust surface, §17.3"),
    ("load_concept_at", "the trust surface's history view, §17.3"),
    ("claim_count", "the trust surface, §17.3"),
    ("forget_session", "§18.4's session delete"),
    (
        "is_complete",
        "§17.3 asks whether a whole answer was sourced",
    ),
    (
        "gap_is_notable",
        "§8.3's framing, for a surface that names the gap",
    ),
    // §18.4's resume path, which invalidates a checkpoint against what moved under it.
    ("against_memory", "§18.4's resume"),
    ("invalidate_from", "§18.4's resume"),
    ("memory_moved", "§18.4's resume"),
    // Reachable through the thing that owns them, which the check cannot see.
    (
        "is_newsworthy",
        "gates the consolidation path that calls it",
    ),
    (
        "default_root",
        "the bundle's default, taken by Memory::open",
    ),
    (
        "rebuild",
        "the index's full pass, run by the tooling rather than the app",
    ),
    // S2, and this is what the check is for. `docs/` §12.7 says a claim from a fetch carries an
    // `EvidenceRef`, and nothing has ever built one.
    (
        "citing",
        "S2: consolidation does not carry an EvidenceRef yet",
    ),
    ("sweep", "S2: never called from consolidation"),
    (
        "holds",
        "S2: the store's own membership check, unused until the sweep runs",
    ),
    // Suspected stranded rather than deliberate. Named so the suite is honest about the state
    // instead of green by omission. Tracked as W12 in PLAN.md.
    ("set_recalled", "W12: suspected stranded, the shape of B-73"),
    ("is_eligible_for_prefetch", "W12: suspected stranded"),
    ("build_prompt_text", "W12: suspected stranded"),
    (
        "blocking",
        "W12: the browser policy's block list is never populated",
    ),
    (
        "control_url",
        "W12: superseded by page_socket, likely deletable",
    ),
];

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
    found
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root is two above this crate")
        .to_path_buf()
}

/// A file's code with every `#[cfg(test)]` module removed.
///
/// **Brace-matched, not truncated at the first marker.** Cutting at the first `#[cfg(test)]`
/// discarded everything after it, and `browser.rs` puts a test module in the middle, so half that
/// file's production code was invisible and its callers with it.
fn production_code(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };

    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(open) = after.find('{') else { break };
        let mut depth = 0;
        let mut end = None;
        for (offset, character) in after[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        rest = &after[end..];
    }
    out.push_str(rest);

    // **Comments are prose and prose is not a caller.** Without this, a name like `check` or
    // `sweep` matches the English in somebody's doc comment and every stranded item looks wired.
    // Found by mutation: un-wiring `citations::check` left this test green.
    out.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn names_in(text: &str, keyword: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(keyword)?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// How many times `name` appears as a whole word in `text`.
fn times_named(text: &str, name: &str) -> usize {
    let mut count = 0;
    let mut from = 0;
    while let Some(at) = text[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if bounded(before) && bounded(after) {
            count += 1;
        }
        from = end;
    }
    count
}

/// Whether `name` appears as a word anywhere in `text`.
fn mentions(text: &str, name: &str) -> bool {
    let mut from = 0;
    while let Some(at) = text[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if bounded(before) && bounded(after) {
            return true;
        }
        from = end;
    }
    false
}

#[test]
fn everything_public_has_a_caller_that_is_not_a_test() {
    let root = workspace();
    let mine = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    // Where a name may legitimately be used: the whole workspace's source, minus test files, minus
    // the `#[cfg(test)]` module of every file, minus the file the item is declared in.
    let mut production: Vec<(PathBuf, String)> = Vec::new();
    for crate_dir in ["crates/loki-core", "crates/loki-ffi", "crates/loki-cli"] {
        for file in rust_files(&root.join(crate_dir).join("src")) {
            production.push((file.clone(), production_code(&file)));
        }
    }

    let allowed: BTreeSet<&str> = AHEAD_OF_ITS_CALLER.iter().map(|(name, _)| *name).collect();
    let mut stranded = Vec::new();

    for file in rust_files(&mine) {
        let code = production_code(&file);
        let mut declared = names_in(&code, "pub fn ");
        declared.extend(names_in(&code, "pub async fn "));
        declared.extend(names_in(&code, "pub struct "));

        for name in declared {
            if allowed.contains(name.as_str()) {
                continue;
            }
            // **Its own file counts, minus the declaration itself.** A helper used only by the
            // module that declares it is doing its job; an item named exactly once anywhere is a
            // declaration and nothing else.
            let named: usize = production
                .iter()
                .map(|(_, text)| times_named(text, &name))
                .sum();
            if named <= 1 {
                stranded.push(format!(
                    "  {}  {name}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }

    stranded.sort();
    stranded.dedup();
    assert!(
        stranded.is_empty(),
        "built and never called from anything but a test.\n\
         Wire it up, delete it, or name it in AHEAD_OF_ITS_CALLER with the reason:\n{}",
        stranded.join("\n")
    );
}

/// The list is a record, not a drawer. An entry for something that no longer exists is a reason
/// nobody has read in a long time.
#[test]
fn nothing_in_the_list_has_since_been_wired_or_deleted() {
    let mine = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let all: String = rust_files(&mine)
        .iter()
        .map(|file| fs::read_to_string(file).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    let gone: Vec<&str> = AHEAD_OF_ITS_CALLER
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !mentions(&all, name))
        .collect();

    assert!(
        gone.is_empty(),
        "named as built ahead of its caller and no longer declared anywhere: {gone:?}"
    );
}
