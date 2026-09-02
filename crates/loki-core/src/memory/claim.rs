//! One statement inside a concept, with a bi-temporal validity window.
//!
//! Four timestamps, not one. World time is when a claim was true; system time is when Loki learned
//! it. A single timeline cannot express "on 29 August we found out the job change happened on
//! 15 July", which is the sentence the whole product is built to be able to say.

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// How a claim came to be known. Decides who wins a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Guessed from context. Everything from import is inferred.
    Inferred,
    /// The user said it. Beats inferred regardless of age.
    Stated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Who may see a claim, and where it may be sent.
///
/// Local-first means the store is yours and you control what is eligible to leave, not that
/// nothing leaves. Any claim entering a prompt goes to whichever provider is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    /// Eligible for the working set and for pre-fetch.
    #[default]
    Normal,
    /// Never in the working set, never pre-fetched. Retrieved only when a task explicitly needs it.
    Private,
}

/// When a claim was true, and when Loki knew it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validity {
    /// World time. When the claim started being true.
    pub valid_from: Date,
    /// World time. `None` means still true.
    pub valid_to: Option<Date>,
    /// System time. When Loki was told.
    pub learned: Date,
    /// System time. `None` means Loki still believes it.
    pub unlearned: Option<Date>,
}

impl Validity {
    /// A claim that is true from a date and still is.
    #[must_use]
    pub const fn open(valid_from: Date, learned: Date) -> Self {
        Self {
            valid_from,
            valid_to: None,
            learned,
            unlearned: None,
        }
    }

    /// Whether the claim was true in the world on a given day.
    ///
    /// Retrieval filters on world time, so a superseded claim cannot surface even from a live
    /// concept. History stays intact for the timeline.
    #[must_use]
    pub fn holds_on(&self, day: Date) -> bool {
        day >= self.valid_from && self.valid_to.is_none_or(|to| day < to)
    }

    /// Whether Loki still believes this.
    #[must_use]
    pub const fn is_believed(&self) -> bool {
        self.unlearned.is_none()
    }

    /// How long Loki believed something that had already stopped being true.
    ///
    /// This is what lets the timeline say "I was wrong about this for six weeks" rather than
    /// "this replaced that". Returns `None` when there was no such gap.
    #[must_use]
    pub fn wrong_for_days(&self) -> Option<i64> {
        let stopped = self.valid_to?;
        let noticed = self.unlearned?;
        let days = noticed.since(stopped).ok()?.get_days();
        (days > 0).then_some(i64::from(days))
    }
}

/// One statement about an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The statement, as a person would read it.
    pub text: String,
    /// What this claim is *about*: a short predicate such as `name`, `employer`, `city`.
    ///
    /// The key reconciliation turns on. Two claims conflict only when they describe the same
    /// attribute of the same entity, which is how Zep decides contradiction and what §9.5's
    /// `## Role` example was reaching for. Without it the only implementable test is comparing
    /// text, and that calls every second fact about a person a contradiction.
    ///
    /// Empty means unknown, which never conflicts with anything: a claim that cannot say what it
    /// is about has no standing to displace one that can.
    #[serde(default)]
    pub attribute: String,
    pub validity: Validity,
    pub confidence: Confidence,
    pub source: Source,
    #[serde(default)]
    pub privacy: Privacy,
    /// The claim that replaced this one, by text. Written when a claim is invalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    /// How often this has been retrieved and used without correction.
    #[serde(default)]
    pub usage_count: u32,
}

impl Claim {
    /// A claim the user just stated, true from today.
    #[must_use]
    pub fn stated(text: impl Into<String>, today: Date) -> Self {
        Self {
            text: text.into(),
            attribute: String::new(),
            validity: Validity::open(today, today),
            confidence: Confidence::High,
            source: Source::Stated,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
        }
    }

    /// A claim guessed from context. Everything import produces is this.
    #[must_use]
    pub fn inferred(text: impl Into<String>, valid_from: Date, learned: Date) -> Self {
        Self {
            text: text.into(),
            attribute: String::new(),
            validity: Validity::open(valid_from, learned),
            confidence: Confidence::Low,
            source: Source::Inferred,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
        }
    }

    /// Sets what this claim is about. Normalized, so `Employer` and `employer ` are one key.
    #[must_use]
    pub fn about(mut self, attribute: impl AsRef<str>) -> Self {
        self.attribute = normalize_attribute(attribute.as_ref());
        self
    }

    /// Whether two claims describe the same thing, and so cannot both be true (§9.7).
    ///
    /// An unknown attribute never collides. Getting this wrong in the permissive direction files
    /// unrelated facts as contradictions and takes the whole concept out of use.
    #[must_use]
    pub fn same_attribute_as(&self, other: &Self) -> bool {
        !self.attribute.is_empty() && self.attribute == other.attribute
    }

    /// Whether two claims assert the same thing in different words.
    ///
    /// Same property, same value, different phrasing. The extractor is a model and does not word a
    /// fact identically twice, so comparing text exactly made every re-run a fresh claim that then
    /// read as a correction of the last one. Nothing had changed, so nothing should be recorded.
    ///
    /// Conservative on purpose. Two wordings that do not reduce to the same words stay separate
    /// claims, because merging a real change into an old one loses it silently.
    #[must_use]
    pub fn restates(&self, other: &Self) -> bool {
        self.attribute == other.attribute
            && asserted_words(&self.text, &self.attribute)
                == asserted_words(&other.text, &other.attribute)
    }

    /// Folds in a restatement of this claim: the same fact, said again.
    ///
    /// The stored wording is kept, so a re-run does not churn the file or the git history. What
    /// can change is standing: a guess the user has since stated outright is no longer a guess
    /// (§9.7 rule 3). Not a use, so `usage_count` is untouched; that meter counts recall.
    pub fn reinforced_by(&mut self, other: &Self) {
        if self.source == Source::Inferred && other.source == Source::Stated {
            self.source = Source::Stated;
            self.confidence = Confidence::High;
        }
    }

    /// Whether this claim may be pre-fetched or put in the working set.
    #[must_use]
    pub fn is_eligible_for_prefetch(&self) -> bool {
        self.privacy == Privacy::Normal && self.validity.is_believed()
    }

    /// Marks this claim as no longer true, superseded by another.
    ///
    /// Nothing is deleted. The claim keeps its world-time window and gains a system-time end, so
    /// the timeline can still say what was believed and for how long.
    pub fn invalidate(&mut self, on: Date, stopped_being_true: Date, replacement: &str) {
        self.validity.valid_to = Some(stopped_being_true);
        self.validity.unlearned = Some(on);
        self.replaced_by = Some(replacement.to_owned());
    }

    /// A claim used without correction earns confidence.
    pub fn used_without_correction(&mut self) {
        self.usage_count = self.usage_count.saturating_add(1);
        self.confidence = match self.confidence {
            Confidence::Low if self.usage_count >= 2 => Confidence::Medium,
            Confidence::Medium if self.usage_count >= 5 => Confidence::High,
            other => other,
        };
    }

    /// A claim contradicted the moment it was used loses confidence and is flagged.
    pub fn contradicted(&mut self) {
        self.usage_count = 0;
        self.confidence = Confidence::Low;
    }
}

/// Words that assert nothing on their own, so swapping them does not change what a claim says.
///
/// `user` is in here because in a personal store the user is the one subject everything orbits:
/// "the user's name" and "Sabharish's name" are the same phrase with the same referent.
const FILLER: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "for", "from", "had", "has",
    "have", "he", "her", "hers", "him", "his", "i", "in", "into", "is", "it", "its", "me", "mine",
    "my", "of", "on", "or", "our", "ours", "s", "she", "that", "the", "their", "theirs", "them",
    "they", "this", "to", "us", "user", "users", "was", "were", "we", "with", "you", "your",
    "yours",
];

/// The words a claim actually asserts: lower-cased, sorted, deduplicated, with filler and the
/// attribute's own words removed.
///
/// Two claims about the same property of the same entity can only differ in the value they set,
/// so once the connective words are gone what is left is that value. Sorted and deduplicated
/// because "Sabharish's name is Sabharish" and "Name is Sabharish" carry the same word twice in
/// one and once in the other.
fn asserted_words(text: &str, attribute: &str) -> Vec<String> {
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .filter(|word| !FILLER.contains(&word.as_str()))
        .filter(|word| !attribute.split('_').any(|part| part == word))
        .collect();
    words.sort_unstable();
    words.dedup();
    words
}

/// Lowercases and trims an attribute key, so casing and spacing do not split one attribute in two.
#[must_use]
pub fn normalize_attribute(raw: &str) -> String {
    raw.trim().to_lowercase().replace([' ', '-'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// The worked example from section 9.5.
    fn job_change() -> (Claim, Claim) {
        let mut old = Claim {
            text: "Works on the platform team".into(),
            attribute: String::new(),
            validity: Validity::open(date(2026, 3, 12), date(2026, 3, 12)),
            confidence: Confidence::High,
            source: Source::Stated,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
        };
        old.invalidate(
            date(2026, 8, 29),
            date(2026, 7, 15),
            "Works on the infra team",
        );

        let new = Claim {
            text: "Works on the infra team".into(),
            attribute: String::new(),
            validity: Validity::open(date(2026, 7, 15), date(2026, 8, 29)),
            confidence: Confidence::High,
            source: Source::Stated,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
        };
        (old, new)
    }

    #[test]
    fn world_time_decides_what_was_true_when() {
        let (old, new) = job_change();
        // In May, the platform team was the truth.
        assert!(old.validity.holds_on(date(2026, 5, 1)));
        assert!(!new.validity.holds_on(date(2026, 5, 1)));
        // In August, infra is.
        assert!(!old.validity.holds_on(date(2026, 8, 1)));
        assert!(new.validity.holds_on(date(2026, 8, 1)));
    }

    #[test]
    fn the_boundary_day_belongs_to_the_new_claim() {
        let (old, new) = job_change();
        assert!(!old.validity.holds_on(date(2026, 7, 15)));
        assert!(new.validity.holds_on(date(2026, 7, 15)));
    }

    #[test]
    fn the_gap_between_true_and_known_is_recoverable() {
        let (old, _) = job_change();
        // Stopped being true 15 July, noticed 29 August. Six weeks of being wrong.
        assert_eq!(old.validity.wrong_for_days(), Some(45));
    }

    #[test]
    fn a_correction_with_no_gap_reports_none() {
        let mut claim = Claim::stated("Lives in Bangalore", date(2026, 1, 1));
        claim.invalidate(date(2026, 6, 1), date(2026, 6, 1), "Lives in Chennai");
        assert_eq!(claim.validity.wrong_for_days(), None);
    }

    #[test]
    fn an_invalidated_claim_is_no_longer_believed_but_is_not_gone() {
        let (old, _) = job_change();
        assert!(!old.validity.is_believed());
        assert_eq!(old.replaced_by.as_deref(), Some("Works on the infra team"));
        assert_eq!(old.text, "Works on the platform team");
    }

    #[test]
    fn private_claims_are_never_prefetched() {
        let mut claim = Claim::stated("Sees a therapist on Thursdays", date(2026, 1, 1));
        assert!(claim.is_eligible_for_prefetch());
        claim.privacy = Privacy::Private;
        assert!(!claim.is_eligible_for_prefetch());
    }

    #[test]
    fn an_invalidated_claim_is_never_prefetched() {
        let (old, _) = job_change();
        assert!(!old.is_eligible_for_prefetch());
    }

    #[test]
    fn confidence_climbs_with_use_and_collapses_on_contradiction() {
        let mut claim =
            Claim::inferred("Prefers short replies", date(2026, 1, 1), date(2026, 1, 1));
        assert_eq!(claim.confidence, Confidence::Low);

        claim.used_without_correction();
        claim.used_without_correction();
        assert_eq!(claim.confidence, Confidence::Medium);

        for _ in 0..3 {
            claim.used_without_correction();
        }
        assert_eq!(claim.confidence, Confidence::High);

        claim.contradicted();
        assert_eq!(claim.confidence, Confidence::Low);
        assert_eq!(claim.usage_count, 0);
    }

    /// B-27: the extractor worded one fact three ways across three runs.
    #[test]
    fn one_fact_worded_three_ways_is_one_fact() {
        let phrasings = [
            "Name is Sabharish",
            "Sabharish's name is Sabharish",
            "The user's name is Sabharish",
        ];
        let claims: Vec<Claim> = phrasings
            .iter()
            .map(|text| Claim::stated(*text, date(2026, 1, 1)).about("name"))
            .collect();
        for other in &claims[1..] {
            assert!(
                claims[0].restates(other),
                "{} vs {}",
                claims[0].text,
                other.text
            );
        }
    }

    #[test]
    fn a_different_value_is_not_a_restatement() {
        let chennai = Claim::stated("Sabharish lives in Chennai", date(2026, 1, 1)).about("city");
        let bangalore =
            Claim::stated("Sabharish lives in Bangalore", date(2026, 1, 1)).about("city");
        assert!(!chennai.restates(&bangalore));
    }

    /// Two properties can be worded alike without being the same fact.
    #[test]
    fn a_different_attribute_is_never_a_restatement() {
        let name = Claim::stated("Sabharish", date(2026, 1, 1)).about("name");
        let nickname = Claim::stated("Sabharish", date(2026, 1, 1)).about("nickname");
        assert!(!name.restates(&nickname));
    }

    /// Merging two wordings that share no vocabulary would lose a real change, so it is refused.
    #[test]
    fn unrelated_wordings_of_one_property_stay_separate() {
        let graduate = Claim::stated("Sabharish is a computer science graduate", date(2026, 1, 1))
            .about("education");
        let school = Claim::stated("Sabharish went to Chennai Public School", date(2026, 1, 1))
            .about("education");
        assert!(!graduate.restates(&school));
    }

    #[test]
    fn a_guess_the_user_states_outright_stops_being_a_guess() {
        let mut held = Claim::inferred(
            "Sabharish lives in Chennai",
            date(2026, 1, 1),
            date(2026, 1, 1),
        )
        .about("city");
        let said = Claim::stated("Sabharish lives in Chennai", date(2026, 6, 1)).about("city");
        held.reinforced_by(&said);
        assert_eq!(held.source, Source::Stated);
        assert_eq!(held.confidence, Confidence::High);
        assert_eq!(held.usage_count, 0, "a restatement is not a use");
    }

    #[test]
    fn stated_outranks_inferred() {
        let stated = Claim::stated("Works on infra", date(2026, 1, 1));
        let inferred = Claim::inferred("Works on platform", date(2026, 1, 1), date(2026, 1, 1));
        assert!(stated.source > inferred.source);
    }
}
