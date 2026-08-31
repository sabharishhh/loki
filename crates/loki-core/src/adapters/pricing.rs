//! Published per-model rates.
//!
//! Cents per million tokens. Cached from each provider's pricing page, so it drifts. An unknown
//! model returns `None` and the caller records [`CostModel::Free`], which under-reports rather
//! than inventing a number. A wrong figure in the ledger is worse than a missing one, because a
//! missing one is visible.

use crate::core::vocab::{Cents, CostModel};

const fn per_token(input: u64, output: u64) -> CostModel {
    CostModel::PerToken {
        input_per_mtok: Cents::new(input),
        output_per_mtok: Cents::new(output),
    }
}

/// Anthropic, cached 2026-06-24.
#[must_use]
pub fn anthropic(model: &str) -> Option<CostModel> {
    Some(match model {
        "claude-fable-5" | "claude-mythos-5" => per_token(1000, 5000),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => {
            per_token(500, 2500)
        }
        "claude-sonnet-5" => per_token(200, 1000),
        "claude-sonnet-4-6" => per_token(300, 1500),
        "claude-haiku-4-5" => per_token(100, 500),
        _ => return None,
    })
}

/// OpenAI, cached 2026-08-31.
#[must_use]
pub fn openai(model: &str) -> Option<CostModel> {
    Some(match model {
        "gpt-5" | "gpt-5.1" => per_token(250, 2000),
        "gpt-5-mini" => per_token(45, 360),
        "gpt-5.2" => per_token(350, 2800),
        "gpt-5.4-mini" => per_token(150, 900),
        "gpt-5.5" => per_token(500, 3000),
        "gpt-5.5-pro" => per_token(3000, 18000),
        "gpt-5.6-sol" => per_token(1000, 1250),
        "gpt-5.6-terra" => per_token(400, 500),
        "gpt-5.6-luna" => per_token(40, 50),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_have_rates() {
        assert_eq!(anthropic("claude-opus-5"), Some(per_token(500, 2500)));
        assert_eq!(openai("gpt-5.6-terra"), Some(per_token(400, 500)));
    }

    #[test]
    fn unknown_models_report_nothing_rather_than_guessing() {
        assert_eq!(anthropic("claude-from-the-future"), None);
        assert_eq!(openai("gpt-9"), None);
        // Prefix matches are not enough. "gpt-5.6" is not "gpt-5.6-terra".
        assert_eq!(openai("gpt-5.6"), None);
    }

    #[test]
    fn a_terra_turn_costs_what_the_page_says() {
        // One million in, one million out. $4.00 plus $5.00 is 900 cents.
        let cost = openai("gpt-5.6-terra").unwrap();
        assert_eq!(cost.charge(1_000_000, 1_000_000), Cents::new(900));
    }
}
