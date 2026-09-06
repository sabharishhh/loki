//! When to search, and when not to (§12.6).
//!
//! **The cost of searching when you should not is not latency, it is trust.** An assistant that
//! searches the web for something you told it last week reads as not knowing you, which is the same
//! cost as a wrong memory arriving from the other direction. So memory is consulted first, for
//! free, because pre-fetch has already run by the time this is asked.
//!
//! **A floor the host decides, plus the model's voice**, which is what §10.8 settled for memory
//! search and the argument transfers exactly: a score measures word match, never whether an answer
//! needs to be current. The floor is deterministic and costs nothing; the model may ask on a turn
//! where the question is about the present and the floor did not fire.

use crate::memory::runtime;

/// Whether this turn should reach the web.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Memory answered it, or it does not depend on the present.
    No,
    /// The host is sure. Searches without asking.
    Yes,
    /// The host is not sure, so the model is offered the choice (§10.8's shape).
    Offer,
}

/// What the host knows when it decides.
#[derive(Debug, Clone, Copy)]
pub struct Situation {
    /// The best score lane 1 returned, if it returned anything.
    pub recall: Option<f32>,
    /// Whether the user asked for it in so many words.
    pub asked: bool,
}

/// Reads §12.6's rows in order.
///
/// # Panics
/// Never.
#[must_use]
pub fn decide(message: &str, situation: Situation) -> Reach {
    // Row 3's explicit half. An instruction outranks every heuristic below it, including a memory
    // that would otherwise have answered: being told to look it up is not a question about the past.
    if situation.asked || asks_for_the_web(message) {
        return Reach::Yes;
    }

    // Row 2. Arithmetic, syntax and definitions do not change, and a search on one is pure latency.
    if is_stable(message) {
        return Reach::No;
    }

    let now = depends_on_now(message);
    let mine = about_the_user(message);

    // **Row 3 before row 1, and the order is the whole point.** Reading them the other way round
    // let a stored fact answer a question about a changing thing: told once that a price was four
    // thousand, Loki would answer "four thousand" to "what is it today" with a high recall score
    // and never look. A stored fact about something that moves is exactly the stale answer this
    // section exists to avoid, so what the world is doing now outranks what memory remembers.
    if now && !mine {
        return Reach::Yes;
    }

    // Row 1. Memory already answered, and §12.6 says that is the end of it. The threshold is the
    // same one lane 1 uses for its own confidence, so the two cannot disagree about what
    // "answered" means. Reached only for questions about the user, where a remembered answer is
    // the point rather than a risk.
    if situation.recall.is_some_and(|score| score >= 0.6) {
        return Reach::No;
    }

    // A present-tense question about the user that memory could not answer. The web does not know
    // where you work either, but the host has nothing left to decide with.
    if now {
        return Reach::Yes;
    }

    // The model's voice. The host is not sure, and a score cannot tell a confident hit from a
    // current one, so the turn is offered the choice rather than having it made for it.
    Reach::Offer
}

/// An instruction to look, rather than a question that might need looking.
fn asks_for_the_web(message: &str) -> bool {
    let lowered = message.to_lowercase();
    [
        "search the web",
        "look it up",
        "look this up",
        "google",
        "search for",
        "find online",
        "on the web",
        "check online",
    ]
    .iter()
    .any(|phrase| lowered.contains(phrase))
}

/// Things whose answers do not move.
fn is_stable(message: &str) -> bool {
    let lowered = message.to_lowercase();
    // Arithmetic, and questions about language rather than about the world.
    let settled = [
        "what does this code",
        "explain this",
        "what is the syntax",
        "how do i write",
        "rewrite",
        "refactor",
        "translate",
        "summarise",
        "summarize",
    ];
    if settled.iter().any(|phrase| lowered.contains(phrase)) {
        return true;
    }
    // A sum is not a search. The lead-in comes off first: nobody types a bare expression, they
    // type "what is 2 + 2", and a check that demanded the whole string be digits caught neither.
    let expression = [
        "what is",
        "whats",
        "what's",
        "calculate",
        "compute",
        "work out",
    ]
    .iter()
    .fold(lowered.clone(), |text, lead| text.replace(lead, " "));
    let expression = expression.trim();
    let arithmetic = !expression.is_empty()
        && expression
            .chars()
            .all(|c| c.is_ascii_digit() || " +-*/=?.,()^%x".contains(c));
    arithmetic && expression.chars().any(|c| c.is_ascii_digit())
}

/// Whether the question is about the user rather than about the world.
///
/// **The line memory sits on.** "Where do I work now" and "what is the price now" both name the
/// present, and only one of them is memory's. Without this, moving the present-tense check above
/// the memory check would send every question about the user to the web, which is the trust cost
/// §12.6 opens by naming: an assistant that searches for something you told it last week reads as
/// not knowing you.
fn about_the_user(message: &str) -> bool {
    let lowered = format!(" {} ", message.to_lowercase());
    [
        " my ", " i ", " me ", " mine ", " our ", " we ", " i'm ", " im ",
    ]
    .iter()
    .any(|word| lowered.contains(word))
}

/// Whether the answer depends on the present.
///
/// **Not "is it after the cutoff".** v0.8 asked the model to reason about its own training date and
/// §9.14 is the evidence that it cannot. The frame carries the real date, so the question becomes
/// whether the answer would be different today, which is answerable from the words.
fn depends_on_now(message: &str) -> bool {
    let lowered = message.to_lowercase();
    let present = [
        "latest",
        "current",
        "currently",
        "right now",
        "today",
        "this week",
        "this month",
        "this year",
        "recent",
        "recently",
        "news",
        "price",
        "stock",
        "weather",
        "score",
        "released",
        "release date",
        "version",
        "who won",
        "still ",
        "now ",
    ];
    if present.iter().any(|phrase| lowered.contains(phrase)) {
        return true;
    }
    // A question about the past is memory's, not the web's, even when it names a year.
    !runtime::asks_about_the_past(message) && lowered.contains("20") && lowered.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTHING: Situation = Situation {
        recall: None,
        asked: false,
    };

    #[test]
    fn being_told_to_look_outranks_everything() {
        // Even a memory that would have answered: an instruction is not a question about the past.
        let known = Situation {
            recall: Some(0.99),
            asked: false,
        };
        assert_eq!(decide("search the web for rust 1.98", known), Reach::Yes);
        assert_eq!(decide("look it up", known), Reach::Yes);
        assert_eq!(
            decide(
                "anything",
                Situation {
                    recall: Some(0.99),
                    asked: true
                }
            ),
            Reach::Yes
        );
    }

    /// The bug this ordering exists to fix, and the reason it is worth a test of its own.
    ///
    /// Told once what a price was, memory scores highly on a question about the price today. Read
    /// in the old order that ended the turn and Loki answered with last month's number, confidently
    /// and without looking. A stored fact about a thing that moves is the stale answer §12.6 is
    /// written against.
    #[test]
    fn a_strong_memory_does_not_answer_a_question_about_now() {
        let remembered = Situation {
            recall: Some(0.95),
            asked: false,
        };
        for question in [
            "what is the price of the pixel today",
            "what is the latest rust version",
            "any news about the launch",
            "what is the current score",
        ] {
            assert_eq!(decide(question, remembered), Reach::Yes, "{question}");
        }
    }

    /// And the other half, which is why the fix is not simply "the present always wins".
    ///
    /// "Where do I work now" names the present too, and it is memory's question. Sending it to the
    /// web is the trust cost §12.6 opens by naming.
    #[test]
    fn a_question_about_the_user_stays_with_memory_even_in_the_present_tense() {
        let remembered = Situation {
            recall: Some(0.95),
            asked: false,
        };
        for question in [
            "where do i work now",
            "what is my current address",
            "who is my manager today",
            "what are our latest plans",
        ] {
            assert_eq!(decide(question, remembered), Reach::No, "{question}");
        }
    }

    #[test]
    fn memory_answering_ends_it() {
        let known = Situation {
            recall: Some(0.8),
            asked: false,
        };
        assert_eq!(decide("what is my sister called", known), Reach::No);
        // A weak hit does not end it, which is the case that made the model get a voice.
        let vague = Situation {
            recall: Some(0.2),
            asked: false,
        };
        assert_ne!(decide("what is my sister called", vague), Reach::No);
    }

    #[test]
    fn a_question_about_the_present_searches_without_asking() {
        for question in [
            "what is the latest rust version",
            "who won the match",
            "what is the weather today",
            "current price of bitcoin",
            "any news about the release date",
        ] {
            assert_eq!(decide(question, NOTHING), Reach::Yes, "{question}");
        }
    }

    /// The trust cost this section is written about: searching for something that cannot have
    /// changed reads as not knowing the user.
    #[test]
    fn settled_things_never_search() {
        for question in [
            "what is 2 + 2",
            "explain this function",
            "rewrite this more simply",
            "what is the syntax for a closure",
            "summarise the above",
        ] {
            assert_eq!(decide(question, NOTHING), Reach::No, "{question}");
        }
    }

    /// Nobody types a bare expression. The lead-in has to come off before the check.
    #[test]
    fn arithmetic_is_recognised_through_the_words_around_it() {
        for sum in [
            "2+2",
            "what is 2 + 2",
            "whats 17 * 4?",
            "calculate (3 + 4) / 2",
        ] {
            assert_eq!(decide(sum, NOTHING), Reach::No, "{sum}");
        }
        // A sentence that merely contains a number is not a sum.
        assert_ne!(decide("what is rust 1.98 about", NOTHING), Reach::No);
    }

    /// The host is not sure, so it does not decide. §10.8's shape, applied here.
    #[test]
    fn an_ordinary_question_is_offered_rather_than_decided() {
        assert_eq!(
            decide("tell me about the rust foundation", NOTHING),
            Reach::Offer
        );
        assert_eq!(
            decide("how does tls fingerprinting work", NOTHING),
            Reach::Offer
        );
    }

    /// A question about the past belongs to memory even when it names a year.
    #[test]
    fn the_past_is_memorys_even_with_a_date_in_it() {
        assert_ne!(decide("what did i say in 2025?", NOTHING), Reach::Yes);
        assert_ne!(decide("do you remember 2024?", NOTHING), Reach::Yes);
    }
}
