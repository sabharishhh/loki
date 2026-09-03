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

use std::collections::HashMap;

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
    /// Cards that answer to one name, so the screen can offer to fold them together (§9.4).
    ///
    /// Derived from the store on every read rather than reported once by the pass that caused it.
    /// A split can arrive from a hand edit or an import as easily as from consolidation, and a
    /// one-shot report would only ever catch the third.
    pub duplicates: Vec<Duplicate>,
}

/// Two or more cards claiming the same surface form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Duplicate {
    /// What they all answer to, as the user would recognise it.
    pub form: String,
    /// Paths, most-known-about first, so the screen's default target is the fuller card.
    pub paths: Vec<String>,
    pub names: Vec<String>,
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
    /// Other names this entity answers to (§9.4).
    ///
    /// Shown, because it is knowledge. It used to live only in frontmatter, so "people call my dad
    /// Ashok" was learned and stored and the user had no way to see that it had been: three
    /// sessions of telling Loki something that already worked, because nothing said so (S-26).
    pub also_known_as: Vec<String>,
    /// Live edges out of this entity, in the same spirit.
    pub relations: Vec<Related>,
}

/// One current edge, as the trust surface shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Related {
    pub label: String,
    /// The target's display name, so a row reads as a sentence rather than as a path.
    pub name: String,
    pub path: String,
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
    /// Other things said about this property that Loki is not using (§9.7 rule 4).
    ///
    /// Shadowed, not retired: kept in the file, never in a prompt, and offered back here. Nothing
    /// blocks on them, because an approval queue nobody works through is worse than a wrong guess
    /// the user can see and flip.
    pub also_said: Vec<Alternative>,
}

/// Something said about a property that a later statement overrode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alternative {
    pub ordinal: u32,
    pub text: String,
    pub since: Option<String>,
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

    let mut cards: Vec<(String, RawConcept)> = Vec::with_capacity(paths.len());
    for path in paths {
        let reader = bundle.reader().await;
        if let Ok(concept) = reader.load_concept(&path) {
            cards.push((path, concept));
        }
    }
    // Every card's display name, so a relation row can read "sister  Lakshmi" rather than showing
    // a path at somebody who has never opened the store.
    let names: HashMap<String, String> = cards
        .iter()
        .map(|(path, concept)| (path.clone(), concept.front.name.clone()))
        .collect();

    let mut entities = Vec::with_capacity(cards.len());
    for (path, concept) in &cards {
        // A card with no claims at all is not knowledge. The owner and assistant cards are seeded
        // before the first turn (§9.4), and until something is learned about them the honest
        // answer to "what do you know" is still nothing.
        //
        // No claims, not no *believed* claims: an entity whose facts were all retired stays on the
        // screen, because §17.3's whole point is being able to see what Loki used to think.
        if concept.claims().next().is_none() {
            continue;
        }
        entities.push(entity(path, concept, today, &names));
    }

    entities.sort_by(|a, b| {
        b.newest()
            .cmp(&a.newest())
            .then_with(|| a.name.cmp(&b.name))
    });
    let duplicates = duplicates(&cards);
    Ok(Knowledge {
        entities,
        duplicates,
    })
}

/// Cards that answer to one name (§9.4).
///
/// Everything else in §9.4 stops a split happening at write time. This is what makes one that got
/// through visible, which has to come first: a split nobody can see is worse than one nobody can
/// fix, because the second at least gets reported.
///
/// A tombstone is skipped, since its forms belong to whatever it merged into.
fn duplicates(cards: &[(String, RawConcept)]) -> Vec<Duplicate> {
    use std::collections::BTreeMap;

    let mut by_form: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (at, (_, concept)) in cards.iter().enumerate() {
        if concept.front.merged_into.is_some() {
            continue;
        }
        let mut forms: Vec<String> = std::iter::once(&concept.front.name)
            .chain(concept.front.aliases.iter())
            .map(|form| form.trim().to_lowercase())
            .filter(|form| !form.is_empty())
            .collect();
        forms.sort_unstable();
        forms.dedup();
        for form in forms {
            by_form.entry(form).or_default().push(at);
        }
    }

    by_form
        .into_iter()
        .filter(|(_, at)| at.len() > 1)
        .map(|(form, at)| {
            let mut at = at;
            // The fuller card first, so the screen's default is to fold the thinner one into it.
            at.sort_by_key(|i| std::cmp::Reverse(cards[*i].1.claims().count()));
            Duplicate {
                form,
                paths: at.iter().map(|i| cards[*i].0.clone()).collect(),
                names: at.iter().map(|i| cards[*i].1.front.name.clone()).collect(),
            }
        })
        .collect()
}

impl Entity {
    /// The most recent thing learned here, for ordering. Absent facts sort last.
    fn newest(&self) -> Option<u32> {
        self.facts.iter().map(|f| f.ordinal).max()
    }
}

fn entity(
    path: &str,
    concept: &RawConcept,
    today: Date,
    names: &HashMap<String, String>,
) -> Entity {
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

    for (at, claim) in &believed {
        // A shadowed claim is not a row of its own. It hangs off the one that overrode it, which
        // is what turns rule 4 from a question into something worth checking.
        if super::reconcile::is_shadowed(concept, *at) {
            continue;
        }
        let mut row = fact(*at, claim, &numbered, today);
        row.also_said = believed
            .iter()
            .filter(|(other_at, other)| {
                *other_at != *at
                    && other.same_attribute_as(claim)
                    && super::reconcile::is_shadowed(concept, *other_at)
            })
            .map(|(other_at, other)| Alternative {
                ordinal: *other_at,
                text: other.text.clone(),
                since: other
                    .validity
                    .valid_from
                    .map(|from| temporal::since(from, today)),
            })
            .collect();
        facts.push(row);
    }

    facts.sort_by(|a, b| a.attribute.cmp(&b.attribute));

    Entity {
        path: path.to_owned(),
        name: concept.front.name.clone(),
        kind: kind_of(path),
        in_use: concept.front.status == Status::Stable && !concept.is_stale_on(today),
        confirmed: concept.front.is_human_verified(),
        facts,
        // The name itself is not an alias of the entity: it is already the heading of the card.
        also_known_as: concept
            .front
            .aliases
            .iter()
            .filter(|form| {
                !form.eq_ignore_ascii_case(&concept.front.name)
                    && super::resolve::is_a_real_name(form)
            })
            .cloned()
            .collect(),
        relations: concept
            .front
            .relations
            .iter()
            .filter(|edge| edge.is_current())
            .map(|edge| Related {
                label: edge.label.clone(),
                name: names
                    .get(&edge.to)
                    .cloned()
                    .unwrap_or_else(|| edge.to.clone()),
                path: edge.to.clone(),
            })
            .collect(),
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
        also_said: Vec::new(),
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

        let out = entity("people/sabharish.md", &concept, today(), &HashMap::new());
        assert_eq!(out.kind, "person");
        assert!(out.in_use);
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.facts[0].was, None);
        assert_eq!(
            out.facts[0].since, None,
            "the source never dated it, so there is no distance to state"
        );
        assert!(out.facts[0].also_said.is_empty());
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

        let out = entity("people/sabharish.md", &concept, today(), &HashMap::new());
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

    /// Rule 4 under option A: the later statement is used, and the earlier hangs off it as
    /// something worth checking. Nothing blocks, and nothing is retired.
    #[test]
    fn a_conflict_leaves_one_fact_and_an_alternative() {
        let mut concept = stable("Sabharish");
        concept.add(
            "city",
            Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city"),
        );
        concept.add(
            "city",
            Claim::stated("Sabharish lives in Bangalore", date(2026, 1, 1)).about("city"),
        );

        let out = entity("people/sabharish.md", &concept, today(), &HashMap::new());
        assert_eq!(out.facts.len(), 1, "{:?}", out.facts);
        assert_eq!(out.facts[0].text, "Sabharish lives in Bangalore");
        assert_eq!(out.facts[0].also_said.len(), 1);
        assert_eq!(out.facts[0].also_said[0].text, "Sabharish lives in Chennai");
        assert_eq!(out.facts[0].also_said[0].ordinal, 0);
        assert!(out.in_use, "a disagreement no longer stops Loki working");
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

        let out = entity("people/sabharish.md", &concept, today(), &HashMap::new());
        assert_eq!(out.facts.len(), 2);
        assert!(out.facts.iter().all(|f| f.also_said.is_empty()));
    }

    #[test]
    fn a_fact_read_off_a_page_says_so() {
        let mut concept = stable("Acme");
        concept.add(
            "funding",
            Claim::new("Acme raised a Series B", Origin::Web, date(2026, 1, 1)).about("funding"),
        );

        let out = entity("projects/acme.md", &concept, today(), &HashMap::new());
        assert!(out.facts[0].from_elsewhere);
        assert_eq!(source_note(Origin::Web), "I read this on a page");
    }

    /// A claim with no attribute cannot conflict, so two of them are two facts (§9.7).
    #[test]
    fn claims_with_no_attribute_never_become_a_question() {
        let mut concept = stable("Sabharish");
        concept.add("Notes", Claim::stated("something", date(2026, 1, 1)));
        concept.add("Notes", Claim::stated("something else", date(2026, 1, 1)));

        let out = entity("people/sabharish.md", &concept, today(), &HashMap::new());
        assert_eq!(out.facts.len(), 2);
        assert!(out.facts.iter().all(|f| f.also_said.is_empty()));
    }
}
