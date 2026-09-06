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
///
/// **`asked` used to live here and was always false.** It meant "the user asked in so many words",
/// which is what `asks_for_the_web` reads off the message standing right beside it, so the field
/// was a second copy of the same question that nobody ever answered.
#[derive(Debug, Clone, Copy)]
pub struct Situation {
    /// The best score lane 1 returned, if it returned anything.
    pub recall: Option<f32>,
}

/// Reads §12.6's rows in order.
///
/// # Panics
/// Never.
#[must_use]
pub fn decide(message: &str, situation: Situation) -> Reach {
    // Row 3's explicit half. An instruction outranks every heuristic below it, including a memory
    // that would otherwise have answered: being told to look it up is not a question about the past.
    if asks_for_the_web(message) {
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

    // The model's voice, and reaching here means the question is about the user and memory did not
    // answer it. Forcing a search on that was the other half of B-87: the web does not know where
    // you work, so the present tense alone is not enough to be sure of anything. A score cannot
    // tell a confident hit from a current one either, so the turn is offered the choice.
    Reach::Offer
}

/// The message reduced to its words, lowercased, space separated and padded at both ends.
///
/// **Every phrase list in this module is matched against this rather than against the raw message.**
/// A plain substring found "now " inside "what do you know about me", so a question about what Loki
/// remembers read as a question about the present and spent eleven seconds on the web before
/// answering it from memory anyway (B-87). The lists are short and English, and every one of them
/// meant whole words all along.
fn words(message: &str) -> String {
    let mut said = String::with_capacity(message.len() + 2);
    said.push(' ');
    for c in message.chars() {
        if c.is_alphanumeric() || c == '\'' {
            said.extend(c.to_lowercase());
        } else if !said.ends_with(' ') {
            said.push(' ');
        }
    }
    if !said.ends_with(' ') {
        said.push(' ');
    }
    said
}

/// Whether `phrase` appears in `said` as whole words. `said` comes from [`words`].
fn says(said: &str, phrase: &str) -> bool {
    said.match_indices(phrase).any(|(at, hit)| {
        let before = at == 0 || said.as_bytes()[at - 1] == b' ';
        let after = said
            .as_bytes()
            .get(at + hit.len())
            .is_none_or(|b| *b == b' ');
        before && after
    })
}

/// Whether `stem` appears in `said`, allowing the endings English puts on the end of a word.
///
/// "searching the web" is the same instruction as "search the web", and "prices" is the same
/// question as "price". A list that has to spell out every ending is a list that will be missing
/// one, which is the failure the whole-word rule was brought in to stop repeating.
fn does(said: &str, stem: &str) -> bool {
    ["", "s", "es", "ed", "ing"]
        .iter()
        .any(|ending| says(said, &format!("{stem}{ending}")))
}

/// Whether the message is an instruction to go and look, rather than a question that might need it.
///
/// **A fast path, not a gate, and the difference is what a miss costs.** Anything this misses still
/// reaches the model as §12.6's offer, so a phrasing nobody thought of costs one extra round trip
/// rather than the search never happening. It was a gate before, and "refer online" asked twice got
/// no search at all.
///
/// **Composed rather than enumerated.** A list of whole phrases is a list of the ways somebody
/// already thought of; a verb of looking plus somewhere to look covers the ones they did not.
/// "refer online", "go and check the internet", "verify this on the web" are all the same sentence
/// to this and none of them would have been written down.
///
/// A false positive here costs a search, never a wrong answer, which is why the rule leans towards
/// catching too much.
fn asks_for_the_web(message: &str) -> bool {
    let said = words(message);

    // Instructions that mean it without naming anywhere to look. "fact-check" is not listed
    // separately because a hyphen is a word break by the time this reads it.
    const OUTRIGHT: [&str; 13] = [
        "google it",
        "google that",
        "google this",
        "look it up",
        "look this up",
        "look that up",
        "search for",
        "fact check",
        "cite sources",
        "cite your sources",
        "with sources",
        "with citations",
        "proper citations",
    ];
    if OUTRIGHT.iter().any(|phrase| says(&said, phrase)) {
        return true;
    }

    // Otherwise: a verb of looking, and somewhere to look.
    const LOOKING: [&str; 12] = [
        "search", "look", "check", "verify", "browse", "refer", "consult", "find", "google",
        "read up", "dig up", "pull up",
    ];
    const OUT_THERE: [&str; 6] = ["web", "online", "internet", "the net", "google", "browser"];

    LOOKING.iter().any(|verb| does(&said, verb)) && OUT_THERE.iter().any(|place| says(&said, place))
}

/// Things whose answers do not move.
fn is_stable(message: &str) -> bool {
    let lowered = message.to_lowercase();
    let said = words(message);
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
    if settled.iter().any(|phrase| does(&said, phrase)) {
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
    let said = words(message);
    ["my", "i", "me", "mine", "our", "we", "i'm", "im"]
        .iter()
        .any(|word| says(&said, word))
}

/// Whether the answer depends on the present.
///
/// **Not "is it after the cutoff".** v0.8 asked the model to reason about its own training date and
/// §9.14 is the evidence that it cannot. The frame carries the real date, so the question becomes
/// whether the answer would be different today, which is answerable from the words.
fn depends_on_now(message: &str) -> bool {
    let lowered = message.to_lowercase();
    let said = words(message);
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
        "still",
        "now",
    ];
    if present.iter().any(|phrase| does(&said, phrase)) {
        return true;
    }
    // A question about the past is memory's, not the web's, even when it names a year.
    !runtime::asks_about_the_past(message) && lowered.contains("20") && lowered.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTHING: Situation = Situation { recall: None };

    #[test]
    fn being_told_to_look_outranks_everything() {
        // Even a memory that would have answered: an instruction is not a question about the past.
        let known = Situation { recall: Some(0.99) };
        assert_eq!(decide("search the web for rust 1.98", known), Reach::Yes);
        assert_eq!(
            decide("look it up", known),
            Reach::Yes,
            "being told to look outranks a memory that would have answered"
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
        let remembered = Situation { recall: Some(0.95) };
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
        let remembered = Situation { recall: Some(0.95) };
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
        let known = Situation { recall: Some(0.8) };
        assert_eq!(decide("what is my sister called", known), Reach::No);
        // A weak hit does not end it, which is the case that made the model get a voice.
        let vague = Situation { recall: Some(0.2) };
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

    /// B-87, and the reason every list in this module now matches whole words.
    ///
    /// "know about me" contains "now ", so a question about what Loki remembers was read as a
    /// question about the present and spent eleven seconds on the web before answering it from
    /// memory anyway. A substring is not a word, and every list here meant words.
    #[test]
    fn a_word_that_merely_contains_a_present_word_is_not_about_the_present() {
        let vague = Situation { recall: Some(0.3) };
        for question in [
            "what do you know about me",
            "do you know about my sister",
            "what is your knowledge of rust",
        ] {
            assert_ne!(decide(question, vague), Reach::Yes, "{question}");
        }
    }

    /// The same rule on the other lists, where a substring forced a search rather than blocked one.
    #[test]
    fn a_word_hidden_inside_another_word_is_not_an_instruction_to_look() {
        for message in [
            // "refer" inside "preference", and a place standing beside it.
            "my preference is the online one",
            // "the net" inside "the netflix", with a verb of looking in front of it.
            "find the netflix password in my notes",
        ] {
            assert_ne!(decide(message, NOTHING), Reach::Yes, "{message}");
        }
    }

    /// Whole words, not whole spellings. A stem list that has to name every ending will miss one.
    #[test]
    fn the_endings_english_puts_on_a_word_still_land() {
        assert_eq!(decide("searching the web for this", NOTHING), Reach::Yes);
        assert_eq!(decide("what are the current prices", NOTHING), Reach::Yes);
    }

    /// The present tense is not enough to be sure when the question is about the user.
    ///
    /// Memory did not answer and the web cannot: the host has run out of things it knows, which is
    /// what Offer means. Forcing a search here was the second half of B-87.
    #[test]
    fn a_present_tense_question_about_the_user_is_offered_not_forced() {
        assert_eq!(decide("what is my current mood", NOTHING), Reach::Offer);
    }

    /// A question about the past belongs to memory even when it names a year.
    #[test]
    fn the_past_is_memorys_even_with_a_date_in_it() {
        assert_ne!(decide("what did i say in 2025?", NOTHING), Reach::Yes);
        assert_ne!(decide("do you remember 2024?", NOTHING), Reach::Yes);
    }
}

#[cfg(test)]
mod asking_outright {
    use super::*;

    fn reach(message: &str) -> Reach {
        decide(message, Situation { recall: None })
    }

    /// W2, in Sabharish's own words. Asked twice and searched neither time.
    #[test]
    fn refer_online_is_an_instruction_to_look() {
        assert_eq!(reach("refer online and then answer"), Reach::Yes);
        assert_eq!(
            reach("refer to her biography online and cite it"),
            Reach::Yes
        );
    }

    /// The point of composing a verb with a place: none of these was ever written down.
    #[test]
    fn phrasings_nobody_listed_still_land() {
        for message in [
            "go and check the internet for this",
            "verify this on the web",
            "consult the web before answering",
            "browse online and tell me",
            "pull up something from the internet",
            "dig up whatever the web says",
        ] {
            assert_eq!(reach(message), Reach::Yes, "{message}");
        }
    }

    #[test]
    fn the_old_phrasings_still_work() {
        for message in [
            "search the web for this",
            "look it up",
            "google it",
            "search for the release notes",
            "check online first",
        ] {
            assert_eq!(reach(message), Reach::Yes, "{message}");
        }
    }

    /// **A miss costs a round trip, never a search that never happens.** An instruction this does
    /// not recognise still reaches the model as the offer, which is the whole reason the rule is
    /// allowed to be simple.
    #[test]
    fn an_instruction_it_misses_is_still_offered() {
        assert_eq!(
            reach("have a rummage and tell me what you find"),
            Reach::Offer
        );
    }

    /// A verb of looking with nowhere to look is not an instruction to search.
    ///
    /// The assertion is that it does not *force* one. Offering is right: the host cannot tell, and
    /// the model reading the sentence will not search for a spelling check.
    #[test]
    fn looking_at_something_here_does_not_force_a_search() {
        for message in [
            "check my spelling in this paragraph",
            "look at the second one again",
            "find the bug in this function",
        ] {
            assert_ne!(reach(message), Reach::Yes, "{message}");
        }
    }

    /// Asking for sources is asking for the web, whether or not it names it.
    #[test]
    fn asking_for_sources_asks_for_the_web() {
        assert_eq!(
            reach("write me a para on her with proper citations"),
            Reach::Yes
        );
        assert_eq!(reach("answer with sources"), Reach::Yes);
    }

    /// Arithmetic stays out of it however it is phrased.
    #[test]
    fn a_sum_is_still_not_a_search() {
        assert_eq!(reach("what is 2 + 2"), Reach::No);
    }
}
