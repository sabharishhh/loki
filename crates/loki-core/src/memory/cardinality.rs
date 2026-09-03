//! Whether a property holds one value or many (S-22).
//!
//! The question every "I have a degree" / "I am now certified" pair asks. Databases have called it
//! cardinality for forty years and OWL calls it a functional property; there is no reason to
//! invent a third answer.
//!
//! **The default is many, and that is the whole design.** §21.2 names wrongly retiring a true
//! claim as the more damaging error, so a property nobody listed accumulates rather than
//! replacing. A short list of genuinely single-valued keys carries the supersession that §9.7
//! needs, and everything outside it is additive.
//!
//! Two lists, because an attribute and a relation are different things that happen to share the
//! word. `employer` as an attribute is many-valued, because a consultant has two; `employer` as a
//! relation is single-valued, because the edge means the current one.

/// Attributes where a later claim replaces an earlier one.
///
/// Deliberately short and deliberately boring. Every entry holds exactly one value at a time by
/// the nature of the thing, not by how often it changes: a person lives in one city, a release
/// ships on one date.
///
/// **The list grows with evidence, never with a hunch.** Adding a key here means a later claim can
/// retire an earlier one, which §21.2 names as the more damaging error, so each entry should come
/// with the case that showed it was needed. `deadline` is here because "ships on the 30th, no the
/// 20th, actually the 30th" otherwise leaves two live answers, which is the PrefEval failure §9.5
/// exists to prevent. `employer` is deliberately absent, because a consultant has two.
const SINGLE_VALUED_ATTRIBUTES: [&str; 8] = [
    "name",
    "birthday",
    "city",
    "timezone",
    "pronouns",
    "age",
    "preferred_name",
    "deadline",
];

/// Relation labels where one live edge is the whole truth.
const SINGLE_VALUED_RELATIONS: [&str; 5] = ["manager", "spouse", "mother", "father", "employer"];

/// Whether a later claim on this attribute supersedes an earlier one.
///
/// Called with an already-normalized key (see [`super::claim::normalize_attribute`]), so the
/// plural fold and the case fold have both happened.
#[must_use]
pub fn attribute_is_single_valued(attribute: &str) -> bool {
    SINGLE_VALUED_ATTRIBUTES.contains(&attribute)
}

/// Whether a new edge with this label closes the one before it.
#[must_use]
pub fn relation_is_single_valued(label: &str) -> bool {
    SINGLE_VALUED_RELATIONS.contains(&label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair that motivated the whole thing: same shape, opposite right answer.
    #[test]
    fn a_degree_accumulates_and_a_city_replaces() {
        assert!(!attribute_is_single_valued("education"));
        assert!(attribute_is_single_valued("city"));
    }

    /// A consultant with two clients is not a contradiction, and the edge to the current one is.
    #[test]
    fn employer_is_many_as_an_attribute_and_one_as_a_relation() {
        assert!(!attribute_is_single_valued("employer"));
        assert!(relation_is_single_valued("employer"));
    }

    /// Two values that cannot both be true are not two facts. Found by `other_people.rs`, which
    /// says a person changing their mind mid-sentence must be left with one answer.
    #[test]
    fn a_ship_date_is_one_date() {
        assert!(attribute_is_single_valued("deadline"));
    }

    /// Anything nobody listed accumulates. This is the direction §21.2 asks for, and it is what
    /// makes the list safe to keep short.
    #[test]
    fn an_unlisted_property_accumulates() {
        for attribute in ["interest", "allergy", "project", "trait", "", "whatever"] {
            assert!(
                !attribute_is_single_valued(attribute),
                "{attribute} should accumulate"
            );
        }
        for label in ["brother", "sister", "friend", "colleague", ""] {
            assert!(!relation_is_single_valued(label), "{label} should be many");
        }
    }
}
