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
use std::ops::Not as _;

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

/// Takes named markers out of an answer, leaving the prose whole.
///
/// **Used for markers that point at nothing.** Removing the citation and keeping the claim is the
/// honest repair: the reader sees a sentence with no source, which is true, instead of a number
/// that opens nothing.
#[must_use]
pub fn without(answer: &str, drop: &BTreeSet<usize>) -> String {
    let mut out = String::with_capacity(answer.len());
    let mut rest = answer;

    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|at| at + open) else {
            break;
        };
        let inside = &rest[open + 1..close];
        let drops = inside
            .parse::<usize>()
            .is_ok_and(|number| drop.contains(&number));
        out.push_str(&rest[..open]);
        if !drops {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);

    // A marker lifted out of "Patna. [5] Born" leaves two spaces, and one before a full stop.
    let mut tidied = out;
    while tidied.contains("  ") {
        tidied = tidied.replace("  ", " ");
    }
    for mark in [".", ",", ";", ":", "!", "?"] {
        tidied = tidied.replace(&format!(" {mark}"), mark);
    }
    tidied.trim().to_owned()
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
            // **A source with nothing read from it was never evidence.** The search offers what it
            // found alongside what it opened, so a title the engine returned and nobody fetched
            // still carries a number; a sentence written "from" one of those was not written from
            // anything, which is the invented case wearing a real number (B-88).
            let real = marker > 0
                && offered
                    .get(marker - 1)
                    .is_some_and(|source| !source.text.trim().is_empty());
            if real {
                used.insert(marker);
            } else {
                coverage.invented.insert(marker);
            }
        }
    }

    coverage.unused = (1..=offered.len())
        .filter(|n| !used.contains(n))
        .filter(|n| offered[n - 1].text.trim().is_empty().not())
        .collect();
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
                // A source with no text is not a source, which is the whole of B-88. Fixtures that
                // left this empty were describing something the search never produces.
                text: format!("what page {n} said"),
                icon: None,
                icon_hash: None,
                read: true,
            })
            .collect()
    }

    /// B-88, as the reader saw it: six sources under the answer, three of them never opened.
    ///
    /// The search offers what it found alongside what it read, so a title the engine returned
    /// carries a number in the list without a word of the page behind it. A sentence citing one of
    /// those was not written from anything, and it looks exactly like a sentence that was.
    #[test]
    fn a_source_that_was_found_but_never_opened_cannot_be_cited() {
        let mut found = offered(3);
        found[2].text = String::new();
        found[2].read = false;

        let coverage = check("Bitcoin trades around forty thousand dollars. [3]", &found);
        assert_eq!(coverage.invented, [3].into_iter().collect());
        assert!(!coverage.is_complete());
        assert!(
            !coverage.unused.contains(&3),
            "a source nobody could have used is not a source that went unused"
        );
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

#[cfg(test)]
mod invented_markers {
    use super::*;

    fn cited(urls: &[&str]) -> Vec<Cited> {
        urls.iter()
            .map(|url| Cited {
                url: (*url).to_owned(),
                title: String::new(),
                text: format!("what {url} said"),
                icon: None,
                icon_hash: None,
                read: true,
            })
            .collect()
    }

    /// B-82, as the reader saw it: seven sentences each carrying `[5]` on a turn where nothing had
    /// been fetched.
    #[test]
    fn an_answer_citing_nothing_it_was_given_loses_its_markers() {
        let answer = "She took office on 25 July 2022. [5] She was born in 1958. [5]";
        let coverage = check(answer, &[]);
        assert_eq!(coverage.invented, BTreeSet::from([5]));
        assert_eq!(
            without(answer, &coverage.invented),
            "She took office on 25 July 2022. She was born in 1958."
        );
    }

    /// The claim is what the reader came for and it survives. Only the false evidence goes.
    #[test]
    fn the_sentence_survives_the_marker_being_removed() {
        let answer = "The river crossed its mark [9] in Patna.";
        let coverage = check(answer, &cited(&["https://a.example"]));
        assert_eq!(
            without(answer, &coverage.invented),
            "The river crossed its mark in Patna."
        );
    }

    /// A real citation is untouched, and one invented marker beside it does not take it down.
    #[test]
    fn a_real_marker_is_kept_when_a_false_one_is_dropped() {
        let answer = "First [1]. Second [7].";
        let coverage = check(answer, &cited(&["https://a.example"]));
        assert_eq!(coverage.invented, BTreeSet::from([7]));
        assert_eq!(without(answer, &coverage.invented), "First [1]. Second.");
    }

    #[test]
    fn text_with_nothing_to_drop_comes_back_as_it_was() {
        let answer = "Nothing here is cited at all.";
        assert_eq!(without(answer, &BTreeSet::new()), answer);
    }

    /// An array index is not a marker, so it is never a candidate for removal.
    #[test]
    fn an_array_index_is_not_dropped() {
        let answer = "Read items[0] carefully.";
        assert_eq!(
            without(answer, &BTreeSet::from([0])),
            "Read items carefully."
        );
    }
}
