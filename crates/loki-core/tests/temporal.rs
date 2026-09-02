//! §21.6. Principle 9, checked rather than trusted.
//!
//! The risk §27 names is that principle 9 becomes ceremony: every temporal value is supposed to
//! route through the host, and a lazy path reintroduces a raw date in a prompt template. That is
//! discovered a year later, as the assistant quietly getting time wrong, unless something fails at
//! build time.
//!
//! Two checks. One reads the source of the places that build prompt text and refuses a date field
//! formatted directly. The other builds a real prompt and asserts that every instant in it arrives
//! with a distance beside it.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::civil::date;
use loki_core::core::temporal;
use loki_core::memory::claim::{Claim, Origin, Privacy};
use loki_core::memory::concept::Status;
use loki_core::memory::handle;
use loki_core::memory::index::{Layer, Recalled, Score};

/// Files that turn stored values into text a model or a person reads.
///
/// `temporal.rs` itself is the one place allowed to format a date, which is the whole point of it
/// existing. Everything here has to go through it.
const PROMPT_BUILDERS: [&str; 4] = [
    "core/prompt.rs",
    "memory/gate.rs",
    "memory/handle.rs",
    "memory/timeline.rs",
];

/// Field accesses that yield an instant. Written with the dot, because `learned` on its own is
/// also an ordinary English word and `format!("learned, {}", ...)` is a label, not a date.
const INSTANTS: [&str; 8] = [
    ".valid_from",
    ".valid_to",
    ".learned",
    ".unlearned",
    ".stale_after",
    ".generated.at",
    ".replaced_from",
    ".replaced_to",
];

fn src(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)
}

/// Lines of real code, with `//` comments and blank lines dropped.
///
/// Without this the rule fires on a doc comment that merely names a field, which is how a check
/// like this ends up disabled.
fn code_lines(path: &Path) -> Vec<(usize, String)> {
    fs::read_to_string(path).map_or_else(
        |_| Vec::new(),
        |text| {
            text.lines()
                .enumerate()
                .map(|(n, line)| (n + 1, line.trim().to_owned()))
                .filter(|(_, line)| !line.is_empty() && !line.starts_with("//"))
                .collect()
        },
    )
}

/// A line that both formats something and reads an instant field, without calling the renderer.
///
/// **What this does not catch.** A date bound to a local first, then interpolated by that name,
/// reads as an ordinary variable and no static rule can tell it from any other. That gap is what
/// the behavioural tests below are for: they assert on real rendered output rather than on source
/// text. This check is the cheap half, and it covers the lazy path, which is reaching straight
/// into the struct.
fn offends(line: &str) -> bool {
    let formats = line.contains("format!")
        || line.contains("push_str")
        || line.contains("write!")
        || line.contains("to_string()");
    let names_instant = INSTANTS.iter().any(|field| line.contains(field));
    formats && names_instant && !line.contains("temporal::")
}

#[test]
fn no_prompt_template_emits_a_raw_instant() {
    let mut offences = Vec::new();
    for file in PROMPT_BUILDERS {
        for (number, line) in code_lines(&src(file)) {
            if offends(&line) {
                offences.push(format!("{file}:{number}: {line}"));
            }
        }
    }

    assert!(
        offences.is_empty(),
        "principle 9: a date reaching a prompt or the timeline has to go through \
         `core::temporal`, so the model is never asked to work out how long ago something was, \
         and the interface and the model cannot disagree about it.\n{}",
        offences.join("\n")
    );
}

/// The check above is only worth having if it can fail. A raw interpolation must trip it.
#[test]
fn the_check_catches_what_it_is_for() {
    assert!(offends(
        r#"out.push_str(&format!("since {}", claim.validity.valid_from));"#
    ));
    assert!(!offends(
        r#"out.push_str(&temporal::since(valid_from, today));"#
    ));
    assert!(
        !offends(r#"    pub valid_from: Option<Date>,"#),
        "a field declaration is not a template"
    );
}

fn recalled(text: &str, valid_from: Option<jiff::civil::Date>) -> Recalled {
    Recalled {
        layer: Layer::Consolidated,
        path: "people/sabharish.md".to_owned(),
        name: "Sabharish".to_owned(),
        heading: "role".to_owned(),
        ordinal: 0,
        text: text.to_owned(),
        status: Status::Stable,
        privacy: Privacy::Normal,
        origin: Origin::Stated,
        valid_from,
        score: Score::default(),
    }
}

/// §10.9's rule, on real output: both halves, always. The instant makes the claim checkable
/// against the file, and the distance is what the model would otherwise compute wrong.
#[test]
fn a_recalled_claim_carries_the_instant_and_the_distance() {
    let text = handle::render(
        &[recalled("Works on the infra team", Some(date(2026, 7, 15)))],
        date(2026, 9, 2),
    );

    assert!(text.contains("15 July"), "{text}");
    assert!(text.contains("about seven weeks"), "{text}");
}

/// A claim the source never dated has no distance to state, and inventing one would be a lie
/// about the record (§9.5).
#[test]
fn an_undated_claim_carries_no_distance() {
    let text = handle::render(
        &[recalled("Sabharish is a computer science graduate", None)],
        date(2026, 9, 2),
    );

    assert_eq!(text.trim(), "- Sabharish is a computer science graduate");
}

/// §17.3's sentence and the prompt's distances come from one renderer, so they round the same way.
#[test]
fn the_timeline_and_the_prompt_agree_on_a_distance() {
    let in_a_prompt = handle::render(
        &[recalled("Works on the infra team", Some(date(2026, 7, 15)))],
        date(2026, 8, 29),
    );
    let on_the_timeline = temporal::span(temporal::calendar_days(
        date(2026, 7, 15),
        date(2026, 8, 29),
    ));

    assert_eq!(on_the_timeline, "about six weeks");
    assert!(
        in_a_prompt.contains(&on_the_timeline),
        "one renderer, one answer: {in_a_prompt} against {on_the_timeline}"
    );
}

/// The claim record stores instants, and only the rendering is relative (§9.14).
#[test]
fn the_file_keeps_the_instant_the_sentence_rounds() {
    let claim =
        Claim::stated("Works on the infra team", date(2026, 8, 29)).dated(date(2026, 7, 15));
    assert_eq!(claim.validity.valid_from, Some(date(2026, 7, 15)));
    assert_eq!(claim.validity.learned, date(2026, 8, 29));
}

/// §8.3's split, end to end. The value that changes every turn is in turn content; the one that
/// holds for the session is the only temporal thing the prefix carries.
#[test]
fn what_moves_is_in_the_turn_and_what_holds_is_in_the_prefix() {
    use jiff::tz::TimeZone;
    use loki_core::core::prompt::{Prefix, Turn};
    use loki_core::core::vocab::ModelRole;

    let started: jiff::Timestamp = "2026-09-02T08:10:00Z".parse().expect("timestamp");
    let now = "2026-09-02T08:50:00Z"
        .parse::<jiff::Timestamp>()
        .expect("timestamp")
        .to_zoned(TimeZone::get("Asia/Kolkata").expect("zone"));

    let mut prefix = Prefix::new("You are Loki.");
    prefix.set_session_start(&now);
    let mut turn = Turn::new();
    turn.set_frame(temporal::Frame::new(now, started, Some(date(2026, 8, 30))).render());

    let request = loki_core::core::prompt::build(&prefix, &turn, ModelRole::Primary, 1_000);
    let system: String = request.system.iter().map(|b| b.text.clone()).collect();

    assert!(system.contains("This session began"), "{system}");
    assert!(
        !system.contains("Now: "),
        "the moving value in the prefix would miss the cache every turn: {system}"
    );
    assert!(request.messages[0].content.starts_with("Now: "));
    assert!(
        request.messages[0]
            .content
            .contains("Before today, you last spoke three days ago."),
        "{:?}",
        request.messages[0]
    );
}
