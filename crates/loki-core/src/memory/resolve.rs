//! Entity resolution: blocking, then matching (§9.4).
//!
//! One entity, one file. A new claim about Meera lands in `meera.md` as it is written, because
//! resolving at query time fragments the store: two versions of a fact end up in separate files
//! and never meet, so the contradiction is never visible and never corrected.
//!
//! Blocking filters to at most five candidates with no model call, and matching is one bounded
//! Utility-role call over that set. The expensive step only ever sees a handful of options, which
//! is principle 4 on the write path.

use std::fmt::Write as _;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::index::{Candidate, Index, IndexError};
use crate::core::vocab::ModelRole;
use crate::ports::model::{Message, ModelError, ModelProvider, Request, SystemBlock};

/// The most candidates blocking will hand to the match call (§9.4).
pub const MAX_CANDIDATES: usize = 5;

/// Where a new entity's file goes. The directories are §9.3's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Person,
    Project,
    Preference,
}

impl Kind {
    #[must_use]
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Person => "people",
            Self::Project => "projects",
            Self::Preference => "preferences",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("could not read the index: {0}")]
    Index(#[from] IndexError),
    #[error("the match call failed: {0}")]
    Model(#[from] ModelError),
    #[error("the surface form was empty")]
    NoSurfaceForm,
}

/// What the match call decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// One candidate is the entity, by position in the list handed to the matcher.
    Existing(usize),
    /// None of them. A new entity.
    New,
    /// Two or more are equally plausible.
    Tie(Vec<usize>),
}

/// Where a claim belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An existing file. Write the claim into it.
    Existing { path: String },
    /// A new file, with its alias list seeded from the surface form that was used.
    New { path: String, aliases: Vec<String> },
    /// Two people with the same name, the known failure of §9.4. Create neither: hold the claim
    /// as `draft` and surface it, the same shape as conflict rule 4.
    Ambiguous { between: Vec<String> },
}

/// The expensive half of §9.4, behind a trait.
///
/// A trait rather than a concrete provider so the resolver can be tested without a model call in
/// the loop. A test whose setup is a model call measures two things and fails for the wrong
/// reasons.
#[async_trait]
pub trait Matcher: Send + Sync {
    /// Picks the entity a claim belongs to from a bounded candidate set.
    ///
    /// # Errors
    /// Fails if the underlying call fails. An unparseable answer is not an error: it is treated
    /// as no match, since inventing a merge is worse than writing a new file.
    async fn decide(
        &self,
        surface: &str,
        claim: &str,
        candidates: &[Candidate],
    ) -> Result<Decision, ResolveError>;
}

/// Resolves a claim's entity.
///
/// Blocking runs first and an empty candidate set short-circuits: a genuinely new entity costs no
/// model call at all, which is what makes import over years of history affordable (§11.5).
///
/// # Errors
/// Fails if the index cannot be read, the match call fails, or the surface form is empty.
pub async fn resolve(
    surface: &str,
    tags: &[String],
    claim: &str,
    kind: Kind,
    index: &Index,
    matcher: &dyn Matcher,
) -> Result<Resolution, ResolveError> {
    let trimmed = surface.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::NoSurfaceForm);
    }

    let candidates = index.candidates(trimmed, tags, MAX_CANDIDATES)?;
    if candidates.is_empty() {
        return Ok(fresh(trimmed, kind));
    }

    let resolution = match matcher.decide(trimmed, claim, &candidates).await? {
        // An out-of-range answer is a malformed answer. A new file is recoverable; merging into
        // the wrong entity is the failure §21.2 exists to measure.
        Decision::Existing(at) => candidates.get(at).map_or_else(
            || fresh(trimmed, kind),
            |c| Resolution::Existing {
                path: c.path.clone(),
            },
        ),
        Decision::New => fresh(trimmed, kind),
        Decision::Tie(between) => {
            let paths: Vec<String> = between
                .into_iter()
                .filter_map(|at| candidates.get(at).map(|c| c.path.clone()))
                .collect();
            // Fewer than two means it was not a tie after all.
            if paths.len() < 2 {
                fresh(trimmed, kind)
            } else {
                Resolution::Ambiguous { between: paths }
            }
        }
    };
    Ok(resolution)
}

/// A new entity, with its alias list seeded from the form that was used (§9.4 step 3).
fn fresh(surface: &str, kind: Kind) -> Resolution {
    Resolution::New {
        path: format!("{}/{}.md", kind.directory(), slug(surface)),
        aliases: vec![surface.to_owned()],
    }
}

/// A filename from a surface form. Lowercase, one hyphen between runs of anything else.
fn slug(surface: &str) -> String {
    let mut out = String::with_capacity(surface.len());
    let mut hyphen = false;
    for ch in surface.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            hyphen = false;
        } else if !out.is_empty() && !hyphen {
            out.push('-');
            hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "unnamed".to_owned()
    } else {
        out
    }
}

/// The match call, on the Utility role.
///
/// Utility rather than Primary because it shares no prefix with the conversation, so routing it
/// costs no cache hit (§22.2). Entity matching is also where the volume is once import runs.
pub struct ModelMatcher<'a> {
    provider: &'a dyn ModelProvider,
    cancel: CancellationToken,
}

impl<'a> ModelMatcher<'a> {
    #[must_use]
    pub fn new(provider: &'a dyn ModelProvider, cancel: CancellationToken) -> Self {
        Self { provider, cancel }
    }
}

const INSTRUCTIONS: &str = "\
You decide which known entity a statement is about.

Answer with one line and nothing else:
  MATCH <number>   the statement is about that numbered entity
  NEW              the statement is about none of them
  TIE <a> <b>      two or more are equally plausible and you cannot tell them apart

Prefer NEW over a guess. Two different people can share a name; if the statement gives you no way
to tell which one it is, answer TIE.";

#[async_trait]
impl Matcher for ModelMatcher<'_> {
    async fn decide(
        &self,
        surface: &str,
        claim: &str,
        candidates: &[Candidate],
    ) -> Result<Decision, ResolveError> {
        let mut prompt = String::new();
        let _ = writeln!(prompt, "Statement: {claim}");
        let _ = writeln!(prompt, "Referring to: {surface}\n");
        let _ = writeln!(prompt, "Known entities:");
        for (at, candidate) in candidates.iter().enumerate() {
            let _ = writeln!(prompt, "  {at}. {} ({})", candidate.name, candidate.path);
        }

        let request = Request {
            role: ModelRole::Utility,
            system: vec![SystemBlock::new(INSTRUCTIONS)],
            messages: vec![Message::user(prompt)],
            max_tokens: 32,
        };

        let mut stream = self
            .provider
            .complete(request, self.cancel.clone())
            .await
            .map_err(ResolveError::Model)?;

        let mut answer = String::new();
        use futures_util::StreamExt as _;
        while let Some(chunk) = stream.next().await {
            if let crate::ports::model::Chunk::Text(text) = chunk.map_err(ResolveError::Model)? {
                answer.push_str(&text);
            }
        }
        Ok(parse_decision(&answer, candidates.len()))
    }
}

/// Reads the matcher's one line.
///
/// Tolerant on purpose, and it fails towards `New`. A malformed answer that becomes a new file
/// costs a duplicate the user can merge; one that becomes a match writes a fact onto the wrong
/// person, which is the over-supersession §21.2 measures.
fn parse_decision(answer: &str, count: usize) -> Decision {
    let line = answer
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_uppercase();

    let numbers: Vec<usize> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .filter(|n| *n < count)
        .collect();

    if line.starts_with("TIE") {
        let mut unique = numbers;
        unique.sort_unstable();
        unique.dedup();
        if unique.len() >= 2 {
            return Decision::Tie(unique);
        }
        return Decision::New;
    }
    if line.starts_with("MATCH")
        && let Some(&first) = numbers.first()
    {
        return Decision::Existing(first);
    }
    Decision::New
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_filename_safe() {
        assert_eq!(slug("Meera Raghunathan"), "meera-raghunathan");
        assert_eq!(slug("  O'Brien & Co.  "), "o-brien-co");
        assert_eq!(slug("Loki"), "loki");
        assert_eq!(slug("???"), "unnamed");
    }

    #[test]
    fn a_new_entity_seeds_its_alias_from_the_surface_form() {
        let Resolution::New { path, aliases } = fresh("Meera Raghunathan", Kind::Person) else {
            panic!("expected a new entity");
        };
        assert_eq!(path, "people/meera-raghunathan.md");
        assert_eq!(aliases, ["Meera Raghunathan"]);
    }

    #[test]
    fn the_matcher_answers_are_read() {
        assert_eq!(parse_decision("MATCH 2", 5), Decision::Existing(2));
        assert_eq!(parse_decision("  match 0  \n", 5), Decision::Existing(0));
        assert_eq!(parse_decision("NEW", 5), Decision::New);
        assert_eq!(parse_decision("TIE 0 3", 5), Decision::Tie(vec![0, 3]));
    }

    /// A wrong merge is the expensive mistake, so anything unclear has to land on `New`.
    #[test]
    fn a_malformed_answer_falls_towards_a_new_entity() {
        for answer in [
            "",
            "I think it is probably Meera?",
            "MATCH",
            "MATCH 9",
            "TIE 1",
            "TIE",
            "{\"decision\": \"existing\"}",
        ] {
            assert_eq!(
                parse_decision(answer, 5),
                Decision::New,
                "answer was {answer:?}"
            );
        }
    }

    #[test]
    fn a_tie_naming_the_same_candidate_twice_is_not_a_tie() {
        assert_eq!(parse_decision("TIE 2 2", 5), Decision::New);
    }

    #[test]
    fn kinds_map_onto_the_section_9_3_layout() {
        assert_eq!(Kind::Person.directory(), "people");
        assert_eq!(Kind::Project.directory(), "projects");
        assert_eq!(Kind::Preference.directory(), "preferences");
    }
}
