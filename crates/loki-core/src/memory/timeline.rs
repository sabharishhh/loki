//! The memory timeline (§17.3).
//!
//! Every promotion, correction and invalidation in plain language. Backed by `log.md` and git
//! history, so it costs almost nothing to build.
//!
//! This is the trust surface for the whole product. Correctness under the hood is not what a user
//! feels; being able to check the work is. So the file is written to be read by a person first and
//! parsed second, and `open file` in Finder shows exactly what the timeline shows.

use jiff::civil::Date;
use serde::Serialize;

use super::bundle::{Bundle, BundleError, LOG};
use super::concept::RawConcept;
use super::consolidate::Report;
use super::reconcile::Precedence;

/// What happened to a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A claim earned `stable`.
    Learned,
    /// A claim replaced another. The row §9.5 exists to make writable.
    Corrected,
    /// A new entity was written, and is held as `draft` until it earns `stable` (§9.8).
    ///
    /// Shown rather than hidden. It is the most common thing that happens after you say
    /// something, and a timeline that stays empty while files are being written is not a trust
    /// surface, it is a lie about the store it claims to reflect.
    Noted,
    /// Two claims conflicted and neither was used. One tap.
    NeedsYou,
}

/// One row of the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The day Loki was told, which is system time and not when the fact became true.
    pub day: Date,
    pub kind: Kind,
    pub concept: String,
    pub text: String,
    /// World time: when the live claim started being true.
    pub from: Option<Date>,
    /// The claim this replaced, for the correction pair.
    pub replaced: Option<String>,
    pub replaced_from: Option<Date>,
    pub replaced_to: Option<Date>,
    /// How long the old claim was believed after it stopped being true.
    pub wrong_for_days: Option<i64>,
}

/// Renders a consolidation run as timeline rows.
///
/// Takes the concepts as they now stand, because the two date ranges a correction needs live on
/// the claims rather than in the decision.
#[must_use]
pub fn rows(report: &Report, concepts: &[(String, RawConcept)], day: Date) -> Vec<Entry> {
    let mut out = Vec::new();

    for decided in &report.decisions {
        let concept = concepts
            .iter()
            .find(|(path, _)| *path == decided.concept)
            .map(|(_, c)| c);
        let held = concept.and_then(|c| c.claims().find(|m| m.text == decided.held));
        let live = concept.and_then(|c| c.claims().find(|m| m.text == decided.incoming));

        match decided.outcome {
            Precedence::Replace => out.push(Entry {
                day,
                kind: Kind::Corrected,
                concept: decided.concept.clone(),
                text: decided.incoming.clone(),
                from: live.map(|c| c.validity.valid_from),
                replaced: Some(decided.held.clone()),
                replaced_from: held.map(|c| c.validity.valid_from),
                replaced_to: held.and_then(|c| c.validity.valid_to),
                wrong_for_days: held.and_then(|c| c.validity.wrong_for_days()),
            }),
            Precedence::Surface => out.push(Entry {
                day,
                kind: Kind::NeedsYou,
                concept: decided.concept.clone(),
                text: decided.incoming.clone(),
                from: live.map(|c| c.validity.valid_from),
                replaced: Some(decided.held.clone()),
                replaced_from: held.map(|c| c.validity.valid_from),
                replaced_to: None,
                wrong_for_days: None,
            }),
            // A claim that lost to a better one is not news. §17.4's restraint applies here too.
            Precedence::Keep => {}
        }
    }

    for path in &report.created {
        let concept = concepts.iter().find(|(p, _)| p == path).map(|(_, c)| c);
        let text = concept
            .and_then(|c| c.claims().next().map(|m| m.text.clone()))
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        // A stated fact is live at once and reads as learned. An inferred one is held, and
        // saying so is more honest than implying it is in use.
        let held = concept.is_none_or(|c| c.front.status != super::concept::Status::Stable);
        out.push(Entry {
            day,
            kind: if held { Kind::Noted } else { Kind::Learned },
            concept: path.clone(),
            text,
            from: None,
            replaced: None,
            replaced_from: None,
            replaced_to: None,
            wrong_for_days: None,
        });
    }

    for path in &report.promoted {
        let text = concepts
            .iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, c)| c.claims().next().map(|m| m.text.clone()))
            .unwrap_or_default();
        out.push(Entry {
            day,
            kind: Kind::Learned,
            concept: path.clone(),
            text,
            from: None,
            replaced: None,
            replaced_from: None,
            replaced_to: None,
            wrong_for_days: None,
        });
    }
    out
}

/// Appends rows to `log.md`, grouped under the day.
///
/// # Errors
/// Fails if the bundle cannot be written.
pub async fn append(bundle: &Bundle, entries: &[Entry], day: Date) -> Result<(), BundleError> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut out = format!("\n## {day}\n\n");
    for entry in entries {
        out.push_str("- ");
        out.push_str(&render(entry));
        out.push('\n');
    }
    let writer = bundle.writer().await;
    writer.append(LOG, &out)
}

/// One row as a sentence.
///
/// §17.3's example, and the reason §9.5 tracks four timestamps: with one timeline this would read
/// "this replaced that", losing the part a person actually cares about.
#[must_use]
pub fn render(entry: &Entry) -> String {
    match entry.kind {
        Kind::Learned => format!("learned, {}: {}", entry.concept, entry.text),
        Kind::Noted => format!("noted, {}: {}", entry.concept, entry.text),
        Kind::NeedsYou => format!(
            "needs you, {}: \"{}\" against \"{}\", and neither is being used",
            entry.concept,
            entry.replaced.as_deref().unwrap_or(""),
            entry.text
        ),
        Kind::Corrected => {
            let mut line = format!("corrected, {}: \"{}\"", entry.concept, entry.text);
            if let Some(from) = entry.from {
                line.push_str(&format!(" from {from}"));
            }
            if let Some(replaced) = &entry.replaced {
                line.push_str(&format!(", replacing \"{replaced}\""));
                if let Some(since) = entry.replaced_from {
                    line.push_str(&format!(" held since {since}"));
                }
            }
            if let Some(days) = entry.wrong_for_days.filter(|d| *d > 0) {
                line.push_str(&format!(", wrong for {days} days"));
            }
            line
        }
    }
}

/// Up to three lines for the session summary (§17.4).
///
/// Restraint is the whole design. Only a promotion or an invalidation fires; a confidence bump is
/// not news. Silence when nothing happened, because a card that says "learned nothing today"
/// teaches people to ignore the card.
#[must_use]
pub fn summary(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.kind != Kind::Learned || !e.text.is_empty())
        .take(3)
        .map(render)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn corrected() -> Entry {
        Entry {
            day: date(2026, 8, 29),
            kind: Kind::Corrected,
            concept: "people/sabharish.md".to_string(),
            text: "on the infra team".to_string(),
            from: Some(date(2026, 7, 15)),
            replaced: Some("on the design team".to_string()),
            replaced_from: Some(date(2026, 3, 1)),
            replaced_to: Some(date(2026, 7, 15)),
            wrong_for_days: Some(45),
        }
    }

    /// §17.3's sentence, which is only writable because of §9.5's four timestamps.
    #[test]
    fn a_correction_says_what_it_replaced_and_for_how_long_it_was_wrong() {
        let line = render(&corrected());
        assert!(line.contains("on the infra team"), "{line}");
        assert!(line.contains("from 2026-07-15"), "{line}");
        assert!(line.contains("replacing \"on the design team\""), "{line}");
        assert!(line.contains("held since 2026-03-01"), "{line}");
        assert!(line.contains("wrong for 45 days"), "{line}");
    }

    #[test]
    fn a_correction_that_was_never_wrong_does_not_say_so() {
        let mut entry = corrected();
        entry.wrong_for_days = Some(0);
        assert!(!render(&entry).contains("wrong for"), "{}", render(&entry));
    }

    #[test]
    fn the_summary_is_capped_at_three_lines() {
        let entries: Vec<Entry> = (0..7).map(|_| corrected()).collect();
        assert_eq!(summary(&entries).len(), 3);
    }

    #[test]
    fn nothing_happening_says_nothing() {
        assert!(summary(&[]).is_empty());
    }

    #[test]
    fn a_kept_claim_is_not_a_timeline_row() {
        let report = Report {
            decisions: vec![super::super::reconcile::Decided {
                concept: "people/dan.md".to_string(),
                held: "a".to_string(),
                incoming: "b".to_string(),
                outcome: Precedence::Keep,
            }],
            ..Report::default()
        };
        assert!(rows(&report, &[], date(2026, 9, 1)).is_empty());
    }
}
