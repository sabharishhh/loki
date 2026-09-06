//! Which of the pages an engine offered are worth reading (§12.7).
//!
//! **The expensive part of a search is reading, not finding.** Discovery comes back with ten or
//! twenty results and the budget reads three of them, so which three is most of the answer's
//! quality and all of its cost. Taking the engine's first three takes whatever the engine
//! optimised for, which is not the same as what was asked.
//!
//! **Lexical and local, because the alternative is a model call in front of every search.** An
//! embedding rerank is what a server does with a GPU already warm. Here the question and the
//! titles are both short, the overlap between them is a real signal, and it costs microseconds.
//!
//! **The engine's order is the tiebreak, never overridden without cause.** An engine has ranking
//! signals this cannot see, so a result only moves when there is a reason in the text itself.

use crate::ports::search::Hit;

/// Reorders results so the ones that answer the question come first.
///
/// A stable sort, so anything this cannot tell apart keeps the order the engine gave it.
#[must_use]
pub fn best_first(question: &str, mut hits: Vec<Hit>) -> Vec<Hit> {
    let wanted = content_words(question);
    if wanted.is_empty() {
        return hits;
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(score(&wanted, hit)));
    hits
}

/// How well one result answers the question.
///
/// The title is worth more than the snippet: a snippet is an excerpt chosen for containing the
/// query, so it matches almost by construction, while a title is what the page says it is about.
fn score(wanted: &[String], hit: &Hit) -> u32 {
    let title = hit.title.to_lowercase();
    let snippet = hit.snippet.to_lowercase();
    wanted
        .iter()
        .map(|word| {
            u32::from(title.contains(word.as_str())) * 3
                + u32::from(snippet.contains(word.as_str()))
        })
        .sum()
}

/// The words in a question that carry its subject.
///
/// Short words and the ones every question contains are dropped: matching on "the" ranks every
/// page equally, which is the same as not ranking at all.
pub(crate) fn content_words(question: &str) -> Vec<String> {
    const EVERYWHERE: [&str; 24] = [
        "the", "and", "for", "what", "when", "where", "which", "who", "why", "how", "is", "are",
        "was", "were", "does", "did", "do", "can", "with", "from", "that", "this", "about", "into",
    ];
    question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() > 2 && !EVERYWHERE.contains(&word.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, snippet: &str) -> Hit {
        Hit {
            url: format!("https://example.com/{}", title.replace(' ', "-")),
            title: title.to_owned(),
            snippet: snippet.to_owned(),
        }
    }

    #[test]
    fn the_result_about_the_question_comes_first() {
        let hits = vec![
            hit("Cricket scores", "live scores"),
            hit("Kerala monsoon rainfall today", "rain across Kerala"),
        ];
        let ranked = best_first("kerala monsoon rainfall", hits);
        assert_eq!(ranked[0].title, "Kerala monsoon rainfall today");
    }

    /// A title match beats a snippet match, because a snippet is chosen for containing the query.
    #[test]
    fn a_title_counts_for_more_than_a_snippet() {
        let hits = vec![
            hit("Homepage", "rust async cancellation explained at length"),
            hit("Rust async cancellation", "a note"),
        ];
        let ranked = best_first("rust async cancellation", hits);
        assert_eq!(ranked[0].title, "Rust async cancellation");
    }

    /// **The engine knows things this does not.** Results it cannot tell apart must come back in
    /// the order they arrived, or the rerank is quietly shuffling good rankings.
    #[test]
    fn results_it_cannot_separate_keep_the_engines_order() {
        let hits = vec![hit("First", ""), hit("Second", ""), hit("Third", "")];
        let ranked = best_first("nothing matches any of these", hits);
        let order: Vec<&str> = ranked.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(order, ["First", "Second", "Third"]);
    }

    /// A question of nothing but common words ranks nothing, rather than ranking everything zero
    /// and reordering on a tie.
    #[test]
    fn a_question_with_no_subject_leaves_the_order_alone() {
        let hits = vec![hit("Alpha", ""), hit("Beta", "")];
        let ranked = best_first("what is the how and why", hits);
        assert_eq!(ranked[0].title, "Alpha");
    }

    #[test]
    fn an_empty_result_set_is_not_a_special_case() {
        assert!(best_first("anything", Vec::new()).is_empty());
    }
}
