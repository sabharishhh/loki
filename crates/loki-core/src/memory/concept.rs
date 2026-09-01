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
    /// Other surface forms this entity is known by. Seeded from the form first used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
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
            tags: Vec::new(),
            okf_version: "0.2".to_owned(),
        }
    }

    /// Whether a human has confirmed this. Pinned, and never decays by heuristic.
    #[must_use]
    pub fn is_human_verified(&self) -> bool {
        self.verified.iter().any(|v| v.by.starts_with("human:"))
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
        } else if !trimmed.is_empty() && pending.is_some() {
            let claim = pending.as_mut().expect("checked");
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

/// A claim with placeholder dates, filled in by the attribute lines that follow it.
fn blank_claim(text: &str) -> Claim {
    use super::claim::{Confidence, Privacy, Source, Validity};
    let epoch = Date::constant(1970, 1, 1);
    Claim {
        text: text.to_owned(),
        validity: Validity::open(epoch, epoch),
        confidence: Confidence::Medium,
        source: Source::Inferred,
        privacy: Privacy::Normal,
        replaced_by: None,
        usage_count: 0,
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
    use super::claim::{Confidence, Privacy, Source};

    // Attributes may share a line: `confidence: high   source: stated`.
    for pair in split_pairs(line) {
        let (key, value) = pair
            .split_once(':')
            .ok_or_else(|| ParseError::BadAttribute {
                line: line_no,
                found: pair.to_owned(),
            })?;
        let value = value.trim();

        match key.trim() {
            "valid_from" => claim.validity.valid_from = date(value, line_no, "valid_from")?,
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
            "source" => {
                claim.source = if value == "stated" {
                    Source::Stated
                } else {
                    Source::Inferred
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
    use super::claim::{Confidence, Privacy, Source};

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

            let v = &claim.validity;
            out.push_str(&format!(
                "  valid_from: {}   valid_to: {}\n",
                v.valid_from,
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
            let source = match claim.source {
                Source::Inferred => "inferred",
                Source::Stated => "stated",
            };
            out.push_str(&format!("  confidence: {confidence}   source: {source}\n"));

            if claim.privacy == Privacy::Private {
                out.push_str("  privacy: private\n");
            }
            if let Some(replacement) = &claim.replaced_by {
                out.push_str(&format!("  replaced_by: {replacement}\n"));
            }
            if claim.usage_count > 0 {
                out.push_str(&format!("  usage_count: {}\n", claim.usage_count));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::claim::{Confidence, Privacy, Source};
    use jiff::civil::date;

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

        assert_eq!(old.validity.valid_from, date(2026, 3, 12));
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
        assert_eq!(claim.source, Source::Stated);
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
            Claim::inferred("Builds Loki", date(2026, 1, 1), date(2026, 1, 5)),
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
