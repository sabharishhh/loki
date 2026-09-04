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

use super::concept;
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

/// **An exact name under another kind no longer forces a merge** (D-069).
///
/// It used to: a matcher answering `New` was overridden when the same slug already existed
/// elsewhere, on the reasoning that identity is the entity and not the directory. That was right
/// when the matcher was shown two bare names, because its "no" was about the claim rather than
/// about identity. It is shown both kinds and both sets of facts now, so its "no" is an informed
/// answer and overriding it merged the company Apple with an allergy to apples.
///
/// Neither direction decides structurally any more. Two cards are written and reported as
/// claiming one name, and one tap folds them together if they should be. That is the safe way
/// round: a split leaves two visible rows, and a merge silently hides a true fact (§21.2).
///
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
    /// `kind` is what the statement is about, so a matcher can weigh it against each candidate's.
    /// Without it the rule that a differing kind is evidence has nothing to compare against.
    ///
    /// # Errors
    /// Fails if the underlying call fails. An unparseable answer is not an error: it is treated
    /// as no match, since inventing a merge is worse than writing a new file.
    async fn decide(
        &self,
        surface: &str,
        claim: &str,
        kind: Kind,
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
    relation: Option<&Edge<'_>>,
    index: &Index,
    matcher: &dyn Matcher,
) -> Result<Resolution, ResolveError> {
    let trimmed = surface.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::NoSurfaceForm);
    }

    // **The two seeded cards before anything else.** `the user` is the commonest subject in the
    // store and it is not a name: it is how the extractor refers to the owner on every single
    // claim. Sent through blocking and a matcher it was a coin flip, and B-53 is what that cost.
    // Nine claims from one session asked the same question nine times, got MATCH, an empty reply
    // and MATCH again, and the owner's facts landed on two cards. Answered here it is
    // deterministic, free, and cannot produce a second owner.
    if let Some(path) = concept::reserved(trimmed) {
        return Ok(Resolution::Existing {
            path: settled(path.to_owned(), index),
        });
    }

    // The graph before the strings. An edge is something somebody said outright, so it beats every
    // guess blocking could make, and it is what turns "my sister" and "Lakshmi" into one card.
    if let Some(path) = through_the_graph(trimmed, relation, index)? {
        return Ok(Resolution::Existing {
            path: settled(path, index),
        });
    }

    let mut candidates = index.candidates(trimmed, tags, MAX_CANDIDATES)?;
    // A descriptor is never the thing it describes. "The user's sister" reads as a near-name match
    // for "the user", and merging there writes a fact about one person onto another.
    if let Some((whose, _)) = descriptor(trimmed) {
        candidates.retain(|c| !c.name.eq_ignore_ascii_case(whose));
    }
    // Kind as evidence, never as a filter (§9.4). A candidate of the same kind is a likelier match
    // and goes first; one of a different kind still gets shown, because the same entity extracted
    // once as a person and once as a project must be able to meet. §12's fetched pages need this
    // more than conversation does: a page about Apple must not merge with an allergy to apples,
    // and it is the matcher, seeing both kinds and both sets of facts, that can tell.
    candidates.sort_by_key(|c| u8::from(c.kind != kind.directory()));
    if candidates.is_empty() {
        return Ok(fresh(trimmed, kind));
    }

    let resolution = match matcher.decide(trimmed, claim, kind, &candidates).await? {
        // An out-of-range answer is a malformed answer. A new file is recoverable; merging into
        // the wrong entity is the failure §21.2 exists to measure.
        Decision::Existing(at) => candidates.get(at).map_or_else(
            || fresh(trimmed, kind),
            |c| Resolution::Existing {
                path: settled(c.path.clone(), index),
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

/// An edge a sentence asserted: this entity is the `label` of `of`.
///
/// Borrowed rather than owned because the caller already has both strings, and resolution is on
/// the write path for every claim in an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge<'a> {
    pub label: &'a str,
    pub of: &'a str,
}

/// Follows a relation to the card it already points at (§9.4, S-21).
///
/// Two ways in, and what they are allowed to do differs.
///
/// A **descriptor** like "my father" or "Vaidyanathan's brother" names nobody, so following the
/// edge is the only way to know who it means. It resolves to whatever the edge points at.
///
/// An **explicit edge** on a named entity may only absorb a *placeholder*. "Karthik is my brother"
/// must not merge into the brother already on file, because two brothers is the ordinary case and
/// merging them loses a person; "Lakshmi is my sister" absorbing the card that says only "the
/// user's sister" loses nothing, because that card was standing in for a name all along.
fn through_the_graph(
    surface: &str,
    relation: Option<&Edge<'_>>,
    index: &Index,
) -> Result<Option<String>, ResolveError> {
    let described = descriptor(surface);
    let Some((whose, label)) = described.or_else(|| relation.map(|e| (e.of, e.label))) else {
        return Ok(None);
    };
    let Some(from) = index.path_answering_to(whose)? else {
        return Ok(None);
    };
    let Some(to) = index.related(&from, label)? else {
        return Ok(None);
    };
    if described.is_none() && !index.describes(&to)? {
        return Ok(None);
    }
    Ok(Some(to))
}

/// Reads a surface form that points at somebody through somebody else.
///
/// "my father" and "the user's sister" both mean the owner's; "Vaidyanathan's brother" means his.
/// Only a single-word label counts: "the user's old school friend" is a description of a person,
/// not a relation anything can follow, and treating it as one would invent an edge nobody said.
#[must_use]
pub fn descriptor(surface: &str) -> Option<(&str, &str)> {
    let trimmed = surface.trim();
    let lower = trimmed.to_lowercase();

    // "my X" and "our X" are the owner's, whoever the owner turns out to be.
    for opener in ["my ", "our "] {
        if let Some(rest) = lower.strip_prefix(opener) {
            let label = &trimmed[trimmed.len() - rest.len()..];
            return one_word(label).map(|label| (OWNER_FORM, label));
        }
    }

    let at = trimmed.find("'s ").or_else(|| trimmed.find("\u{2019}s "))?;
    let whose = trimmed[..at].trim();
    // The separator is three characters either way: the apostrophe, the s and the space.
    let label = trimmed[at..].chars().skip(3).count();
    let label = &trimmed[trimmed.len() - label..];
    if whose.is_empty() {
        return None;
    }
    one_word(label).map(|label| (whose, label))
}

/// The surface form the owner's card answers to. Seeded before the first turn (§9.4).
const OWNER_FORM: &str = "the user";

fn one_word(label: &str) -> Option<&str> {
    let label = label.trim();
    (!label.is_empty() && !label.contains(char::is_whitespace)).then_some(label)
}

/// A new entity, with its alias list seeded from the form that was used (§9.4 step 3).
/// Follows `merged_into` to the card a tombstone became (§9.4).
///
/// **A merged card still answers to its old names.** Blocking excludes tombstones, but a relation
/// can still point at one and a reserved form can still name one, and a write that lands on a
/// tombstone is invisible in both directions: the §10.4 gate keeps a deprecated concept out of
/// every prompt, and §17.3's duplicate list skips it, so the fact is neither usable nor
/// repairable. Four of Sabharish's facts went that way. B-54.
///
/// Bounded, because a cycle in the pointers must not become a hang. On anything unreadable the
/// original path is returned unchanged, which is the behaviour that existed before this.
fn settled(path: String, index: &Index) -> String {
    let mut at = path;
    for _ in 0..MERGE_DEPTH {
        match index.merged_into(&at) {
            Ok(Some(next)) if next != at => at = next,
            _ => break,
        }
    }
    at
}

/// How far a chain of merges is followed. A store with more than this is malformed.
const MERGE_DEPTH: usize = 8;

fn fresh(surface: &str, kind: Kind) -> Resolution {
    Resolution::New {
        path: format!("{}/{}.md", kind.directory(), slug(surface)),
        aliases: vec![surface.to_owned()],
    }
}

/// Whether a surface form describes an entity rather than naming it (§9.4, S-21).
///
/// "The user's sister" and "the client from Tuesday" are placeholders standing in for names nobody
/// has given. Deterministic string work rather than a model call: this decides whether a later real
/// name may absorb the card, and a rule a person can read is worth more here than a judgement.
///
/// One-sided, like every other guard in this module. A missed descriptor costs a card that keeps
/// its placeholder name; a false positive would let an ordinary name be overwritten.
#[must_use]
pub fn looks_described(surface: &str) -> bool {
    let lower = surface.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    // A possessive is the giveaway: something is being pointed at through something else.
    if lower.contains("'s ") || lower.contains("\u{2019}s ") {
        return true;
    }
    const OPENERS: [&str; 8] = ["the ", "my ", "a ", "an ", "his ", "her ", "their ", "our "];
    OPENERS.iter().any(|opener| lower.starts_with(opener))
}

/// Whether a surface form is a name the entity is known by, as opposed to a way of reaching it.
///
/// The owner's card answers to `me`, `myself` and `the owner`. Those are machinery: they exist so
/// blocking can route a claim, and telling the user or the model that Sabharish is "also called
/// me, myself, the owner" is noise in one place and a paid token in the other.
#[must_use]
pub fn is_a_real_name(form: &str) -> bool {
    const PRONOUNS: [&str; 10] = [
        "me", "myself", "i", "you", "we", "us", "him", "her", "them", "it",
    ];
    let lower = form.trim().to_lowercase();
    !lower.is_empty() && !looks_described(&lower) && !PRONOUNS.contains(&lower.as_str())
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

Each entity is listed with its kind and with a few things already believed about it. Read those
before answering: the name alone cannot tell two people apart, and the facts usually can.

Answer with one line and nothing else:
  MATCH <number>   the statement is about that numbered entity
  NEW              the statement is about none of them
  TIE <a> <b>      two or more are equally plausible and you cannot tell them apart

Prefer NEW over a guess. A new file is a duplicate somebody can merge in one tap; a wrong match
writes a fact onto the wrong person, and nothing ever surfaces it.

- If a known entity's facts contradict the statement, it is a different entity. Someone on the
  design team is not the person who runs infra, whatever they are both called.
- A different kind is evidence, not proof. The company Apple and an allergy to apples are
  different things; a person also filed as a project is one thing filed twice. When the kinds
  differ and nothing in the facts links them, answer NEW.
- Two entities that both fit and cannot be told apart are a TIE. That is a real answer, and it is
  the right one for two people who share a name.";

#[async_trait]
impl Matcher for ModelMatcher<'_> {
    async fn decide(
        &self,
        surface: &str,
        claim: &str,
        kind: Kind,
        candidates: &[Candidate],
    ) -> Result<Decision, ResolveError> {
        let mut prompt = String::new();
        let _ = writeln!(prompt, "Statement: {claim}");
        let _ = writeln!(prompt, "Referring to: {surface} [{}]\n", kind.directory());
        let _ = writeln!(prompt, "Known entities:");
        for (at, candidate) in candidates.iter().enumerate() {
            let _ = writeln!(
                prompt,
                "  {at}. {} [{}] ({})",
                candidate.name, candidate.kind, candidate.path
            );
            for fact in &candidate.facts {
                let _ = writeln!(prompt, "       - {fact}");
            }
            if candidate.facts.is_empty() {
                let _ = writeln!(prompt, "       - nothing known yet");
            }
        }

        // **A non-answer is asked again, once.** Measured on Sabharish's store: the same question
        // came back empty after 1422 ms, twice in one pass, and an unreadable answer used to fall
        // through to `New`. "Prefer NEW over a guess" is a rule about answers, and a model that
        // returned nothing has not guessed and has not answered. B-53.
        for attempt in 0..=RETRIES {
            let answer = self.ask(&prompt).await?;
            if let Some(decision) = parse_decision(&answer, candidates.len()) {
                return Ok(decision);
            }
            if attempt == RETRIES {
                // Still nothing. `New` is the documented direction to fail in: a duplicate is one
                // tap on §17.3's list, and a wrong match writes a fact onto the wrong person where
                // nothing surfaces it. It is a decision now rather than a fallthrough.
                return Ok(Decision::New);
            }
        }
        Ok(Decision::New)
    }
}

/// How many times an unreadable answer is asked again.
const RETRIES: usize = 1;

impl ModelMatcher<'_> {
    async fn ask(&self, prompt: &str) -> Result<String, ResolveError> {
        use futures_util::StreamExt as _;

        let request = Request {
            role: ModelRole::Utility,
            system: vec![SystemBlock::new(INSTRUCTIONS)],
            messages: vec![Message::user(prompt.to_owned())],
            max_tokens: 32,
        };
        let mut stream = self
            .provider
            .complete(request, self.cancel.clone())
            .await
            .map_err(ResolveError::Model)?;

        let mut answer = String::new();
        while let Some(chunk) = stream.next().await {
            if let crate::ports::model::Chunk::Text(text) = chunk.map_err(ResolveError::Model)? {
                answer.push_str(&text);
            }
        }
        Ok(answer)
    }
}

/// Reads the matcher's one line, or `None` when there is no line to read.
///
/// Tolerant on purpose, and where it does decide it fails towards `New`: a malformed answer that
/// becomes a new file costs a duplicate the user can merge, and one that becomes a match writes a
/// fact onto the wrong person, which is the over-supersession §21.2 measures.
///
/// **An empty or wordless reply is not an answer.** It used to be read as `New`, so a provider
/// hiccup created a person. The caller asks again instead.
fn parse_decision(answer: &str, count: usize) -> Option<Decision> {
    let line = answer
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
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
            return Some(Decision::Tie(unique));
        }
        return Some(Decision::New);
    }
    if line.starts_with("MATCH")
        && let Some(&first) = numbers.first()
    {
        return Some(Decision::Existing(first));
    }
    // A word that is neither MATCH nor TIE, NEW included. The model answered; it just did not pick
    // anybody.
    Some(Decision::New)
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
        assert_eq!(parse_decision("MATCH 2", 5), Some(Decision::Existing(2)));
        assert_eq!(
            parse_decision("  match 0  \n", 5),
            Some(Decision::Existing(0))
        );
        assert_eq!(parse_decision("NEW", 5), Some(Decision::New));
        assert_eq!(
            parse_decision("TIE 0 3", 5),
            Some(Decision::Tie(vec![0, 3]))
        );
    }

    /// A wrong merge is the expensive mistake, so anything unclear has to land on `New`.
    #[test]
    fn a_malformed_answer_falls_towards_a_new_entity() {
        for answer in [
            "I think it is probably Meera?",
            "MATCH",
            "MATCH 9",
            "TIE 1",
            "TIE",
            "{\"decision\": \"existing\"}",
        ] {
            assert_eq!(
                parse_decision(answer, 5),
                Some(Decision::New),
                "answer was {answer:?}"
            );
        }
    }

    /// B-53. A model that returned nothing has not answered, and `New` creates a person.
    #[test]
    fn a_reply_with_no_words_in_it_is_not_an_answer() {
        for answer in ["", "   ", "\n\n", "\t \r\n  "] {
            assert_eq!(
                parse_decision(answer, 5),
                None,
                "a non-answer must be asked again, not read as NEW: {answer:?}"
            );
        }
        // A reply with any word in it is an answer, even an unusable one.
        assert_eq!(parse_decision("hmm", 5), Some(Decision::New));
    }

    #[test]
    fn a_tie_naming_the_same_candidate_twice_is_not_a_tie() {
        assert_eq!(parse_decision("TIE 2 2", 5), Some(Decision::New));
    }

    /// B-53. The two seeded cards are answered from their fixed paths and never by a model.
    #[test]
    fn the_reserved_forms_always_mean_the_seeded_cards() {
        for form in ["the user", "The User", "  me  ", "myself", "the owner"] {
            assert_eq!(concept::reserved(form), Some(crate::memory::bundle::OWNER));
        }
        for form in ["you", "You", "the assistant"] {
            assert_eq!(
                concept::reserved(form),
                Some(crate::memory::bundle::ASSISTANT)
            );
        }
        // A descriptor of the owner is not the owner, and a real name is nobody reserved.
        for form in [
            "the user's father",
            "the users",
            "Sabharish",
            "Loki",
            "the user's sister",
            "",
        ] {
            assert_eq!(concept::reserved(form), None, "{form:?}");
        }
    }

    #[test]
    fn kinds_map_onto_the_section_9_3_layout() {
        assert_eq!(Kind::Person.directory(), "people");
        assert_eq!(Kind::Project.directory(), "projects");
        assert_eq!(Kind::Preference.directory(), "preferences");
    }
}
