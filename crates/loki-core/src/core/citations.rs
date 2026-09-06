//! Whether an answer carries where it came from (§12.7, §21.5).
//!
//! **"Every claim in the answer carries a source URL and the span it came from. An answer where one
//! claim has no source is not finished, it is out of budget, and it says so."** That sentence is
//! the contract, and this is the part that can tell whether it was kept.
//!
//! **A check, not a fixer.** Nothing here rewrites an answer to insert a citation it did not have:
//! a citation added by machinery points at a source the sentence was not written from, which is
//! worse than a missing one because it looks like provenance. What this does is measure, so §21.5
//! has a number and §12.7 has something to say "it is out of budget" about.

use std::collections::BTreeSet;

use crate::core::websearch::Cited;

/// What an answer's citations turned out to be.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    /// Sentences that state something and carry a marker.
    pub sourced: usize,
    /// Sentences that state something and carry none.
    pub unsourced: usize,
    /// Markers pointing at a source that was never offered.
    ///
    /// **Separate from unsourced on purpose.** A missing citation is an answer that ran short; an
    /// invented one is an answer that made a source up, which is a different and worse failure and
    /// would otherwise hide inside a good-looking coverage figure.
    pub invented: BTreeSet<usize>,
    /// Sources that were offered and never used.
    pub unused: BTreeSet<usize>,
}

impl Coverage {
    /// The §21.5 number: the fraction of factual sentences carrying a source.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        let total = self.sourced + self.unsourced;
        if total == 0 {
            return 1.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a sentence count never nears 2^24"
        )]
        {
            self.sourced as f32 / total as f32
        }
    }

    /// Whether this answer met the contract.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unsourced == 0 && self.invented.is_empty()
    }
}

/// Reads an answer against the sources it was given.
#[must_use]
pub fn check(answer: &str, offered: &[Cited]) -> Coverage {
    let mut coverage = Coverage::default();
    let mut used = BTreeSet::new();

    for sentence in sentences(answer) {
        let markers = markers_in(&sentence);
        if markers.is_empty() {
            if states_a_fact(&sentence) {
                coverage.unsourced += 1;
            }
            continue;
        }
        coverage.sourced += 1;
        for marker in markers {
            if marker == 0 || marker > offered.len() {
                coverage.invented.insert(marker);
            } else {
                used.insert(marker);
            }
        }
    }

    coverage.unused = (1..=offered.len()).filter(|n| !used.contains(n)).collect();
    coverage
}

/// Splits into sentences, keeping the markers attached to the sentence they end.
fn sentences(answer: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for character in answer.chars() {
        current.push(character);
        // A marker sits after the full stop as often as before it, so a sentence does not end
        // until the punctuation has had its citations.
        if matches!(character, '.' | '!' | '?' | '\n') && !current.trim().is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    // Stitch a bracket that landed after the break back onto the sentence it belongs to.
    let mut stitched: Vec<String> = Vec::with_capacity(out.len());
    for piece in out {
        if piece.trim_start().starts_with('[')
            && let Some(previous) = stitched.last_mut()
        {
            previous.push_str(&piece);
        } else {
            stitched.push(piece);
        }
    }
    stitched
}

/// The `[n]` markers in a sentence.
fn markers_in(sentence: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let bytes = sentence.as_bytes();
    let mut at = 0;
    while let Some(open) = sentence[at..].find('[') {
        let open = open + at;
        let Some(close) = sentence[open..].find(']').map(|end| end + open) else {
            break;
        };
        let inside = &sentence[open + 1..close];
        // `[1]`, and also `[1, 2]` and `[1][2]`, which are both how a model writes two sources.
        for part in inside.split([',', ' ']) {
            if let Ok(number) = part.trim().parse::<usize>() {
                out.push(number);
            }
        }
        at = close + 1;
        if at >= bytes.len() {
            break;
        }
    }
    out
}

/// Whether a sentence asserts something that would need a source.
///
/// **Conservative, because the cost of the two errors is not symmetric.** Counting a hedge as a
/// claim understates coverage, which shows up as an answer reporting itself incomplete when it was
/// fine. Missing a real claim overstates it, which is the failure §21.5 exists to detect, so this
/// leans towards counting.
fn states_a_fact(sentence: &str) -> bool {
    let trimmed = sentence.trim();
    if trimmed.len() < 25 {
        return false;
    }
    let lowered = trimmed.to_lowercase();
    // Things that are not assertions about the world: questions, offers, and the answer talking
    // about itself.
    let not_a_claim = [
        "i could not",
        "i couldn't",
        "would you like",
        "let me know",
        "i do not have",
        "i don't have",
        "no source",
        "out of budget",
        "here is",
        "here are",
    ];
    if trimmed.ends_with('?') || not_a_claim.iter().any(|hedge| lowered.starts_with(hedge)) {
        return false;
    }
    // A list item that is only a label is not a claim either.
    trimmed.split_whitespace().count() >= 5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(count: usize) -> Vec<Cited> {
        (1..=count)
            .map(|n| Cited {
                url: format!("https://s{n}.test"),
                title: format!("t{n}"),
                text: String::new(),
                icon: None,
                read: true,
            })
            .collect()
    }

    #[test]
    fn an_answer_that_cites_everything_is_complete() {
        let answer = "Rust reached version 1.0 in May of 2015 [1]. \
                      The language is maintained by the Rust Foundation today [2].";
        let coverage = check(answer, &offered(2));
        assert_eq!(coverage.sourced, 2);
        assert_eq!(coverage.unsourced, 0);
        assert!(coverage.is_complete());
        assert!((coverage.fraction() - 1.0).abs() < f32::EPSILON);
    }

    /// §12.7: an answer with an uncited claim is out of budget, not finished.
    #[test]
    fn a_claim_without_a_source_is_counted_against_it() {
        let answer = "Rust reached version 1.0 in May of 2015 [1]. \
                      It is now the most widely admired language in the world.";
        let coverage = check(answer, &offered(1));
        assert_eq!(coverage.sourced, 1);
        assert_eq!(coverage.unsourced, 1);
        assert!(!coverage.is_complete());
        assert!((coverage.fraction() - 0.5).abs() < f32::EPSILON);
    }

    /// The worse failure, and the reason it is counted separately: a source that was never offered.
    #[test]
    fn an_invented_source_is_not_the_same_as_a_missing_one() {
        let answer = "Rust reached version 1.0 in May of 2015 [7].";
        let coverage = check(answer, &offered(2));
        assert!(coverage.invented.contains(&7));
        assert!(!coverage.is_complete());
        // It still counts as sourced, so a coverage figure alone could not have caught it.
        assert_eq!(coverage.sourced, 1);
        assert_eq!(coverage.unsourced, 0);
    }

    #[test]
    fn several_sources_on_one_sentence_are_all_read() {
        for answer in [
            "The language is fast and safe, which many people have written about [1, 2].",
            "The language is fast and safe, which many people have written about [1][2].",
        ] {
            let coverage = check(answer, &offered(2));
            assert!(coverage.unused.is_empty(), "{answer}");
        }
    }

    /// A marker after the full stop is the commonest way a model writes one.
    #[test]
    fn a_marker_outside_the_sentence_still_belongs_to_it() {
        let answer = "Rust reached version 1.0 in May of 2015. [1]";
        let coverage = check(answer, &offered(1));
        assert_eq!(coverage.unsourced, 0);
        assert_eq!(coverage.sourced, 1);
    }

    /// Hedges and questions are not claims, or an honest answer would report itself incomplete.
    #[test]
    fn the_answer_talking_about_itself_is_not_a_claim() {
        let answer = "I could not read one of the pages, so this may be incomplete. \
                      Would you like me to try the others again?";
        let coverage = check(answer, &offered(1));
        assert_eq!(coverage.unsourced, 0);
        assert!(coverage.is_complete());
    }

    #[test]
    fn a_source_nobody_used_is_reported_without_failing_the_answer() {
        let answer = "Rust reached version 1.0 in May of 2015 [1].";
        let coverage = check(answer, &offered(3));
        assert_eq!(coverage.unused, BTreeSet::from([2, 3]));
        // Not every offered source has to be used, so this alone is not incompleteness.
        assert!(coverage.is_complete());
    }

    #[test]
    fn an_answer_with_nothing_to_source_is_complete() {
        assert!(check("", &[]).is_complete());
        assert!(check("Yes.", &[]).is_complete());
    }
}
