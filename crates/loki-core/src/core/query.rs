//! Turning what someone said into what to search for (§12.6).
//!
//! **A question and a query are not the same string.** "hey can you tell me what the news is in
//! kerala today please" carries six words of address and politeness that an engine has to rank
//! around. Stripping them costs nothing and no model call, which matters because this runs before
//! the model on every turn the host decides for itself.
//!
//! **Deliberately conservative, because the failure is silent.** A rewrite that drops a word the
//! question turned on returns confident results about something else, and nothing downstream can
//! tell that from a good answer. So this removes only the wrapper around a question, never the
//! question, and hands back the original whenever it is unsure. The larger rewrite the industry
//! does, one question fanned out into several, needs a model to judge intent and belongs with the
//! model's own `WEB:` request rather than here.

/// What a search engine should be asked, given what the user said.
///
/// Falls back to the original whenever stripping would leave nothing to search for.
#[must_use]
pub fn for_search(message: &str) -> String {
    let trimmed = message.trim();
    let mut words: Vec<&str> = trimmed.split_whitespace().collect();

    strip_leading(&mut words);
    strip_trailing(&mut words);

    let rebuilt = words.join(" ");
    let cleaned = rebuilt.trim_matches(|c: char| c == ',' || c == '?' || c == '!' || c == '.');

    // **Nothing survived that anyone was asking about.** "hey loki" strips to "loki", which is a
    // search for the assistant's own name rather than for anything. Length is the wrong test here
    // and was the first one written: "bitcoin" is short and is the whole question, while "loki" is
    // longer and is none of it. The test is whether a word is left that is not itself address.
    if cleaned.is_empty() || words.iter().all(|word| is_address(bare(word))) {
        return trimmed.to_owned();
    }
    cleaned.to_owned()
}

/// Whether a word only ever appears as address or politeness, never as a subject.
fn is_address(word: &str) -> bool {
    OPENERS
        .iter()
        .chain(CLOSERS.iter())
        .flat_map(|phrase| phrase.iter())
        .any(|known| word.eq_ignore_ascii_case(known))
}

/// Address and politeness, longest phrase first so "do you know" beats "do".
const OPENERS: [&[&str]; 24] = [
    &["do", "you", "know"],
    &["did", "you", "know"],
    &["i", "want", "to", "know"],
    &["i", "need", "to", "know"],
    &["i", "am", "curious", "about"],
    &["tell", "me", "about"],
    &["can", "you", "tell", "me"],
    &["could", "you", "tell", "me"],
    &["can", "you"],
    &["could", "you"],
    &["would", "you"],
    &["will", "you"],
    &["tell", "me"],
    &["show", "me"],
    &["give", "me"],
    &["find", "me"],
    &["search", "for"],
    &["look", "up"],
    &["hey"],
    &["hi"],
    &["hello"],
    &["please"],
    &["pls"],
    &["loki"],
];

/// Politeness at the end, which an engine ranks as content.
const CLOSERS: [&[&str]; 6] = [
    &["thank", "you"],
    &["for", "me"],
    &["thanks"],
    &["please"],
    &["pls"],
    &["ta"],
];

fn strip_leading(words: &mut Vec<&str>) {
    let mut stripping = true;
    while stripping {
        stripping = false;
        for phrase in OPENERS {
            if starts_with(words, phrase) {
                words.drain(..phrase.len());
                stripping = true;
                break;
            }
        }
        // A comma left behind by "hey loki, what is..." is not a word anyone searched for.
        if words.first().is_some_and(|first| bare(first).is_empty()) {
            words.remove(0);
            stripping = true;
        }
    }
}

fn strip_trailing(words: &mut Vec<&str>) {
    let mut stripping = true;
    while stripping {
        stripping = false;
        for phrase in CLOSERS {
            if ends_with(words, phrase) {
                words.truncate(words.len() - phrase.len());
                stripping = true;
                break;
            }
        }
    }
}

fn starts_with(words: &[&str], phrase: &[&str]) -> bool {
    words.len() > phrase.len()
        && words
            .iter()
            .zip(phrase)
            .all(|(word, want)| bare(word).eq_ignore_ascii_case(want))
}

fn ends_with(words: &[&str], phrase: &[&str]) -> bool {
    words.len() > phrase.len()
        && words[words.len() - phrase.len()..]
            .iter()
            .zip(phrase)
            .all(|(word, want)| bare(word).eq_ignore_ascii_case(want))
}

/// A word without the punctuation stuck to it.
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_comes_off_and_the_question_stays() {
        assert_eq!(
            for_search("hey loki, can you tell me what the news is in kerala today please?"),
            "what the news is in kerala today"
        );
    }

    #[test]
    fn a_query_someone_already_wrote_well_is_left_alone() {
        let already = "rust async cancellation safety";
        assert_eq!(for_search(already), already);
    }

    #[test]
    fn an_instruction_to_search_is_not_part_of_the_search() {
        assert_eq!(
            for_search("search for the rust 1.96 release notes"),
            "the rust 1.96 release notes"
        );
        assert_eq!(
            for_search("look up manchester united fixtures"),
            "manchester united fixtures"
        );
    }

    /// The failure this is written against: stripping until nothing useful is left. A message that
    /// is all address keeps its own words rather than reaching an engine as an empty string.
    #[test]
    fn a_message_that_is_only_address_is_left_whole() {
        assert_eq!(for_search("hey loki"), "hey loki");
        assert_eq!(for_search("please"), "please");
    }

    /// The case that broke the first guard, which tested length. One real word is a whole query
    /// when it is the subject, and "loki" is longer than "bitcoin" and is not one.
    #[test]
    fn one_real_word_is_enough_of_a_query() {
        assert_eq!(for_search("tell me about bitcoin"), "bitcoin");
    }

    /// A word that merely looks like politeness in the middle of a question is content.
    #[test]
    fn politeness_inside_the_question_is_not_stripped() {
        assert_eq!(
            for_search("what does please and thank you mean in japanese"),
            "what does please and thank you mean in japanese"
        );
    }

    /// Not everything that opens with an interrogative is filler, and dropping it changes the
    /// question. "how to" is the classic: without it the query is an instruction, not a request.
    #[test]
    fn an_interrogative_is_never_treated_as_wrapper() {
        assert_eq!(
            for_search("how to reverse a linked list"),
            "how to reverse a linked list"
        );
        assert_eq!(for_search("why is the sky blue"), "why is the sky blue");
    }

    #[test]
    fn an_empty_or_punctuation_only_message_survives() {
        assert_eq!(for_search("   "), "");
        assert_eq!(for_search("???"), "???");
    }
}
