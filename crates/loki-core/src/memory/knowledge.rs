//! What Loki knows, as a person would read it (§17.3).
//!
//! **Rows are what Loki knows, not a stream of state transitions.** The first build of the
//! timeline rendered `log.md`, which is a chronological record of things that happened to the
//! store. Sabharish's words for it were broken atomic pieces listed linearly, and he was right:
//! a log answers "what changed" and the trust surface has to answer "what do you think you know".
//!
//! So this reads the concept files instead. Every field of a row comes from a line you can open,
//! which is §17.3's requirement, and the log keeps its own job of recording the sequence.
//!
//! **A correction is one row.** §9.5's four timestamps are what make that possible: the superseded
//! claim is still in the file with its world-time window closed, so the row can carry both ranges
//! and say how long the wrong one was believed. Two rows struck through against each other is the
//! shape of the data, not the shape of the answer.
//!
//! **No internal state name reaches this module's output.** Not `draft`, not `candidate`, not
//! `stable`. A concept that is not prompt-eligible is one Loki is not using yet, and that is what
//! the field says.

use jiff::civil::Date;
use serde::Serialize;

use super::bundle::{Bundle, BundleError};
use super::claim::{Claim, Origin};
use super::concept::{RawConcept, Status};
use crate::core::temporal;

/// Everything Loki knows, grouped by the thing it is about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Knowledge {
    pub entities: Vec<Entity>,
}

/// One person, project or preference, and what is known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entity {
    /// The file this came from. Every row is a line in it, and the screen can open it.
    pub path: String,
    pub name: String,
    /// `person`, `project` or `preference`, from the directory.
    pub kind: String,
    /// Whether anything here can reach a prompt.
    ///
    /// A boolean rather than a status name, because the user is owed the consequence and not the
    /// vocabulary: what matters is whether Loki is using this, not what the file calls itself.
    pub in_use: bool,
    /// Confirmed by a person, so nothing decays it by heuristic (§9.9).
    pub confirmed: bool,
    pub facts: Vec<Fact>,
    /// Conflicts waiting on the user (§9.7 rule 4). One tap each.
    pub questions: Vec<Question>,
}

/// One thing Loki knows, with its own history folded in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    /// Addresses this claim inside its concept, for edit, delete and correction.
    pub ordinal: u32,
    /// Which property of the entity this sets. Shown as a quiet label, not as a heading.
    pub attribute: String,
    pub text: String,
    /// `Since 15 July, about seven weeks.` Absent when the source never dated it (§9.5).
    pub since: Option<String>,
    /// What this replaced, on the same row (§17.3).
    pub was: Option<Correction>,
    /// True when the claim came from a page or an account rather than from the user (§9.12).
    ///
    /// Surfaced because §17.6 needs connector-written memory listable and removable, and because
    /// a fact Loki read somewhere is a different kind of thing from one you told it.
    pub from_elsewhere: bool,
}

/// The half of a correction that is no longer true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Correction {
    pub text: String,
    /// `from 1 March to 15 July`, or `until 15 July` when the start was never given.
    pub held: String,
    /// `about six weeks`, when Loki went on believing it after it had stopped being true.
    pub wrong_for: Option<String>,
}

/// Two claims that cannot both be true, and nothing has picked between them (§9.7 rule 4).
///
/// Rendered as the question it is rather than as a state. Guessing is how a memory system poisons
/// itself, so the store holds both and asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    pub attribute: String,
    pub options: Vec<Answer>,
}

/// One side of a question, addressed the way [`Fact`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub ordinal: u32,
    pub text: String,
    pub since: Option<String>,
}

/// Reads the whole store into the shape §17.3 renders.
///
/// Newest first by what was learned, because a person opening this wants to check recent work.
///
/// # Errors
/// Fails if the bundle cannot be read. A single unparseable file is skipped rather than failing
/// the screen: one bad file should not hide the rest of the store.
pub async fn read(bundle: &Bundle, today: Date) -> Result<Knowledge, BundleError> {
    let paths = {
        let reader = bundle.reader().await;
        reader.concepts()?
    };

    let mut entities = Vec::with_capacity(paths.len());
    for path in paths {
        let concept = {
            let reader = bundle.reader().await;
            match reader.load_concept(&path) {
                Ok(concept) => concept,
                Err(_) => continue,
            }
        };
        entities.push(entity(&path, &concept, today));
    }

    entities.sort_by(|a, b| {
        b.newest()
            .cmp(&a.newest())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Knowledge { entities })
}

impl Entity {
    /// The most recent thing learned here, for ordering. Absent facts sort last.
    fn newest(&self) -> Option<u32> {
        self.facts.iter().map(|f| f.ordinal).max()
    }
}

fn entity(path: &str, concept: &RawConcept, today: Date) -> Entity {
    let numbered: Vec<(u32, &Claim)> = concept
        .claims()
        .enumerate()
        .filter_map(|(at, claim)| u32::try_from(at).ok().map(|n| (n, claim)))
        .collect();

    let believed: Vec<(u32, &Claim)> = numbered
        .iter()
        .filter(|(_, c)| c.validity.is_believed())
        .copied()
        .collect();

    let mut facts = Vec::new();
    let mut questions = Vec::new();

    for (at, claim) in &believed {
        // Two believed claims about one attribute is rule 4's surface, and the only path that
        // leaves both standing. Everything else resolves at write time.
        let rivals: Vec<&(u32, &Claim)> = believed
            .iter()
            .filter(|(_, other)| claim.same_attribute_as(other))
            .collect();
        if rivals.len() > 1 {
            if rivals[0].0 == *at {
                questions.push(question(&rivals, today));
            }
            continue;
        }
        facts.push(fact(*at, claim, &numbered, today));
    }

    facts.sort_by(|a, b| a.attribute.cmp(&b.attribute));

    Entity {
        path: path.to_owned(),
        name: concept.front.name.clone(),
        kind: kind_of(path),
        in_use: concept.front.status == Status::Stable && !concept.is_stale_on(today),
        confirmed: concept.front.is_human_verified(),
        facts,
        questions,
    }
}

fn fact(ordinal: u32, claim: &Claim, all: &[(u32, &Claim)], today: Date) -> Fact {
    Fact {
        ordinal,
        attribute: claim.attribute.clone(),
        text: claim.text.clone(),
        since: claim
            .validity
            .valid_from
            .map(|from| temporal::since(from, today)),
        was: superseded_by(claim, all, today),
        from_elsewhere: !claim.origin.durable_eligible(),
    }
}

/// The most recent claim this one replaced, folded onto the same row.
///
/// Matched on `replaced_by`, which the invalidating write records, so the pairing comes from the
/// file rather than from guessing which retired claim went with which live one.
fn superseded_by(claim: &Claim, all: &[(u32, &Claim)], today: Date) -> Option<Correction> {
    let old = all
        .iter()
        .filter(|(_, other)| !other.validity.is_believed())
        .filter(|(_, other)| other.replaced_by.as_deref() == Some(claim.text.as_str()))
        .max_by_key(|(_, other)| other.validity.valid_to)?
        .1;

    Some(Correction {
        text: old.text.clone(),
        held: held(old, today),
        wrong_for: old
            .validity
            .wrong_for_days()
            .filter(|days| *days > 0)
            .map(temporal::span),
    })
}

fn held(claim: &Claim, today: Date) -> String {
    let until = claim
        .validity
        .valid_to
        .map(|to| temporal::day_month(to, today));
    match (claim.validity.valid_from, until) {
        (Some(from), Some(to)) => format!("from {} to {}", temporal::day_month(from, today), to),
        (Some(from), None) => format!("from {}", temporal::day_month(from, today)),
        (None, Some(to)) => format!("until {to}"),
        (None, None) => "before that".to_owned(),
    }
}

fn question(rivals: &[&(u32, &Claim)], today: Date) -> Question {
    Question {
        attribute: rivals[0].1.attribute.clone(),
        options: rivals
            .iter()
            .map(|(at, claim)| Answer {
                ordinal: *at,
                text: claim.text.clone(),
                since: claim
                    .validity
                    .valid_from
                    .map(|from| temporal::since(from, today)),
            })
            .collect(),
    }
}

/// The singular of the directory a concept lives in.
///
/// Derived from the path rather than stored, because §9.4 makes the directory cosmetic: identity
/// is the entity, and a concept can legitimately sit under a kind it was first extracted as.
fn kind_of(path: &str) -> String {
    match path.split('/').next().unwrap_or_default() {
        "people" => "person",
        "projects" => "project",
        "preferences" => "preference",
        other => other,
    }
    .to_owned()
}

/// Whether a claim came from somewhere other than the user, in the words §17.6 will list it by.
#[must_use]
pub const fn source_note(origin: Origin) -> &'static str {
    match origin {
        Origin::Stated => "you told me",
        Origin::Inferred => "I worked this out",
        Origin::Web => "I read this on a page",
        Origin::Connector => "from a connected account",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::claim::Claim;
    use crate::memory::concept::Frontmatter;
    use jiff::civil::date;

    fn today() -> Date {
        date(2026, 9, 2)
    }

    fn stable(name: &str) -> RawConcept {
        let mut front = Frontmatter::new(name, date(2026, 1, 1));
        front.status = Status::Stable;
        RawConcept::new(front)
    }

    #[test]
    fn an_ordinary_fact_is_one_row_with_no_history() {
        let mut concept = stable("Sabharish");
        concept.add(
            "education",
            Claim::stated("Sabharish is a computer science graduate", date(2026, 1, 1))
                .about("education"),
        );

        let out = entity("people/sabharish.md", &concept, today());
        assert_eq!(out.kind, "person");
        assert!(out.in_use);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].was, None);
        assert_eq!(
            out.facts[0].since, None,
            "the source never dated it, so there is no distance to state"
        );
        assert!(out.questions.is_empty());
    }

    /// §17.3: a correction is one row carrying both ranges, never two struck through.
    #[test]
    fn a_correction_is_one_row_with_both_ranges() {
        let mut concept = stable("Sabharish");
        let mut old = Claim::stated("Works on the platform team", date(2026, 3, 12))
            .about("role")
            .dated(date(2026, 3, 12));
        old.invalidate(
            date(2026, 8, 29),
            date(2026, 7, 15),
            "Works on the infra team",
        );
        concept.add("role", old);
        concept.add(
            "role",
            Claim::stated("Works on the infra team", date(2026, 8, 29))
                .about("role")
                .dated(date(2026, 7, 15)),
        );

        let out = entity("people/sabharish.md", &concept, today());
        assert_eq!(out.facts.len(), 1, "one row, not two: {:?}", out.facts);

        let fact = &out.facts[0];
        assert_eq!(fact.text, "Works on the infra team");
        assert_eq!(
            fact.since.as_deref(),
            Some("Since 15 July, about seven weeks.")
        );

        let was = fact.was.as_ref().expect("the row carries what it replaced");
        assert_eq!(was.text, "Works on the platform team");
        assert_eq!(was.held, "from 12 March to 15 July");
        assert_eq!(
            was.wrong_for.as_deref(),
            Some("about six weeks"),
            "the sentence §9.5 exists to make writable"
        );
    }

    /// Rule 4's surface, rendered as the question it is rather than as a state name.
    #[test]
    fn a_conflict_becomes_a_question_with_two_answers() {
        let mut concept = stable("Sabharish");
        concept.front.status = Status::Draft;
        concept.add(
            "city",
            Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
        );
        concept.add(
            "city",
            Claim::stated("Sabharish lives in Bangalore", date(2026, 1, 1)).about("city"),
        );

        let out = entity("people/sabharish.md", &concept, today());
        assert!(out.facts.is_empty(), "neither is in use: {:?}", out.facts);
        assert_eq!(out.questions.len(), 1);
        assert_eq!(out.questions[0].attribute, "city");
        assert_eq!(out.questions[0].options.len(), 2);
        assert_eq!(out.questions[0].options[0].ordinal, 0);
        assert_eq!(out.questions[0].options[1].ordinal, 1);
        assert!(!out.in_use, "a concept with an open question is not in use");
    }

    /// Unrelated facts are not rivals, however many there are.
    #[test]
    fn facts_about_different_attributes_are_not_a_question() {
        let mut concept = stable("Sabharish");
        concept.add(
            "name",
            Claim::stated("The user's name is Sabharish", date(2026, 1, 1)).about("name"),
        );
        concept.add(
            "city",
            Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
        );

        let out = entity("people/sabharish.md", &concept, today());
        assert_eq!(out.facts.len(), 2);
        assert!(out.questions.is_empty());
    }

    #[test]
    fn a_fact_read_off_a_page_says_so() {
        let mut concept = stable("Acme");
        concept.add(
            "funding",
            Claim::new("Acme raised a Series B", Origin::Web, date(2026, 1, 1)).about("funding"),
        );

        let out = entity("projects/acme.md", &concept, today());
        assert!(out.facts[0].from_elsewhere);
        assert_eq!(source_note(Origin::Web), "I read this on a page");
    }

    /// A claim with no attribute cannot conflict, so two of them are two facts (§9.7).
    #[test]
    fn claims_with_no_attribute_never_become_a_question() {
        let mut concept = stable("Sabharish");
        concept.add("Notes", Claim::stated("something", date(2026, 1, 1)));
        concept.add("Notes", Claim::stated("something else", date(2026, 1, 1)));

        let out = entity("people/sabharish.md", &concept, today());
        assert_eq!(out.facts.len(), 2);
        assert!(out.questions.is_empty());
    }
}
