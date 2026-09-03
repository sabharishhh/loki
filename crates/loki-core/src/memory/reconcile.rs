//! Conflict precedence and promotion. The rules that decide what memory believes.
//!
//! Pure functions over claims, deliberately. These are the decisions §21.2 scores, so they have
//! to be inspectable and testable without a model call anywhere near them.

use jiff::civil::Date;

use super::claim::{Claim, Confidence};
use super::concept::{RawConcept, Status};

/// What to do with a new claim that conflicts with one already held (§9.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precedence {
    /// The incoming claim wins. Invalidate the held one and record the replacement.
    Replace,
    /// The held claim wins. Drop the incoming one.
    Keep,
    /// Two `stated` claims, neither clearly newer. Do not guess: mark both `draft` and surface it.
    Surface,
}

/// Applies §9.7's four rules, in order.
///
/// Rule 1, an explicit user statement beating everything, is the caller's to signal through
/// `incoming_is_explicit`: only the caller knows whether the user just said this in so many words,
/// as opposed to it having been extracted from an episode.
#[must_use]
pub fn precedence(held: &Claim, incoming: &Claim, incoming_is_explicit: bool) -> Precedence {
    // 1. An explicit user statement beats everything.
    if incoming_is_explicit {
        return Precedence::Replace;
    }

    // 3. A stated claim beats one that is not, regardless of age. Checked before rule 2, because
    //    rule 2 only governs when the two are of equal standing.
    match (held.origin.is_stated(), incoming.origin.is_stated()) {
        (false, true) => return Precedence::Replace,
        (true, false) => return Precedence::Keep,
        _ => {}
    }

    // 3b. §9.12 on the write path: content that did not come from the user never displaces
    //     content that did. Rule 3 covers stated against inferred; this covers inferred against
    //     web or connector, which v0.8 had no way to express.
    if held.origin.durable_eligible() && !incoming.origin.durable_eligible() {
        return Precedence::Keep;
    }

    // 2. Otherwise a more recent statement beats an older one. World time first, because when the
    //    fact started being true is what orders it (§9.5): an import learned today can describe
    //    something true years ago.
    //
    //    Only when both carry one. A claim with no world time is ordered by system time, which is
    //    §9.5's stated fallback and is why the field is optional: "undated" and "dated today" are
    //    different, and treating them alike fired rule 4 on almost every pair.
    if let (Some(mine), Some(theirs)) = (incoming.validity.valid_from, held.validity.valid_from)
        && mine != theirs
    {
        return if mine > theirs {
            Precedence::Replace
        } else {
            Precedence::Keep
        };
    }

    match incoming.validity.learned.cmp(&held.validity.learned) {
        std::cmp::Ordering::Greater => Precedence::Replace,
        std::cmp::Ordering::Less => Precedence::Keep,
        // 4. Told in the same breath, about the same thing, and neither is newer.
        //    Guessing is how a memory system poisons itself.
        std::cmp::Ordering::Equal => {
            if held.origin.is_stated() {
                Precedence::Surface
            } else {
                // Two guesses of one vintage. Neither has standing to displace the other, and
                // surfacing every import collision would bury the user.
                Precedence::Keep
            }
        }
    }
}

/// Whether a draft claim has earned `stable` (§9.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotion {
    /// Recurring, low stakes, no conflict.
    Auto,
    /// First mention. Promotes on a second occurrence, or on use without correction.
    Hold,
    /// A conflict on a durable claim, or a `private` tier claim. One tap.
    Ask,
}

/// Decides whether a claim promotes, waits, or needs the user.
///
/// `occurrences` counts how many times this claim has been seen, including now.
///
/// **A `stated` claim promotes on its first mention.** Sabharish's call, and it closes a hole in
/// §9.8: that table says a first mention "promotes on a second occurrence, or on use without
/// correction", but a draft is never retrieved, so it can never be used, so the second path was
/// unreachable by construction. Told once, a fact stayed invisible for ever.
///
/// `inferred` still waits, which is what the draft tier is actually for: stopping a guess becoming
/// a fact about you. Import is unaffected, because §11.4 makes everything it writes inferred.
#[must_use]
pub fn promotion(claim: &Claim, conflicted: bool) -> Promotion {
    use super::claim::Privacy;

    if conflicted || claim.privacy == Privacy::Private {
        return Promotion::Ask;
    }
    // §9.12: content that did not come from the user never auto-promotes. It stays stored,
    // indexed and searchable, and reaches a later prompt only through a deliberate search or a
    // confirmation. Checked before anything else, because recurrence is exactly the signal a
    // fetched page would otherwise accumulate.
    if !claim.origin.durable_eligible() {
        return Promotion::Ask;
    }
    if claim.origin.is_stated() || earned_by_recall(claim) {
        return Promotion::Auto;
    }
    Promotion::Hold
}

/// Distinct questions a claim must have answered before it is trusted (§9.8, §26 question 16).
///
/// Breadth, not volume. A claim that answers one question repeatedly is narrower than one that
/// answers several, and thirty hits on a single query is one fact being useful once.
pub const PROMOTE_AT_QUERIES: u32 = 3;

/// Distinct days it must have answered them across.
///
/// Multi-day recurrence separates a fact that mattered from one that mattered for an afternoon.
/// Two is the smallest number that can tell those apart at all.
pub const PROMOTE_AT_DAYS: u32 = 2;

/// Whether recall behaviour has earned an inferred claim its place (§9.8, §10.6).
///
/// **Counted, never judged.** §22.4 rejects putting a model in the promotion path, because this is
/// the one decision that shapes the store and it has to stay auditable.
///
/// v0.8 promoted on a second occurrence. That rewards an extractor that repeats itself and says
/// nothing about whether the fact matters. A claim that answered three different questions on
/// three different days is evidence of usefulness; a claim written twice in one session is
/// evidence of nothing.
///
/// The numbers are open question 16 and cannot be picked properly before §21.3 reports a promotion
/// rate. Two named constants, so this stays a number to change rather than a rule to rewrite.
#[must_use]
pub fn earned_by_recall(claim: &Claim) -> bool {
    claim.recall_queries >= PROMOTE_AT_QUERIES && claim.recall_days >= PROMOTE_AT_DAYS
}

/// Whether a later claim about the same attribute overrides this one (§9.7 rule 4).
///
/// The one predicate deciding what a conflict costs. Rule 4 is the only path that leaves two
/// believed claims on one attribute, and this says the newer of them is the one Loki uses.
///
/// **Per claim, not per concept.** Two claims that cannot both be true settle between themselves
/// and take nothing else with them. Scoping this to the concept is how a disagreement about a
/// degree hid a person's name: the store held a correct, stated, high-confidence name and answered
/// that it did not know it.
///
/// **Shadowed, not retired.** The older claim keeps its window and stays in the file, so §21.2's
/// "true claims wrongly invalidated" stays at zero and the interface can offer it back. It simply
/// never reaches a prompt, which is all PrefEval's finding actually requires.
///
/// Later means later in the file. Claims are appended, so file order is the order they arrived,
/// and rule 4 fires precisely when no date can separate them.
#[must_use]
pub fn is_shadowed(concept: &RawConcept, ordinal: u32) -> bool {
    let Some(claim) = concept.claims().nth(ordinal as usize) else {
        return false;
    };
    if !claim.validity.is_believed() || claim.attribute.is_empty() {
        return false;
    }
    // Only a single-valued attribute can be overridden (S-22). On a many-valued one the two claims
    // are both true and both belong in the prompt: the second brother, the second client, the
    // certificate on top of the degree.
    if !super::cardinality::attribute_is_single_valued(&claim.attribute) {
        return false;
    }
    concept
        .claims()
        .skip(ordinal as usize + 1)
        .filter(|later| later.validity.is_believed())
        .any(|later| later.same_attribute_as(claim))
}

/// Whether a concept has stopped mattering and should be archived (§9.10).
///
/// Nothing is deleted by heuristic. `deprecated` stays linkable and searchable.
#[must_use]
pub fn should_archive(concept: &RawConcept, today: Date, unused_days: i64) -> bool {
    if concept.front.status != Status::Stable {
        return false;
    }
    // Human-verified concepts are exempt, permanently. A person looked at this and said yes.
    if concept.front.is_human_verified() {
        return false;
    }
    // Age alone is not disuse. A fact used last week is not stale because it is old.
    let used = concept.claims().any(|c| c.usage_count > 0);
    if used {
        return false;
    }
    let age = today
        .since(concept.front.generated.at)
        .map_or(0, |s| s.get_days().into());
    age >= unused_days
}

/// The reference a relative time resolves against (§9.6).
///
/// "Two weeks ago" is not storable, and the reference differs by where the claim came from.
/// Getting this wrong silently corrupts every imported claim by however old the export is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// A live session. Relative time resolves against today.
    Live(Date),
    /// Import. Resolves against the timestamp of the message the claim came from.
    Message(Date),
    /// Web evidence. The page's published date if it has one, else the fetch time.
    Page(Date),
}

impl Reference {
    /// The day a relative expression counts back from.
    #[must_use]
    pub const fn anchor(self) -> Date {
        match self {
            Self::Live(day) | Self::Message(day) | Self::Page(day) => day,
        }
    }

    /// Resolves an offset in days before the anchor into an absolute date.
    #[must_use]
    pub fn resolve(self, days_ago: i64) -> Date {
        self.anchor()
            .checked_sub(jiff::Span::new().days(days_ago))
            .unwrap_or_else(|_| self.anchor())
    }
}

/// One reconcile decision, kept so §21.2 can score over-supersession.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub concept: String,
    pub held: String,
    pub incoming: String,
    pub outcome: Precedence,
}

/// Applies a decision to a concept in place, returning what happened.
///
/// `Replace` invalidates the held claim on `today` and records what replaced it, which is what
/// makes §17.3's "I was wrong about it for six weeks" sentence writable at all.
pub fn apply(
    concept: &mut RawConcept,
    heading: &str,
    held_text: &str,
    incoming: Claim,
    outcome: Precedence,
    today: Date,
) {
    match outcome {
        Precedence::Replace => {
            // When the new claim carries no world time, the old one stopped being true the day we
            // found out. That is the system-time fallback again, and it is honest: `wrong_for_days`
            // then reports no gap, because there is no period we can name having been wrong for.
            let stopped = incoming.validity.valid_from.unwrap_or(today);
            let replacement = incoming.text.clone();
            for claim in concept.claims_mut() {
                if claim.text == held_text {
                    claim.invalidate(today, stopped, &replacement);
                }
            }
            concept.add(heading, incoming);
        }
        Precedence::Keep => {}
        Precedence::Surface => {
            // Neither is used, so both drop to draft and the concept stops being prompt-eligible
            // until a person resolves it.
            concept.front.status = Status::Draft;
            for claim in concept.claims_mut() {
                if claim.text == held_text {
                    claim.confidence = Confidence::Low;
                }
            }
            let mut held_back = incoming;
            held_back.confidence = Confidence::Low;
            concept.add(heading, held_back);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::concept::{Attribution, Frontmatter};
    use super::*;
    use jiff::civil::date;

    fn stated(text: &str, from: Date) -> Claim {
        Claim::stated(text, from)
    }

    fn inferred(text: &str, from: Date, learned: Date) -> Claim {
        Claim::inferred(text, learned).dated(from)
    }

    #[test]
    fn rule_one_an_explicit_statement_beats_everything() {
        let held = stated("on the platform team", date(2026, 8, 1));
        let old = stated("on the infra team", date(2026, 1, 1));
        // Older in world time, and it still wins, because the user just said it.
        assert_eq!(precedence(&held, &old, true), Precedence::Replace);
    }

    #[test]
    fn rule_two_the_newer_statement_wins_when_both_are_stated() {
        let held = stated("in Chennai", date(2026, 1, 1));
        let newer = stated("in Bangalore", date(2026, 7, 1));
        assert_eq!(precedence(&held, &newer, false), Precedence::Replace);
        assert_eq!(precedence(&newer, &held, false), Precedence::Keep);
    }

    #[test]
    fn rule_three_stated_beats_inferred_regardless_of_age() {
        let held = inferred("in Chennai", date(2026, 8, 1), date(2026, 8, 1));
        let older = stated("in Bangalore", date(2026, 1, 1));
        // The stated claim is six months older in world time and still wins.
        assert_eq!(precedence(&held, &older, false), Precedence::Replace);
    }

    #[test]
    fn rule_three_holds_the_other_way_too() {
        let held = stated("in Bangalore", date(2026, 1, 1));
        let newer = inferred("in Chennai", date(2026, 8, 1), date(2026, 8, 1));
        assert_eq!(precedence(&held, &newer, false), Precedence::Keep);
    }

    /// The rule that matters. Guessing is how a memory system poisons itself.
    #[test]
    fn rule_four_two_stated_claims_of_the_same_vintage_surface() {
        let held = stated("works at Acme", date(2026, 3, 1));
        let other = stated("works at Globex", date(2026, 3, 1));
        assert_eq!(precedence(&held, &other, false), Precedence::Surface);
    }

    /// §9.7's stated consequence: on day one everything is inferred, so rule 3 never fires and
    /// rule 2 governs. Correct, and it looks like a bug if nobody wrote it down.
    #[test]
    fn before_the_user_says_anything_rule_two_governs() {
        let held = inferred("in Chennai", date(2026, 1, 1), date(2026, 8, 1));
        let newer = inferred("in Bangalore", date(2026, 7, 1), date(2026, 8, 1));
        assert_eq!(precedence(&held, &newer, false), Precedence::Replace);
    }

    #[test]
    fn two_inferred_claims_of_one_vintage_do_not_surface() {
        let held = inferred("a", date(2026, 3, 1), date(2026, 8, 1));
        let other = inferred("b", date(2026, 3, 1), date(2026, 8, 1));
        // Surfacing every import collision would bury the user on first run.
        assert_eq!(precedence(&held, &other, false), Precedence::Keep);
    }

    /// The hole this closes: a draft is never retrieved, so "promotes on use" could never fire.
    #[test]
    fn what_the_user_says_is_usable_at_once() {
        let claim = stated("prefers short replies", date(2026, 8, 1));
        assert_eq!(promotion(&claim, false), Promotion::Auto);
    }

    #[test]
    fn an_inferred_first_mention_still_waits() {
        let claim = inferred("prefers short replies", date(2026, 8, 1), date(2026, 8, 1));
        assert_eq!(promotion(&claim, false), Promotion::Hold);
    }

    /// §9.8: an inferred claim earns its place on recall behaviour, not on being written twice.
    #[test]
    fn a_guess_that_answered_several_questions_across_days_is_promoted() {
        let mut claim = inferred("prefers short replies", date(2026, 8, 1), date(2026, 8, 1));
        claim.recall_queries = PROMOTE_AT_QUERIES;
        claim.recall_days = PROMOTE_AT_DAYS;
        assert_eq!(promotion(&claim, false), Promotion::Auto);
    }

    /// Volume is not breadth. Thirty hits on one question in one afternoon is one fact being
    /// useful once, which is precisely what "seen twice" used to reward.
    #[test]
    fn a_guess_recalled_often_for_one_question_is_not_promoted() {
        let mut claim = inferred("prefers short replies", date(2026, 8, 1), date(2026, 8, 1));
        claim.recalls = 30;
        claim.recall_queries = 1;
        claim.recall_days = 1;
        assert_eq!(promotion(&claim, false), Promotion::Hold);
    }

    /// Several questions, but all on one day. Still a busy afternoon, not a fact that matters.
    #[test]
    fn breadth_without_recurrence_is_not_enough() {
        let mut claim = inferred("prefers short replies", date(2026, 8, 1), date(2026, 8, 1));
        claim.recall_queries = PROMOTE_AT_QUERIES + 2;
        claim.recall_days = 1;
        assert_eq!(promotion(&claim, false), Promotion::Hold);
    }

    /// Saying it does not override the two cases that need a person.
    #[test]
    fn a_stated_claim_that_conflicts_still_asks() {
        let claim = stated("earns x", date(2026, 8, 1));
        assert_eq!(promotion(&claim, true), Promotion::Ask);
    }

    #[test]
    fn a_conflict_or_a_private_claim_asks() {
        let claim = stated("earns x", date(2026, 8, 1));
        assert_eq!(promotion(&claim, true), Promotion::Ask);

        let mut private = stated("earns x", date(2026, 8, 1));
        private.privacy = super::super::claim::Privacy::Private;
        assert_eq!(promotion(&private, false), Promotion::Ask);
    }

    #[test]
    fn relative_time_resolves_against_the_right_reference() {
        // The same "two weeks ago" from an export a year old must not land on today.
        let live = Reference::Live(date(2026, 9, 1));
        let import = Reference::Message(date(2025, 9, 1));
        assert_eq!(live.resolve(14), date(2026, 8, 18));
        assert_eq!(import.resolve(14), date(2025, 8, 18));
    }

    fn stable(name: &str, at: Date) -> RawConcept {
        let mut front = Frontmatter::new(name, at);
        front.status = Status::Stable;
        RawConcept::new(front)
    }

    #[test]
    fn an_unused_old_concept_archives() {
        let concept = stable("Old thing", date(2026, 1, 1));
        assert!(should_archive(&concept, date(2026, 9, 1), 180));
        assert!(!should_archive(&concept, date(2026, 2, 1), 180));
    }

    #[test]
    fn a_used_concept_never_archives_on_age_alone() {
        let mut concept = stable("Used thing", date(2026, 1, 1));
        let mut claim = stated("something", date(2026, 1, 1));
        claim.used_without_correction();
        concept.add("Notes", claim);
        assert!(!should_archive(&concept, date(2030, 1, 1), 180));
    }

    #[test]
    fn a_human_verified_concept_is_exempt() {
        let mut concept = stable("Verified thing", date(2026, 1, 1));
        concept.front.verified.push(Attribution {
            by: "human:sabharishhh".to_string(),
            at: date(2026, 1, 2),
        });
        assert!(!should_archive(&concept, date(2030, 1, 1), 180));
    }

    #[test]
    fn a_draft_concept_is_not_archived() {
        let mut concept = RawConcept::new(Frontmatter::new("Draft thing", date(2026, 1, 1)));
        concept.front.status = Status::Draft;
        assert!(!should_archive(&concept, date(2030, 1, 1), 180));
    }

    #[test]
    fn replacing_records_what_superseded_what() {
        let mut concept = RawConcept::new(Frontmatter::new("Sabharish", date(2026, 1, 1)));
        concept.add("Location", stated("in Chennai", date(2026, 1, 1)));
        // Told on 29 August about a move that happened on 15 July, so the incoming claim carries
        // a world time. Without one the old claim would stop being true the day we found out,
        // and there would be no gap to report.
        let incoming = Claim::stated("in Bangalore", date(2026, 8, 29)).dated(date(2026, 7, 15));

        apply(
            &mut concept,
            "Location",
            "in Chennai",
            incoming,
            Precedence::Replace,
            date(2026, 8, 29),
        );

        let old = concept
            .claims()
            .find(|c| c.text == "in Chennai")
            .expect("held claim");
        assert!(
            !old.validity.is_believed(),
            "the old claim is still believed"
        );
        assert_eq!(old.replaced_by.as_deref(), Some("in Bangalore"));
        // §9.5's worked example: told on 29 August about a move on 15 July.
        assert_eq!(old.validity.wrong_for_days(), Some(45));
    }

    #[test]
    fn surfacing_drops_the_concept_out_of_the_prompt() {
        let mut concept = stable("Meera", date(2026, 1, 1));
        concept.add("Work", stated("at Acme", date(2026, 3, 1)));

        apply(
            &mut concept,
            "Work",
            "at Acme",
            stated("at Globex", date(2026, 3, 1)),
            Precedence::Surface,
            date(2026, 9, 1),
        );

        assert_eq!(concept.front.status, Status::Draft);
        assert_eq!(concept.claims().count(), 2);
        assert!(concept.claims().all(|c| c.confidence == Confidence::Low));
    }
}
