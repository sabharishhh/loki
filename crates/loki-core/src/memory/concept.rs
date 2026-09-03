//! One OKF document. A person, a project, a preference.
//!
//! Markdown with YAML frontmatter, which is what makes the store portable: any OKF consumer can
//! read it, and so can a text editor. Bi-temporal validity on claims is our extension. OKF permits
//! producer-defined keys and requires consumers to tolerate them, so it is in spec but not
//! standard, and a future reader should know which parts travel.

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::claim::Claim;

/// How far a concept has earned its way toward being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// In `scratch/`, or awaiting a second occurrence. Never reaches a prompt.
    #[default]
    Draft,
    /// Earned its place.
    Stable,
    /// Retired by age and disuse. Still linkable and searchable, never deleted.
    Deprecated,
}

/// Who an entity is to this store (§9.4, S-21).
///
/// Exactly one `Owner` and one `Assistant`, both seeded before the first turn so an "I" or a "you"
/// always has a card to land on. Everything else defaults to `Other`, so a store written before
/// this existed reads correctly rather than declaring itself the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The person this store belongs to.
    Owner,
    /// Loki.
    Assistant,
    #[default]
    Other,
}

/// Whether `name` is a name or a placeholder standing in for one (§9.4, S-21).
///
/// "The user's sister" is a description of somebody nobody has named yet. Keeping the difference
/// is what lets a named card absorb a described one instead of the two sitting side by side for
/// ever, and it is what §17.3 needs to say "you have not told me their name".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    #[default]
    Named,
    Described,
}

/// An edge from this entity to another, bi-temporal like a claim (§9.4, S-21).
///
/// `until` is what makes a manager who changed different from a manager who was wrong. Nothing is
/// deleted: a closed edge stays in the file and stays walkable, it simply stops being current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub label: String,
    /// A bundle-relative concept path.
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Date>,
}

impl Relation {
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.until.is_none()
    }
}

fn is_other(role: &Role) -> bool {
    matches!(role, Role::Other)
}

fn is_named(label: &Label) -> bool {
    matches!(label, Label::Named)
}

/// Who wrote something, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub by: String,
    pub at: Date,
}

/// OKF frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontmatter {
    pub name: String,
    #[serde(default)]
    pub status: Status,
    pub generated: Attribution,
    /// Empty is OKF's unverified tier. An entry by a `human:` actor is the top tier and never
    /// decays by heuristic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified: Vec<Attribution>,
    /// An absolute instant, so staleness is a comparison. A trip expires. Your name does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<Date>,
    /// Other surface forms this entity is known by. Seeded from the form first used, and it grows:
    /// a learned name, a nickname and a rename all append rather than replacing the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "is_other")]
    pub role: Role,
    #[serde(default, skip_serializing_if = "is_named")]
    pub label: Label,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
    /// Where this card's contents went, when a merge folded it into another (§9.4).
    ///
    /// A tombstone rather than a deletion: links into it still resolve, git still has what it
    /// held, and a person reading the file can see what happened to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub okf_version: String,
}

impl Frontmatter {
    #[must_use]
    pub fn new(name: impl Into<String>, at: Date) -> Self {
        Self {
            name: name.into(),
            status: Status::Draft,
            generated: Attribution {
                by: "loki/0.1".to_owned(),
                at,
            },
            verified: Vec::new(),
            stale_after: None,
            aliases: Vec::new(),
            role: Role::Other,
            label: Label::Named,
            relations: Vec::new(),
            merged_into: None,
            tags: Vec::new(),
            okf_version: "0.2".to_owned(),
        }
    }

    /// Whether a human has confirmed this. Pinned, and never decays by heuristic.
    #[must_use]
    pub fn is_human_verified(&self) -> bool {
        self.verified.iter().any(|v| v.by.starts_with("human:"))
    }

    /// Whether this entity already answers to `form`.
    #[must_use]
    pub fn answers_to(&self, form: &str) -> bool {
        let form = form.trim();
        self.name.eq_ignore_ascii_case(form)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(form))
    }

    /// Records another surface form this entity is known by.
    ///
    /// The whole of change C: the list was written once at creation and never again, so a person
    /// referred to a second way became a second file. Appending costs nothing and the path, which
    /// is the identity, never moves.
    pub fn learn_alias(&mut self, form: &str) {
        let form = form.trim();
        if form.is_empty() || self.answers_to(form) {
            return;
        }
        self.aliases.push(form.to_owned());
    }

    /// Adopts a real name, keeping the old one as an alias.
    ///
    /// A rename is one field and one alias, never a move: the path is the identity (§9.4) and
    /// moving the file would break every link into it and lose the git history that makes a
    /// correction reviewable.
    pub fn rename(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() || self.name.eq_ignore_ascii_case(name) {
            self.label = Label::Named;
            return;
        }
        let was = std::mem::replace(&mut self.name, name.to_owned());
        self.learn_alias(&was);
        self.label = Label::Named;
    }

    /// The current target of a relation, if there is exactly one.
    #[must_use]
    pub fn related(&self, label: &str) -> Option<&str> {
        let mut current = self
            .relations
            .iter()
            .filter(|r| r.is_current() && r.label.eq_ignore_ascii_case(label));
        let first = current.next()?;
        // Two current targets is not an answer to "who is my X". Case 7's two brothers are
        // correct and unanswerable in the singular, and guessing between them would be worse.
        current.next().is_none().then_some(first.to.as_str())
    }

    /// Records an edge, closing an earlier one when the label may only have one live target.
    ///
    /// Many-valued by default. §21.2 names wrongly retiring a true claim as the more damaging
    /// error, and a second brother is far more common than a second mother.
    pub fn relate(&mut self, label: &str, to: &str, on: Date) {
        let label = label.trim().to_lowercase();
        if label.is_empty() || to.is_empty() {
            return;
        }
        if let Some(held) = self
            .relations
            .iter_mut()
            .find(|r| r.label == label && r.to == to)
        {
            // Said again. Reopen it rather than adding a second copy of one edge.
            held.until = None;
            return;
        }
        if super::cardinality::relation_is_single_valued(&label) {
            for held in self
                .relations
                .iter_mut()
                .filter(|r| r.label == label && r.is_current())
            {
                held.until = Some(on);
            }
        }
        self.relations.push(Relation {
            label,
            to: to.to_owned(),
            since: Some(on),
            until: None,
        });
    }
}

/// A group of claims under a heading, as the markdown lays them out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub claims: Vec<Claim>,
}

/// A concept as it sits on disk, before the gate has looked at it.
///
/// Named `Raw` because nothing here has been checked. Reaching a prompt requires becoming an
/// `Active`, which is a separate type with a checked constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawConcept {
    pub front: Frontmatter,
    pub sections: Vec<Section>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("no frontmatter. an OKF document opens with a --- fenced YAML block")]
    NoFrontmatter,
    #[error("frontmatter is not valid YAML: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("claim on line {line} has no text")]
    EmptyClaim { line: usize },
    #[error("claim attribute on line {line} is not `key: value`: {found}")]
    BadAttribute { line: usize, found: String },
    #[error("line {line}: {what} is not a date in YYYY-MM-DD form: {found}")]
    BadDate {
        line: usize,
        what: &'static str,
        found: String,
    },
    #[error("claim on line {line} has no valid_from")]
    NoValidFrom { line: usize },
}

impl RawConcept {
    #[must_use]
    pub const fn new(front: Frontmatter) -> Self {
        Self {
            front,
            sections: Vec::new(),
        }
    }

    /// Every claim, across all sections.
    pub fn claims(&self) -> impl Iterator<Item = &Claim> {
        self.sections.iter().flat_map(|s| s.claims.iter())
    }

    pub fn claims_mut(&mut self) -> impl Iterator<Item = &mut Claim> {
        self.sections.iter_mut().flat_map(|s| s.claims.iter_mut())
    }

    /// Adds a claim under a heading, creating the section if it is new.
    pub fn add(&mut self, heading: &str, claim: Claim) {
        if let Some(section) = self.sections.iter_mut().find(|s| s.heading == heading) {
            section.claims.push(claim);
        } else {
            self.sections.push(Section {
                heading: heading.to_owned(),
                claims: vec![claim],
            });
        }
    }

    /// Whether this concept has passed its expiry.
    #[must_use]
    pub fn is_stale_on(&self, day: Date) -> bool {
        self.front.stale_after.is_some_and(|end| day >= end)
    }
}

const FENCE: &str = "---";

/// Parses an OKF document.
///
/// # Errors
/// Fails on missing or malformed frontmatter, and on a claim whose attributes cannot be read.
/// Unknown attribute keys are kept rather than rejected: OKF requires consumers to tolerate
/// producer-defined fields, and rejecting one would make the store less portable, not more.
pub fn parse(text: &str) -> Result<RawConcept, ParseError> {
    let body = strip_frontmatter(text)?;
    let front: Frontmatter = serde_yaml_ng::from_str(body.front)?;

    let mut sections: Vec<Section> = Vec::new();
    let mut pending: Option<Claim> = None;

    for (offset, line) in body.rest.lines().enumerate() {
        let line_no = body.first_body_line + offset;
        let trimmed = line.trim();

        if let Some(heading) = trimmed.strip_prefix("## ") {
            flush(&mut pending, &mut sections);
            sections.push(Section {
                heading: heading.trim().to_owned(),
                claims: Vec::new(),
            });
        } else if let Some(text) = trimmed.strip_prefix("- ") {
            flush(&mut pending, &mut sections);
            let text = text.trim();
            if text.is_empty() {
                return Err(ParseError::EmptyClaim { line: line_no });
            }
            pending = Some(blank_claim(text));
        } else if let Some(claim) = pending.as_mut()
            && !trimmed.is_empty()
        {
            apply_attribute(claim, trimmed, line_no)?;
        }
    }
    flush(&mut pending, &mut sections);

    Ok(RawConcept { front, sections })
}

struct Split<'a> {
    front: &'a str,
    rest: &'a str,
    first_body_line: usize,
}

fn strip_frontmatter(text: &str) -> Result<Split<'_>, ParseError> {
    let rest = text
        .strip_prefix(FENCE)
        .and_then(|r| r.strip_prefix('\n'))
        .ok_or(ParseError::NoFrontmatter)?;
    let end = rest.find("\n---").ok_or(ParseError::NoFrontmatter)?;
    let front = &rest[..end];
    let after = rest[end..]
        .strip_prefix("\n---")
        .and_then(|r| r.strip_prefix('\n'))
        .unwrap_or("");
    Ok(Split {
        front,
        rest: after,
        // Two fence lines plus the frontmatter, and lines are one-based.
        first_body_line: front.lines().count() + 3,
    })
}

/// A claim with a placeholder `learned`, filled in by the attribute lines that follow it.
///
/// `origin` starts inferred and `valid_from` starts absent, so a v0.8 file that names neither
/// reads as a guess with no world time, which is the conservative direction on both.
fn blank_claim(text: &str) -> Claim {
    use super::claim::{Confidence, Origin, Privacy, Validity};
    Claim {
        text: text.to_owned(),
        attribute: String::new(),
        validity: Validity::undated(Date::constant(1970, 1, 1)),
        confidence: Confidence::Medium,
        origin: Origin::Inferred,
        privacy: Privacy::Normal,
        replaced_by: None,
        usage_count: 0,
        evidence: Vec::new(),
        recalls: 0,
        recall_days: 0,
        recall_queries: 0,
    }
}

fn flush(pending: &mut Option<Claim>, sections: &mut Vec<Section>) {
    let Some(claim) = pending.take() else { return };
    if let Some(section) = sections.last_mut() {
        section.claims.push(claim);
    } else {
        sections.push(Section {
            heading: String::new(),
            claims: vec![claim],
        });
    }
}

fn apply_attribute(claim: &mut Claim, line: &str, line_no: usize) -> Result<(), ParseError> {
    use super::claim::{Confidence, Origin, Privacy};

    // `evidence` is the one key whose value is free text, and a URL contains a colon, so
    // `split_pairs` would read `https:` as the start of a second pair. Taken whole, before the
    // split, rather than teaching the splitter about schemes.
    if let Some(value) = line.strip_prefix("evidence:") {
        if let Some(anchor) = evidence(value.trim()) {
            claim.evidence.push(anchor);
        }
        return Ok(());
    }

    // Attributes may share a line: `confidence: high   origin: stated`.
    for pair in split_pairs(line) {
        let (key, value) = pair
            .split_once(':')
            .ok_or_else(|| ParseError::BadAttribute {
                line: line_no,
                found: pair.to_owned(),
            })?;
        let value = value.trim();

        match key.trim() {
            "valid_from" => claim.validity.valid_from = maybe_date(value, line_no, "valid_from")?,
            "valid_to" => claim.validity.valid_to = maybe_date(value, line_no, "valid_to")?,
            "learned" => claim.validity.learned = date(value, line_no, "learned")?,
            "unlearned" => claim.validity.unlearned = maybe_date(value, line_no, "unlearned")?,
            "confidence" => {
                claim.confidence = match value {
                    "low" => Confidence::Low,
                    "high" => Confidence::High,
                    _ => Confidence::Medium,
                }
            }
            // `about` is what v0.8-era files were written with, before §9.13 named the field.
            "attribute" | "about" => claim.attribute = super::claim::normalize_attribute(value),
            // Likewise `source`, which §9.12 renamed and widened.
            "origin" | "source" => {
                claim.origin = match value {
                    "stated" => Origin::Stated,
                    "web" => Origin::Web,
                    "connector" => Origin::Connector,
                    // Anything unrecognised reads as a guess. OKF says tolerate an unknown value,
                    // and §9.12 says the safe reading of unknown provenance is not-from-the-user.
                    _ => Origin::Inferred,
                }
            }
            "privacy" => {
                claim.privacy = if value == "private" {
                    Privacy::Private
                } else {
                    Privacy::Normal
                }
            }
            "replaced_by" => claim.replaced_by = (value != "null").then(|| value.to_owned()),
            "usage_count" => claim.usage_count = value.parse().unwrap_or(0),
            "recalls" => claim.recalls = value.parse().unwrap_or(0),
            "recall_days" => claim.recall_days = value.parse().unwrap_or(0),
            "recall_queries" => claim.recall_queries = value.parse().unwrap_or(0),
            // Unknown keys are ignored, not rejected. OKF requires that of consumers.
            _ => {}
        }
    }
    Ok(())
}

/// Splits `a: 1   b: 2` into `a: 1` and `b: 2`.
///
/// A value can contain spaces, so the split point is a run of whitespace followed by `word:`.
fn split_pairs(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut pairs = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b' ' && i + 1 < bytes.len() {
            let after = line[i..].trim_start();
            let looks_like_key = after.split_once(':').is_some_and(|(k, _)| {
                !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
            if looks_like_key && line[start..i].contains(':') {
                pairs.push(line[start..i].trim());
                start = line.len() - after.len();
                i = start;
                continue;
            }
        }
        i += 1;
    }
    let tail = line[start..].trim();
    if !tail.is_empty() {
        pairs.push(tail);
    }
    pairs
}

fn date(value: &str, line: usize, what: &'static str) -> Result<Date, ParseError> {
    value.parse().map_err(|_| ParseError::BadDate {
        line,
        what,
        found: value.to_owned(),
    })
}

/// Reads one `evidence:` line, `<hash> <source>` with an optional ` #<span>`.
///
/// One line per anchor rather than a nested list, because a claim block is flat `key: value` and a
/// repeated key diffs one anchor at a time.
fn evidence(value: &str) -> Option<super::claim::EvidenceRef> {
    let (hash, rest) = value.split_once(char::is_whitespace)?;
    let (source, span) = rest.trim().split_once(" #").map_or_else(
        || (rest.trim(), None),
        |(s, span)| (s.trim(), Some(span.trim().to_owned())),
    );
    (!hash.is_empty() && !source.is_empty()).then(|| super::claim::EvidenceRef {
        source: source.to_owned(),
        hash: hash.to_owned(),
        span,
    })
}

fn maybe_date(value: &str, line: usize, what: &'static str) -> Result<Option<Date>, ParseError> {
    if value == "null" || value.is_empty() {
        return Ok(None);
    }
    date(value, line, what).map(Some)
}

/// Renders a concept back to markdown.
///
/// Field order is fixed so an unchanged concept produces an identical file. A git diff should show
/// what actually changed, not a reshuffle.
///
/// # Panics
/// If the frontmatter cannot be serialized, which needs a non-string map key it cannot have.
#[must_use]
pub fn render(concept: &RawConcept) -> String {
    use super::claim::{Confidence, Origin, Privacy};

    let mut out = String::from(FENCE);
    out.push('\n');
    out.push_str(&serde_yaml_ng::to_string(&concept.front).expect("frontmatter is serializable"));
    out.push_str(FENCE);
    out.push('\n');

    for section in &concept.sections {
        out.push('\n');
        if !section.heading.is_empty() {
            out.push_str("## ");
            out.push_str(&section.heading);
            out.push('\n');
        }
        for claim in &section.claims {
            out.push_str("- ");
            out.push_str(&claim.text);
            out.push('\n');

            // Written before the dates because it is what the claim is *about*, and a reader
            // scanning the file should see that before its validity window.
            if !claim.attribute.is_empty() {
                out.push_str(&format!("  attribute: {}\n", claim.attribute));
            }

            let v = &claim.validity;
            out.push_str(&format!(
                "  valid_from: {}   valid_to: {}\n",
                v.valid_from
                    .map_or_else(|| "null".to_owned(), |d| d.to_string()),
                v.valid_to
                    .map_or_else(|| "null".to_owned(), |d| d.to_string())
            ));
            out.push_str(&format!(
                "  learned: {}   unlearned: {}\n",
                v.learned,
                v.unlearned
                    .map_or_else(|| "null".to_owned(), |d| d.to_string())
            ));

            let confidence = match claim.confidence {
                Confidence::Low => "low",
                Confidence::Medium => "medium",
                Confidence::High => "high",
            };
            let origin = match claim.origin {
                Origin::Inferred => "inferred",
                Origin::Stated => "stated",
                Origin::Web => "web",
                Origin::Connector => "connector",
            };
            out.push_str(&format!("  confidence: {confidence}   origin: {origin}\n"));

            if claim.privacy == Privacy::Private {
                out.push_str("  privacy: private\n");
            }
            if let Some(replacement) = &claim.replaced_by {
                out.push_str(&format!("  replaced_by: {replacement}\n"));
            }
            if claim.usage_count > 0 {
                out.push_str(&format!("  usage_count: {}\n", claim.usage_count));
            }
            // Counted signals, written only once there is something to count, so a fresh file
            // stays quiet and a diff shows movement rather than a row of zeroes.
            if claim.recalls > 0 || claim.recall_days > 0 || claim.recall_queries > 0 {
                out.push_str(&format!(
                    "  recalls: {}   recall_days: {}   recall_queries: {}\n",
                    claim.recalls, claim.recall_days, claim.recall_queries
                ));
            }
            for anchor in &claim.evidence {
                out.push_str(&format!("  evidence: {} {}", anchor.hash, anchor.source));
                if let Some(span) = &anchor.span {
                    out.push_str(&format!(" #{span}"));
                }
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::claim::{Confidence, Origin, Privacy};
    use jiff::civil::date;

    /// A file written by v0.8 has to keep reading. OKF requires consumers to tolerate what they
    /// do not recognise, and §9.12 depends on that rule specifically: a bundle carrying an origin
    /// this version does not know must degrade rather than break.
    #[test]
    fn a_v0_8_shaped_file_still_reads() {
        const OLD: &str = r"---
name: Sabharish
status: stable
generated:
  by: loki/0.1
  at: 2026-09-02
okf_version: '0.2'
---

## name
- The user's name is Sabharish
  about: name
  valid_from: 2026-09-02   valid_to: null
  learned: 2026-09-02   unlearned: null
  confidence: high   source: stated
";
        let concept = parse(OLD).expect("a v0.8 file parses");
        let claim = concept.claims().next().expect("claim");
        assert_eq!(claim.attribute, "name", "`about` is the old spelling");
        assert_eq!(claim.origin, Origin::Stated, "`source` is the old spelling");
        assert_eq!(
            claim.validity.valid_from,
            Some(date(2026, 9, 2)),
            "a date the old file gave is a date, not a guess at one"
        );
        assert!(claim.evidence.is_empty());
        assert_eq!(claim.recalls, 0);
    }

    /// An origin nobody has heard of reads as a guess, never as something the user said.
    #[test]
    fn an_unknown_origin_degrades_to_inferred() {
        const FUTURE: &str = r"---
name: Sabharish
status: stable
generated:
  by: loki/0.9
  at: 2026-09-02
okf_version: '0.2'
---

## name
- Told to us by another agent
  attribute: name
  learned: 2026-09-02   unlearned: null
  confidence: high   origin: peer
";
        let concept = parse(FUTURE).expect("an unknown origin does not break the parse");
        assert_eq!(
            concept.claims().next().expect("claim").origin,
            Origin::Inferred
        );
    }

    #[test]
    fn evidence_and_recall_counts_round_trip() {
        use crate::memory::claim::EvidenceRef;

        let mut concept = RawConcept::new(Frontmatter::new("Acme", date(2026, 9, 2)));
        let mut claim = Claim::stated("Acme raised a Series B", date(2026, 9, 2))
            .about("funding")
            .citing(EvidenceRef {
                source: "https://example.com/acme".to_owned(),
                hash: "b4c9a2".to_owned(),
                span: Some("L20-L24".to_owned()),
            })
            .citing(EvidenceRef {
                source: "https://example.com/press".to_owned(),
                hash: "77de10".to_owned(),
                span: None,
            });
        claim.recalls = 7;
        claim.recall_days = 3;
        claim.recall_queries = 4;
        concept.add("funding", claim);

        let reparsed = parse(&render(&concept)).expect("round trip");
        let back = reparsed.claims().next().expect("claim");
        assert_eq!(
            back.evidence.len(),
            2,
            "a claim can rest on several mentions"
        );
        assert_eq!(back.evidence[0].hash, "b4c9a2");
        assert_eq!(back.evidence[0].span.as_deref(), Some("L20-L24"));
        assert_eq!(back.evidence[1].span, None);
        assert_eq!(
            (back.recalls, back.recall_days, back.recall_queries),
            (7, 3, 4)
        );
    }

    /// The worked example from section 9.5, as it would sit on disk.
    const MEERA: &str = r"---
name: Meera
status: stable
generated:
  by: loki/0.1
  at: 2026-03-12
verified:
- by: 'human:sabharish'
  at: 2026-08-29
aliases:
- Meera Raghunathan
tags:
- person
okf_version: '0.2'
---

## Role
- Works on the infra team
  valid_from: 2026-07-15   valid_to: null
  learned: 2026-08-29   unlearned: null
  confidence: high   source: stated

- Works on the platform team
  valid_from: 2026-03-12   valid_to: 2026-07-15
  learned: 2026-03-12   unlearned: 2026-08-29
  confidence: high   source: stated
  replaced_by: Works on the infra team
";

    #[test]
    fn a_document_parses_into_frontmatter_and_claims() {
        let concept = parse(MEERA).expect("parse");
        assert_eq!(concept.front.name, "Meera");
        assert_eq!(concept.front.status, Status::Stable);
        assert!(concept.front.is_human_verified());
        assert_eq!(concept.front.aliases, ["Meera Raghunathan"]);
        assert_eq!(concept.sections.len(), 1);
        assert_eq!(concept.sections[0].heading, "Role");
        assert_eq!(concept.sections[0].claims.len(), 2);
    }

    #[test]
    fn both_timelines_survive_the_parse() {
        let concept = parse(MEERA).expect("parse");
        let old = &concept.sections[0].claims[1];

        assert_eq!(old.validity.valid_from, Some(date(2026, 3, 12)));
        assert_eq!(old.validity.valid_to, Some(date(2026, 7, 15)));
        assert_eq!(old.validity.learned, date(2026, 3, 12));
        assert_eq!(old.validity.unlearned, Some(date(2026, 8, 29)));
        assert_eq!(old.replaced_by.as_deref(), Some("Works on the infra team"));
        // The sentence the product exists to be able to say.
        assert_eq!(old.validity.wrong_for_days(), Some(45));
    }

    #[test]
    fn attributes_sharing_a_line_are_both_read() {
        let concept = parse(MEERA).expect("parse");
        let claim = &concept.sections[0].claims[0];
        assert_eq!(claim.confidence, Confidence::High);
        assert_eq!(claim.origin, Origin::Stated);
    }

    #[test]
    fn rendering_a_parsed_document_reproduces_it() {
        let concept = parse(MEERA).expect("parse");
        let rendered = render(&concept);
        let reparsed = parse(&rendered).expect("reparse");
        assert_eq!(concept, reparsed, "a round trip changed the concept");
    }

    #[test]
    fn rendering_is_byte_stable_so_diffs_stay_readable() {
        let concept = parse(MEERA).expect("parse");
        let once = render(&concept);
        let twice = render(&parse(&once).expect("reparse"));
        assert_eq!(once, twice, "rendering twice produced different bytes");
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let mut concept = RawConcept::new(Frontmatter::new("Sabharish", date(2026, 1, 1)));
        concept.front.status = Status::Stable;
        concept.front.stale_after = Some(date(2027, 1, 1));
        concept.front.tags = vec!["person".into()];
        concept.front.verified.push(Attribution {
            by: "human:sabharish".into(),
            at: date(2026, 2, 1),
        });

        let mut claim = Claim::stated("Sees a therapist on Thursdays", date(2026, 1, 1));
        claim.privacy = Privacy::Private;
        claim.usage_count = 4;
        concept.add("Health", claim);
        concept.add(
            "Work",
            Claim::inferred("Builds Loki", date(2026, 1, 5)).dated(date(2026, 1, 1)),
        );

        let reparsed = parse(&render(&concept)).expect("reparse");
        assert_eq!(concept, reparsed);
        assert_eq!(reparsed.claims().count(), 2);
        assert_eq!(reparsed.sections[0].claims[0].privacy, Privacy::Private);
        assert_eq!(reparsed.sections[0].claims[0].usage_count, 4);
        assert_eq!(reparsed.front.stale_after, Some(date(2027, 1, 1)));
    }

    #[test]
    fn a_document_without_frontmatter_is_rejected() {
        assert!(matches!(
            parse("## Role\n- Works on infra\n"),
            Err(ParseError::NoFrontmatter)
        ));
    }

    #[test]
    fn a_bad_date_names_the_line_and_the_field() {
        let text = "---\nname: X\ngenerated:\n  by: loki\n  at: 2026-01-01\n---\n\n## R\n- A claim\n  valid_from: yesterday\n";
        match parse(text) {
            Err(ParseError::BadDate { what, found, .. }) => {
                assert_eq!(what, "valid_from");
                assert_eq!(found, "yesterday");
            }
            other => panic!("expected a date error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_attributes_are_tolerated_not_rejected() {
        // OKF requires consumers to accept producer-defined fields. Rejecting one would make the
        // store less portable, not more.
        let text = "---\nname: X\ngenerated:\n  by: loki\n  at: 2026-01-01\n---\n\n## R\n- A claim\n  valid_from: 2026-01-01   learned: 2026-01-01\n  some_future_field: whatever\n";
        let concept = parse(text).expect("unknown fields should not fail the parse");
        assert_eq!(concept.claims().count(), 1);
    }

    #[test]
    fn staleness_is_a_comparison_against_an_instant() {
        let mut concept = RawConcept::new(Frontmatter::new("Trip", date(2026, 1, 1)));
        assert!(
            !concept.is_stale_on(date(2030, 1, 1)),
            "no expiry means never stale"
        );

        concept.front.stale_after = Some(date(2026, 6, 1));
        assert!(!concept.is_stale_on(date(2026, 5, 31)));
        assert!(concept.is_stale_on(date(2026, 6, 1)));
    }
}
