//! The working set: what memory says before anything is asked (§9.2).
//!
//! Generated, never hand-edited. It lives in the frozen prefix, so it changes once per session and
//! not per turn (§8.1). A correction edits the entity and the working set regenerates; editing the
//! derived file directly would make it stop being derived, and the next regeneration would discard
//! the edit.
//!
//! Capped, because this is paid on every single call of the session. §3 records the measured
//! version of getting that wrong: bootstrap files re-injected at 3 to 5k tokens on every message.

use jiff::civil::Date;

use super::bundle::{Bundle, BundleError, WORKING_SET};
use super::gate::{Active, TierScope};
use super::index::{Index, IndexError};

/// Characters the working set may occupy. Roughly a thousand tokens.
///
/// A cap in characters rather than tokens because the core has no tokenizer and must not depend on
/// one: a provider-specific count in Ring 0 would tie the frozen prefix to a provider.
pub const MAX_CHARS: usize = 4_000;

/// How many concepts are considered before the cap does the rest of the work.
const CANDIDATES: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum WorkingSetError {
    #[error("could not read or write the bundle: {0}")]
    Bundle(#[from] BundleError),
    #[error("could not read the index: {0}")]
    Index(#[from] IndexError),
}

/// What generation produced. `included` is what reached the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Generated {
    pub included: Vec<String>,
    /// Concepts that were eligible but did not fit the cap.
    pub dropped: Vec<String>,
    pub chars: usize,
}

/// Regenerates `working-set.md` and returns what went into it.
///
/// Ordered by usage, so what you actually rely on survives the cap and what you do not falls off
/// the end. Everything passes the §10.4 gate first, so a draft, deprecated or stale concept cannot
/// reach the prefix even if it ranks well.
///
/// # Errors
/// Fails if the bundle cannot be read or written, or the index cannot be read.
pub async fn generate(
    bundle: &Bundle,
    index: &Index,
    scope: TierScope,
    today: Date,
) -> Result<Generated, WorkingSetError> {
    let ranked = index.most_used(CANDIDATES)?;

    // Every card's display name, so an edge reads as "father: Vaidyanathan" rather than as a path.
    let names = display_names(bundle).await;

    let mut out = String::from("# Working set\n\n");
    let mut result = Generated::default();

    for path in ranked {
        let concept = {
            let reader = bundle.reader().await;
            match reader.load_concept(&path) {
                Ok(concept) => concept,
                // A file that will not parse is not a reason to ship an empty prefix.
                Err(_) => continue,
            }
        };
        let front = concept.front.clone();
        // The gate, not a status check. A draft cannot reach a prompt by any path.
        let Ok(active) = Active::try_from(concept, today, scope) else {
            continue;
        };
        let claims: Vec<&str> = active
            .visible_claims(scope, today)
            .map(|c| c.text.as_str())
            .collect();
        if claims.is_empty() {
            continue;
        }

        let mut block = format!("## {}\n", active.name());
        // What this is called and who it is connected to, in the prompt for the same reason they
        // are on the trust surface: a store that knows a nickname and cannot say it is
        // indistinguishable from one that never heard it (S-26). Asked "what is my dad called",
        // Loki had the name, the alias and the edge on disk and none of the three in front of it.
        let known_as: Vec<&str> = front
            .aliases
            .iter()
            .map(String::as_str)
            .filter(|form| {
                !form.eq_ignore_ascii_case(active.name()) && super::resolve::is_a_real_name(form)
            })
            .collect();
        if !known_as.is_empty() {
            block.push_str(&format!("- also called {}\n", known_as.join(", ")));
        }
        for edge in front.relations.iter().filter(|edge| edge.is_current()) {
            let who = names.get(&edge.to).map_or(edge.to.as_str(), String::as_str);
            block.push_str(&format!("- {}: {who}\n", edge.label));
        }
        for claim in claims {
            block.push_str("- ");
            block.push_str(claim);
            block.push('\n');
        }
        block.push('\n');

        if out.len() + block.len() > MAX_CHARS {
            result.dropped.push(path);
            continue;
        }
        out.push_str(&block);
        result.included.push(path);
    }

    result.chars = out.len();
    {
        let writer = bundle.writer().await;
        writer.write(WORKING_SET, &out)?;
    }
    Ok(result)
}

/// Every concept's display name, keyed by path.
async fn display_names(bundle: &Bundle) -> std::collections::HashMap<String, String> {
    let reader = bundle.reader().await;
    reader
        .concepts()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let name = reader.load_concept(&path).ok()?.front.name;
            Some((path, name))
        })
        .collect()
}

/// Reads the working set for the frozen prefix.
///
/// # Errors
/// Fails if the bundle cannot be read. A missing file is not an error: it means nothing has been
/// learned yet, and an empty prefix is the correct prompt for that.
pub async fn read(bundle: &Bundle) -> Result<String, WorkingSetError> {
    let reader = bundle.reader().await;
    Ok(reader.read(WORKING_SET).unwrap_or_default())
}
