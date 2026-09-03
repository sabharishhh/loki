//! One statement inside a concept, with a bi-temporal validity window.
//!
//! Four timestamps, not one. World time is when a claim was true; system time is when Loki learned
//! it. A single timeline cannot express "on 29 August we found out the job change happened on
//! 15 July", which is the sentence the whole product is built to be able to say.

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Where a claim came from. Decides who wins a conflict, and what may ever become durable (§9.12).
///
/// Four values behind one eligibility function, shipped in v1 with one agent and one user because
/// of §6.2's asymmetry: adding `Peer { agent }` later is a variant and a row in
/// [`Origin::durable_eligible`], while introducing the concept after promotion, the gate and the
/// timeline are written is a restructure of the write path.
///
/// Not `Ord`. With two values an ordering meant "stated beats inferred"; with four it would have
/// to answer whether web beats inferred, which is not a question this enum should be able to
/// imply. Precedence asks the two predicates instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Loki derived it from what the user said. Everything from import is inferred (§11.4).
    Inferred,
    /// The user said it, in this app. Beats anything else regardless of age.
    Stated,
    /// Derived from fetched page content (§12).
    Web,
    /// Derived from an account's data (§15).
    Connector,
}

impl Origin {
    /// Whether a claim from here may ever become a durable fact about the user (§9.12).
    ///
    /// Principle 2 on the write path. Without this, a claim extracted from a fetched page can
    /// accumulate recurrence and promote into a fact about you with no user statement anywhere in
    /// its lineage, and nothing else in the design stops it: §9.7's rules only engage once
    /// something contradicts, and an uncontested false claim is never contradicted.
    #[must_use]
    pub const fn durable_eligible(self) -> bool {
        matches!(self, Self::Stated | Self::Inferred)
    }

    /// Whether the user said this in so many words.
    #[must_use]
    pub const fn is_stated(self) -> bool {
        matches!(self, Self::Stated)
    }
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
    /// World time. When the claim started being true, and `None` when the source never said.
    ///
    /// Optional on purpose (§9.5). v0.8 defaulted this to the day the claim was written, which
    /// made every pair the same vintage, so neither was clearly newer in world time and §9.7's
    /// rule 4 fired on almost every claim. "Undated" and "dated today" are different facts about
    /// a claim and the record now tells them apart.
    #[serde(default)]
    pub valid_from: Option<Date>,
    /// World time. `None` means still true.
    pub valid_to: Option<Date>,
    /// System time. When Loki was told.
    pub learned: Date,
    /// System time. `None` means Loki still believes it.
    pub unlearned: Option<Date>,
}

impl Validity {
    /// A claim Loki learned on a day, whose world time the source never gave.
    ///
    /// The common case. Most of what a person says carries no date.
    #[must_use]
    pub const fn undated(learned: Date) -> Self {
        Self {
            valid_from: None,
            valid_to: None,
            learned,
            unlearned: None,
        }
    }

    /// A claim that is true from a date the source gave, and still is.
    #[must_use]
    pub const fn open(valid_from: Date, learned: Date) -> Self {
        Self {
            valid_from: Some(valid_from),
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
        // No start date means true as far back as anything knows, which is the honest reading of
        // a source that never said when.
        self.valid_from.is_none_or(|from| day >= from) && self.valid_to.is_none_or(|to| day < to)
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
    /// What a single-valued attribute is set *to*, when the source made it plain (S-26).
    ///
    /// Cardinality decides that two `name` claims compete. Without knowing the value, competing
    /// means "differently worded", so "the user's father's name is Vaidyanathan" and
    /// "Vaidyanathan's official name is Vaidyanathan" were a contradiction rather than the same
    /// fact said twice, and the store surfaced a conflict a person could only read as nonsense.
    ///
    /// Optional, and absent means unknown. A claim with no value falls back to comparing text,
    /// which is what every file written before this does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub validity: Validity,
    pub confidence: Confidence,
    /// Where this came from, and so whether it may ever become durable (§9.12).
    ///
    /// Defaults to [`Origin::Inferred`] when a file does not say, which is the conservative
    /// direction: a claim of unknown provenance is treated as a guess, never as something the
    /// user said.
    #[serde(default = "inferred", alias = "source")]
    pub origin: Origin,
    #[serde(default)]
    pub privacy: Privacy,
    /// The claim that replaced this one, by text. Written when a claim is invalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    /// How often this has been retrieved and used without correction (§9.9).
    ///
    /// Distinct from the recall counters below. This one moves confidence and feeds §9.10's
    /// archival; those decide promotion. A claim can be recalled without being used well.
    #[serde(default)]
    pub usage_count: u32,
    /// What this claim rests on: zero or more anchors into `evidence/` (§9.13, §12.7).
    ///
    /// A list because one claim can rest on several mentions. A single source string cannot
    /// express a claim supported by three separate fetches. Empty until §12 fills it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    /// How often retrieval returned this claim (§10.6).
    #[serde(default)]
    pub recalls: u32,
    /// How many distinct days retrieval returned it on.
    ///
    /// Multi-day recurrence separates a fact that mattered from one that mattered once.
    #[serde(default)]
    pub recall_days: u32,
    /// How many distinct queries returned it.
    ///
    /// A claim that answers one question repeatedly is narrower than one that answers several.
    #[serde(default)]
    pub recall_queries: u32,
}

const fn inferred() -> Origin {
    Origin::Inferred
}

/// An anchor from a claim to the content it came from (§9.13, §12.7).
///
/// Split from the episode on purpose. The immutable fact is that a URL was fetched at a time and
/// yielded content with a given hash; the page itself is bulky and ages, so it is cached
/// separately in `evidence/` under its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Where the content came from. A URL for a fetch, a path for a file.
    pub source: String,
    /// Content address in `evidence/`. What keeps the claim checkable once the page has changed.
    pub hash: String,
    /// Which part of the content, when extraction located one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
}

impl Claim {
    /// A claim from `origin`, learned on a day, with no world time.
    ///
    /// No world time is the default because most of what a person says carries no date (§9.5).
    /// Add one with [`Claim::dated`] when the source actually gave one.
    #[must_use]
    pub fn new(text: impl Into<String>, origin: Origin, learned: Date) -> Self {
        Self {
            text: text.into(),
            attribute: String::new(),
            value: None,
            validity: Validity::undated(learned),
            confidence: if origin.is_stated() {
                Confidence::High
            } else {
                Confidence::Low
            },
            origin,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
            evidence: Vec::new(),
            recalls: 0,
            recall_days: 0,
            recall_queries: 0,
        }
    }

    /// A claim the user stated, learned today, with no world time.
    #[must_use]
    pub fn stated(text: impl Into<String>, learned: Date) -> Self {
        Self::new(text, Origin::Stated, learned)
    }

    /// A claim guessed from context. Everything import produces is this (§11.4).
    #[must_use]
    pub fn inferred(text: impl Into<String>, learned: Date) -> Self {
        Self::new(text, Origin::Inferred, learned)
    }

    /// Sets when this started being true. Only for a date the source actually gave (§9.5).
    #[must_use]
    pub const fn dated(mut self, valid_from: Date) -> Self {
        self.validity.valid_from = Some(valid_from);
        self
    }

    /// Attaches an anchor to what this claim rests on.
    #[must_use]
    pub fn citing(mut self, evidence: EvidenceRef) -> Self {
        self.evidence.push(evidence);
        self
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
        if self.attribute != other.attribute {
            return false;
        }
        // The value settles it when both carry one. Two sentences setting one property to one
        // value are one fact however differently they are worded, and comparing the words instead
        // is what turned a name said twice into a contradiction (S-26).
        if let (Some(mine), Some(theirs)) = (&self.value, &other.value) {
            return mine.eq_ignore_ascii_case(theirs);
        }
        asserted_words(&self.text, &self.attribute) == asserted_words(&other.text, &other.attribute)
    }

    /// Folds in a restatement of this claim: the same fact, said again.
    ///
    /// The stored wording is kept, so a re-run does not churn the file or the git history. What
    /// can change is standing: a guess the user has since stated outright is no longer a guess
    /// (§9.7 rule 3). Not a use, so `usage_count` is untouched; that meter counts recall.
    pub fn reinforced_by(&mut self, other: &Self) {
        if !self.origin.is_stated() && other.origin.is_stated() {
            self.origin = Origin::Stated;
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

/// Folds an attribute key to one canonical spelling, so casing, spacing and number do not split
/// one property into two.
///
/// Found in Sabharish's store: `interest` and `interests` sat as separate sections about the same
/// thing, so a later statement never superseded an earlier one and both stood as duplicates. The
/// extraction prompt asks for consistency and the model does not always give it, which is open
/// question 18's drift arriving in practice.
///
/// **This only has to be consistent, not linguistically correct.** `status` folds to `statu`,
/// which is not a word and does not matter: both sides of every comparison go through here, so a
/// wrong-looking stem still groups the same property together. That is why a crude rule is the
/// right one, and why it needs no dictionary.
#[must_use]
pub fn normalize_attribute(raw: &str) -> String {
    let key = raw.trim().to_lowercase().replace([' ', '-'], "_");
    let Some(stem) = key.strip_suffix('s') else {
        return key;
    };
    // `address` and `analysis` are not plurals. Their endings are, so the ending is what to check.
    if stem.len() < 3 || stem.ends_with('s') || stem.ends_with('u') || stem.ends_with('i') {
        return key;
    }
    if let Some(root) = stem.strip_suffix("ie") {
        return format!("{root}y");
    }
    stem.to_owned()
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
            value: None,
            validity: Validity::open(date(2026, 3, 12), date(2026, 3, 12)),
            confidence: Confidence::High,
            origin: Origin::Stated,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
            evidence: Vec::new(),
            recalls: 0,
            recall_days: 0,
            recall_queries: 0,
        };
        old.invalidate(
            date(2026, 8, 29),
            date(2026, 7, 15),
            "Works on the infra team",
        );

        let new = Claim {
            text: "Works on the infra team".into(),
            attribute: String::new(),
            value: None,
            validity: Validity::open(date(2026, 7, 15), date(2026, 8, 29)),
            confidence: Confidence::High,
            origin: Origin::Stated,
            privacy: Privacy::Normal,
            replaced_by: None,
            usage_count: 0,
            evidence: Vec::new(),
            recalls: 0,
            recall_days: 0,
            recall_queries: 0,
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
        let mut claim = Claim::inferred("Prefers short replies", date(2026, 1, 1));
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
        let mut held =
            Claim::inferred("Sabharish lives in Chennai", date(2026, 1, 1)).about("city");
        let said = Claim::stated("Sabharish lives in Chennai", date(2026, 6, 1)).about("city");
        held.reinforced_by(&said);
        assert_eq!(held.origin, Origin::Stated);
        assert_eq!(held.confidence, Confidence::High);
        assert_eq!(held.usage_count, 0, "a restatement is not a use");
    }

    /// The drift that produced duplicates in the live store: one property, two spellings.
    #[test]
    fn a_plural_and_a_singular_attribute_are_one_key() {
        assert_eq!(normalize_attribute("interests"), "interest");
        assert_eq!(normalize_attribute("Interest"), "interest");
        assert_eq!(normalize_attribute("hobbies"), "hobby");
        assert_eq!(normalize_attribute("reply style"), "reply_style");

        let one =
            Claim::stated("Sabharish is interested in AI", date(2026, 1, 1)).about("interest");
        let two =
            Claim::stated("Sabharish is interested in ML", date(2026, 1, 1)).about("interests");
        assert!(
            one.same_attribute_as(&two),
            "two spellings of one property have to compete, or neither ever supersedes the other"
        );
    }

    /// Endings that look plural and are not. The stem only has to be consistent, not a word.
    #[test]
    fn words_that_merely_end_in_s_are_left_alone() {
        assert_eq!(normalize_attribute("address"), "address");
        assert_eq!(normalize_attribute("status"), "status");
        assert_eq!(normalize_attribute("analysis"), "analysis");
    }

    #[test]
    fn only_stated_and_inferred_may_become_durable() {
        assert!(Origin::Stated.durable_eligible());
        assert!(Origin::Inferred.durable_eligible());
        assert!(!Origin::Web.durable_eligible());
        assert!(!Origin::Connector.durable_eligible());
    }

    /// The default is the conservative one: unknown provenance is a guess, never a statement.
    #[test]
    fn a_claim_that_does_not_say_where_it_came_from_is_a_guess() {
        let parsed: Claim = serde_json::from_str(
            r#"{"text":"Lives in Chennai","validity":{"valid_to":null,
               "learned":"2026-01-01","unlearned":null},"confidence":"medium"}"#,
        )
        .expect("a v0.8-shaped claim still parses");
        assert_eq!(parsed.origin, Origin::Inferred);
        assert_eq!(parsed.validity.valid_from, None);
        assert!(parsed.evidence.is_empty());
    }

    /// A claim the source never dated holds on any day, which is what "we do not know when" means.
    #[test]
    fn an_undated_claim_holds_on_any_day() {
        let claim = Claim::stated("Sabharish is a computer science graduate", date(2026, 1, 1));
        assert_eq!(claim.validity.valid_from, None);
        assert!(claim.validity.holds_on(date(2020, 1, 1)));
        assert!(claim.validity.holds_on(date(2030, 1, 1)));
    }
}
