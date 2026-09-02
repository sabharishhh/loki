//! One renderer for every time a person or a model reads (§8.3, §9.14, §10.9, §17.3).
//!
//! **Absolute in the file. Relative in the prompt and the interface. The conversion happens here.**
//! A file is a record and has to be unambiguous years later, so it stores instants. A sentence is
//! for a person, and a person says "six weeks ago". Neither side does the other's job.
//!
//! **Why one module rather than two.** §10.9 requires the timeline's sentence and the prompt's
//! distances to come from one renderer, so the interface and the model cannot disagree about how
//! long ago something was. Two functions that both round weeks will eventually round them
//! differently, and the disagreement surfaces as the product contradicting itself.
//!
//! Principle 9 is the reason any of this is here. On TReMu's three tasks, standard LLM approaches
//! score roughly 30 percent against about 78 for approaches that hand the computation to code, and
//! a 2026 ACL Findings paper found no model above 65 percent alignment with human temporal
//! judgement even when given timestamps. A model can use a resolved temporal fact and cannot
//! reliably derive one.

use jiff::civil::Date;
use jiff::{Timestamp, Zoned};

/// Days below which a gap since the last session is not worth remarking on (§26, question 22).
///
/// One number, in one place, because there is no principled value yet. Three days is probably not
/// news and three weeks probably is; this sits between them and moves when §21 has real sessions
/// to look at.
pub const GAP_WORTH_MENTIONING_DAYS: i64 = 4;

/// The three lines of §8.3, computed before the model call and carried in turn content.
///
/// Turn content, never the prefix. Putting the current time in the system prompt breaks the
/// provider cache on every single turn, which is the obvious placement and the wrong one.
///
/// Capped and stable in shape: always three lines, so it compresses well in the model's attention
/// and never grows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    now: Zoned,
    session_started: Timestamp,
    /// The last local day the user said anything, from before this session started.
    ///
    /// A date rather than an instant, because the line it feeds is calendar-based and a date is
    /// what the store can actually know: episodes are dated files, and the last one is where the
    /// user last spoke. Inventing an instant from a date would be precision we do not have.
    last_spoke: Option<Date>,
}

impl Frame {
    /// Builds the frame from one clock read.
    ///
    /// `last_spoke` is `None` on a first run, which is a fact worth stating rather than a line to
    /// drop: the shape stays three lines either way.
    #[must_use]
    pub const fn new(now: Zoned, session_started: Timestamp, last_spoke: Option<Date>) -> Self {
        Self {
            now,
            session_started,
            last_spoke,
        }
    }

    /// Whether the gap since the last session is long enough to be worth a person noticing.
    ///
    /// Separate from rendering, because §17.4's summary and §12.6's search trigger want the
    /// judgement and the frame always states the fact regardless.
    #[must_use]
    pub fn gap_is_notable(&self) -> bool {
        self.days_since_last()
            .is_some_and(|days| days >= GAP_WORTH_MENTIONING_DAYS)
    }

    /// Calendar days since the last session, in the local zone.
    ///
    /// Calendar, not elapsed. Someone on Wednesday who last spoke on Sunday says "three days ago"
    /// even if it was 71 hours, and §9.14's whole zone argument is that "yesterday" is a local
    /// civil fact rather than a count of seconds.
    fn days_since_last(&self) -> Option<i64> {
        Some(calendar_days(self.last_spoke?, self.now.date()))
    }

    /// The frame as it reaches the model.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "Now: {}\nThis session started {}.\n{}\n",
            self.stamp(),
            // Elapsed rather than calendar, because a session's age is a real duration: one that
            // began forty minutes ago began forty minutes ago whatever the date did in between.
            elapsed((self.now.timestamp().as_second() - self.session_started.as_second()).max(0)),
            self.last_line()
        )
    }

    /// `Wednesday 2 September 2026, 14:20 (Asia/Kolkata)`.
    fn stamp(&self) -> String {
        format!(
            "{} ({})",
            self.now.strftime("%A %-d %B %Y, %H:%M"),
            self.now.time_zone().iana_name().unwrap_or("local")
        )
    }

    fn last_line(&self) -> String {
        let Some(days) = self.days_since_last() else {
            return "This is your first session.".to_owned();
        };
        if days == 0 {
            return "You last spoke earlier today.".to_owned();
        }
        format!("Before today, you last spoke {}.", ago(days))
    }
}

/// How long ago something was, as a person says it. Never a bare number of days.
///
/// `today` and `yesterday` rather than "0 days ago", because a distance of zero is not a distance.
#[must_use]
pub fn ago(days: i64) -> String {
    match days {
        d if d <= 0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        d => format!("{} ago", span(d)),
    }
}

/// A duration in days, as a person says it. The unit widens as the number grows.
///
/// Rounded, and it says so. "About six weeks" is what a person means and what §17.3's sentence
/// reads as; "45 days" is what a database means.
#[must_use]
pub fn span(days: i64) -> String {
    let days = days.max(0);
    match days {
        0 => "no time".to_owned(),
        1 => "a day".to_owned(),
        2..=10 => format!("{} days", count(days)),
        11..=13 => "about a week".to_owned(),
        14..=75 => weeks(days),
        // Months stop short of a year. Thirteen months is a true answer and a worse sentence than
        // "about one year", which is what a person would have said.
        76..=364 => months(days),
        _ => years(days),
    }
}

/// `Since 15 July, about seven weeks.` Both halves, on purpose (§10.9).
///
/// The instant is what makes the claim checkable against the file. The distance is what the model
/// would otherwise compute, and §9.14 is the evidence that it computes it wrong.
#[must_use]
pub fn since(from: Date, today: Date) -> String {
    let days = calendar_days(from, today);
    if days == 0 {
        return format!("Since {}.", day_month(from, today));
    }
    format!("Since {}, {}.", day_month(from, today), span(days))
}

/// `15 July`, or `15 July 2024` when it is not the year we are in.
///
/// The year is noise on a date inside the current year and essential outside it, so it appears
/// when it carries information. `today` is a parameter rather than a clock read, which is
/// principle 9 and is what lets §21.2 render at any point on a timeline.
#[must_use]
pub fn day_month(date: Date, today: Date) -> String {
    if date.year() == today.year() {
        date.strftime("%-d %B").to_string()
    } else {
        date.strftime("%-d %B %Y").to_string()
    }
}

/// Calendar days between two local dates, never negative.
///
/// The unit every distance in this module is built on. Calendar rather than elapsed, because a
/// person counts days by the date changing and not by twenty-four hours passing.
#[must_use]
pub fn calendar_days(from: Date, to: Date) -> i64 {
    to.since(from).map_or(0, |s| i64::from(s.get_days())).max(0)
}

/// A real duration, for the one line of the frame that measures one.
fn elapsed(seconds: i64) -> String {
    const DAY: i64 = 86_400;
    if seconds >= DAY {
        return ago(seconds / DAY);
    }
    match seconds {
        s if s < 90 => "just now".to_owned(),
        s if s < 5_400 => format!("{} minutes ago", count((s + 30) / 60)),
        s => format!("{} ago", plural(count((s + 1_800) / 3_600), "hour")),
    }
}

fn weeks(days: i64) -> String {
    format!("about {}", plural(count((days + 3) / 7), "week"))
}

fn months(days: i64) -> String {
    format!("about {}", plural(count((days * 2 + 30) / 61), "month"))
}

fn years(days: i64) -> String {
    format!("about {}", plural(count((days + 182) / 365), "year"))
}

fn plural(counted: String, unit: &str) -> String {
    if counted == "one" {
        format!("{counted} {unit}")
    } else {
        format!("{counted} {unit}s")
    }
}

/// Small numbers as words, larger ones as digits.
///
/// "Six weeks" is how §17.3's sentence is written and how a person reads it. "37 months" is how a
/// person reads a number that large.
fn count(n: i64) -> String {
    const WORDS: [&str; 13] = [
        "no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    usize::try_from(n)
        .ok()
        .and_then(|at| WORDS.get(at))
        .map_or_else(|| n.to_string(), |word| (*word).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    /// §17.3's sentence: told on 29 August about a move on 15 July, wrong for six weeks.
    #[test]
    fn the_worked_example_reads_as_the_document_writes_it() {
        assert_eq!(span(45), "about six weeks");
    }

    /// §10.9's example, verbatim.
    #[test]
    fn a_recalled_claim_carries_the_instant_and_the_distance() {
        assert_eq!(
            since(date(2026, 7, 15), date(2026, 9, 2)),
            "Since 15 July, about seven weeks."
        );
    }

    /// The year is what tells a reader that a date is not from this year.
    #[test]
    fn the_year_appears_only_when_it_carries_information() {
        assert_eq!(day_month(date(2026, 7, 15), date(2026, 9, 2)), "15 July");
        assert_eq!(
            day_month(date(2024, 7, 15), date(2026, 9, 2)),
            "15 July 2024"
        );
        assert!(since(date(2024, 3, 1), date(2026, 9, 2)).contains("2024"));
    }

    #[test]
    fn the_unit_widens_as_the_number_grows() {
        assert_eq!(span(1), "a day");
        assert_eq!(span(3), "three days");
        assert_eq!(span(12), "about a week");
        assert_eq!(span(21), "about three weeks");
        assert_eq!(span(90), "about three months");
        assert_eq!(span(400), "about one year");
        assert_eq!(span(1_100), "about three years");
    }

    /// A distance of zero is not a distance, and a person never says "0 days ago".
    #[test]
    fn today_and_yesterday_are_words_not_numbers() {
        assert_eq!(ago(0), "today");
        assert_eq!(ago(1), "yesterday");
        assert_eq!(ago(3), "three days ago");
    }

    #[test]
    fn the_frame_is_three_lines_and_names_the_zone() {
        let kolkata = TimeZone::get("Asia/Kolkata").expect("zone");
        let now = at("2026-09-02T08:50:00Z").to_zoned(kolkata);
        let frame = Frame::new(now, at("2026-09-02T08:10:00Z"), Some(date(2026, 8, 30)));

        let text = frame.render();
        let lines: Vec<&str> = text.trim().lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "Now: Wednesday 2 September 2026, 14:20 (Asia/Kolkata)"
        );
        assert_eq!(lines[1], "This session started 40 minutes ago.");
        assert_eq!(lines[2], "Before today, you last spoke three days ago.");
    }

    /// The shape holds at three lines even when there is nothing to compare against.
    #[test]
    fn a_first_session_still_renders_three_lines() {
        let now = at("2026-09-02T08:50:00Z").to_zoned(TimeZone::UTC);
        let frame = Frame::new(now, at("2026-09-02T08:49:00Z"), None);
        let text = frame.render();
        assert_eq!(text.trim().lines().count(), 3, "{text}");
        assert!(text.contains("This is your first session."));
        assert!(!frame.gap_is_notable());
    }

    /// A second session on the same day is not a gap, and saying "before today" would be wrong.
    #[test]
    fn a_same_day_second_session_is_not_a_gap() {
        let now = at("2026-09-02T17:00:00Z").to_zoned(TimeZone::UTC);
        let frame = Frame::new(now, at("2026-09-02T16:55:00Z"), Some(date(2026, 9, 2)));
        assert!(frame.render().contains("You last spoke earlier today."));
        assert!(!frame.gap_is_notable(), "hours apart is not an absence");
    }

    /// Three weeks away is what §8.3 says a person expects to be noticed.
    #[test]
    fn a_long_absence_is_notable() {
        let now = at("2026-09-02T09:00:00Z").to_zoned(TimeZone::UTC);
        let frame = Frame::new(now, at("2026-09-02T08:59:00Z"), Some(date(2026, 8, 12)));
        assert!(frame.gap_is_notable());
        assert!(frame.render().contains("about three weeks ago"));
    }
}
