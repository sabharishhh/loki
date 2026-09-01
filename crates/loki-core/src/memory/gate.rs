//! The only path from a file to a prompt.
//!
//! Typestate is a compile-time pattern and a `status` field read off disk is data the compiler
//! cannot check. So parse rather than validate: a [`RawConcept`] is what is on disk, an [`Active`]
//! is what has been checked, and only an `Active` can be built into a prompt.
//!
//! The point is that no code path exists to skip this. Not a rule anyone has to remember.

use jiff::civil::Date;

use super::claim::{Claim, Privacy};
use super::concept::{RawConcept, Status};
use crate::core::vocab::Locality;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    #[error("not stable")]
    NotStable,
    #[error("past its stale_after")]
    Stale,
}

/// Which claims a request may carry, decided by where the model runs.
///
/// A capability, not a setting. `private` claims are eligible only for a request that a task
/// explicitly needs them for, and a future `secret` tier will never reach a cloud provider at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierScope {
    locality: Locality,
    /// Whether this particular task asked for private claims.
    private_allowed: bool,
}

impl TierScope {
    /// The default. Normal claims only, which is what pre-fetch and the working set get.
    #[must_use]
    pub const fn normal(locality: Locality) -> Self {
        Self {
            locality,
            private_allowed: false,
        }
    }

    /// A task that explicitly needs private claims.
    ///
    /// They still transit to the provider when used. Local-first means the store is yours and you
    /// control what is eligible to be sent, not that nothing leaves.
    #[must_use]
    pub const fn including_private(locality: Locality) -> Self {
        Self {
            locality,
            private_allowed: true,
        }
    }

    #[must_use]
    pub const fn locality(self) -> Locality {
        self.locality
    }

    #[must_use]
    pub const fn admits(self, privacy: Privacy) -> bool {
        match privacy {
            Privacy::Normal => true,
            Privacy::Private => self.private_allowed,
        }
    }
}

/// A concept that has passed the gate.
///
/// Constructing one is the only way to get concept text into a prompt, and the only constructor
/// is [`Active::try_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active(RawConcept);

impl Active {
    /// Checks a concept read off disk.
    ///
    /// # Errors
    /// Rejects anything not `stable`, and anything past its `stale_after`.
    pub fn try_from(raw: RawConcept, today: Date) -> Result<Self, GateError> {
        if raw.front.status != Status::Stable {
            return Err(GateError::NotStable);
        }
        if raw.is_stale_on(today) {
            return Err(GateError::Stale);
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub const fn concept(&self) -> &RawConcept {
        &self.0
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.front.name
    }

    /// The claims this scope may see, on a given day.
    ///
    /// Filters on world time as well as status, so an invalidated claim cannot surface even from a
    /// live concept.
    pub fn visible_claims(&self, scope: TierScope, today: Date) -> impl Iterator<Item = &Claim> {
        self.0
            .claims()
            .filter(move |c| scope.admits(c.privacy))
            .filter(move |c| c.validity.is_believed())
            .filter(move |c| c.validity.holds_on(today))
    }
}

/// Builds the text a prompt carries for a set of concepts.
///
/// Takes `Active`, not `RawConcept`, so an unchecked concept cannot be passed by mistake. That is
/// the whole design: the signature refuses rather than a reviewer noticing.
#[must_use]
pub fn build_prompt_text(concepts: &[Active], scope: TierScope, today: Date) -> String {
    let mut out = String::new();
    for concept in concepts {
        let claims: Vec<&Claim> = concept.visible_claims(scope, today).collect();
        if claims.is_empty() {
            continue;
        }
        out.push_str("## ");
        out.push_str(concept.name());
        out.push('\n');
        for claim in claims {
            out.push_str("- ");
            out.push_str(&claim.text);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::claim::Claim;
    use crate::memory::concept::Frontmatter;
    use jiff::civil::date;

    fn today() -> Date {
        date(2026, 8, 1)
    }

    fn stable(name: &str) -> RawConcept {
        let mut front = Frontmatter::new(name, date(2026, 1, 1));
        front.status = Status::Stable;
        RawConcept::new(front)
    }

    #[test]
    fn a_draft_concept_cannot_reach_a_prompt() {
        let draft = RawConcept::new(Frontmatter::new("Meera", date(2026, 1, 1)));
        assert_eq!(draft.front.status, Status::Draft);
        assert_eq!(Active::try_from(draft, today()), Err(GateError::NotStable));
    }

    #[test]
    fn a_deprecated_concept_cannot_reach_a_prompt() {
        let mut concept = stable("Old project");
        concept.front.status = Status::Deprecated;
        assert_eq!(
            Active::try_from(concept, today()),
            Err(GateError::NotStable)
        );
    }

    #[test]
    fn a_stale_concept_cannot_reach_a_prompt() {
        let mut concept = stable("Trip to Tokyo");
        concept.front.stale_after = Some(date(2026, 7, 1));
        assert_eq!(Active::try_from(concept, today()), Err(GateError::Stale));
    }

    #[test]
    fn a_stable_unexpired_concept_passes() {
        assert!(Active::try_from(stable("Meera"), today()).is_ok());
    }

    #[test]
    fn private_claims_are_withheld_by_default() {
        let mut concept = stable("Sabharish");
        concept.add("Work", Claim::stated("Builds Loki", date(2026, 1, 1)));
        let mut secret = Claim::stated("Sees a therapist on Thursdays", date(2026, 1, 1));
        secret.privacy = Privacy::Private;
        concept.add("Health", secret);

        let active = Active::try_from(concept, today()).expect("gate");
        let normal: Vec<_> = active
            .visible_claims(TierScope::normal(Locality::Cloud), today())
            .collect();
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0].text, "Builds Loki");
    }

    #[test]
    fn a_task_that_needs_private_claims_can_have_them() {
        let mut concept = stable("Sabharish");
        let mut secret = Claim::stated("Sees a therapist on Thursdays", date(2026, 1, 1));
        secret.privacy = Privacy::Private;
        concept.add("Health", secret);

        let active = Active::try_from(concept, today()).expect("gate");
        let seen: Vec<_> = active
            .visible_claims(TierScope::including_private(Locality::Cloud), today())
            .collect();
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn an_invalidated_claim_cannot_surface_from_a_live_concept() {
        let mut concept = stable("Meera");
        let mut old = Claim::stated("Works on the platform team", date(2026, 3, 12));
        old.invalidate(
            date(2026, 7, 20),
            date(2026, 7, 15),
            "Works on the infra team",
        );
        concept.add("Role", old);
        concept.add(
            "Role",
            Claim::stated("Works on the infra team", date(2026, 7, 15)),
        );

        let active = Active::try_from(concept, today()).expect("gate");
        let seen: Vec<_> = active
            .visible_claims(TierScope::normal(Locality::Cloud), today())
            .collect();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text, "Works on the infra team");
    }

    #[test]
    fn a_claim_not_yet_true_does_not_surface() {
        let mut concept = stable("Sabharish");
        concept.add(
            "Plans",
            Claim::stated("Moves to Chennai", date(2026, 12, 1)),
        );

        let active = Active::try_from(concept, today()).expect("gate");
        assert_eq!(
            active
                .visible_claims(TierScope::normal(Locality::Cloud), today())
                .count(),
            0
        );
    }

    #[test]
    fn prompt_text_carries_only_what_the_scope_admits() {
        let mut concept = stable("Sabharish");
        concept.add("Work", Claim::stated("Builds Loki", date(2026, 1, 1)));
        let mut secret = Claim::stated("Private thing", date(2026, 1, 1));
        secret.privacy = Privacy::Private;
        concept.add("Health", secret);

        let active = vec![Active::try_from(concept, today()).expect("gate")];
        let text = build_prompt_text(&active, TierScope::normal(Locality::Cloud), today());

        assert!(text.contains("Builds Loki"));
        assert!(!text.contains("Private thing"));
    }

    #[test]
    fn a_concept_with_nothing_visible_contributes_nothing() {
        let concept = stable("Empty");
        let active = vec![Active::try_from(concept, today()).expect("gate")];
        let text = build_prompt_text(&active, TierScope::normal(Locality::Cloud), today());
        assert!(text.is_empty());
    }
}
