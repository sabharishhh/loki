//! The two-zone prompt.
//!
//! Provider caches key on an exact prefix, so anything that changes per turn must sit after
//! everything that does not. Retrieval lands in turn content, never in the prefix.

use jiff::Zoned;

use super::ids::ConceptId;
use crate::core::vocab::ModelRole;
use crate::ports::model::{Message, Request, SystemBlock};

/// How long a standing instruction lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Until the session ends.
    Session,
    /// Until the user removes it.
    Persistent,
}

/// A directive that compaction can never remove.
///
/// An instruction that can be summarized away is not an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub text: String,
    pub scope: Scope,
}

impl Standing {
    #[must_use]
    pub fn session(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            scope: Scope::Session,
        }
    }

    #[must_use]
    pub fn persistent(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            scope: Scope::Persistent,
        }
    }
}

/// The frozen zone. Changes once per session, or on an explicit instruction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefix {
    system: String,
    standing: Vec<Standing>,
    working_set: Option<String>,
    /// When this session began, stated once (§8.3).
    ///
    /// The only temporal value the prefix may carry, because it is the only one that does not
    /// change during a session. Everything that moves is in the frame, in turn content, since a
    /// value that changes per turn misses the provider cache on every turn.
    session_start: Option<String>,
}

impl Prefix {
    #[must_use]
    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            standing: Vec::new(),
            working_set: None,
            session_start: None,
        }
    }

    /// Anchors the session in the prefix. Once per session, so it costs no cache hit.
    pub fn set_session_start(&mut self, when: &Zoned) {
        self.session_start = Some(format!(
            "This session began {} ({}).",
            when.strftime("%A %-d %B %Y, %H:%M"),
            when.time_zone().iana_name().unwrap_or("local")
        ));
    }

    /// Adds a standing instruction. Costs one cache miss and buys permanence.
    pub fn add_standing(&mut self, instruction: Standing) {
        self.standing.push(instruction);
    }

    pub fn set_working_set(&mut self, text: impl Into<String>) {
        self.working_set = Some(text.into());
    }

    #[must_use]
    pub fn standing(&self) -> &[Standing] {
        &self.standing
    }

    /// Drops session-scoped instructions. Called when a session ends, never by compaction.
    pub fn end_session(&mut self) {
        self.standing.retain(|s| s.scope == Scope::Persistent);
        self.session_start = None;
    }

    /// The prefix as provider blocks, with the cache breakpoint on the last one.
    #[must_use]
    pub fn blocks(&self) -> Vec<SystemBlock> {
        let mut blocks = vec![SystemBlock::new(&self.system)];

        if let Some(start) = &self.session_start {
            blocks.push(SystemBlock::new(start));
        }

        if !self.standing.is_empty() {
            let joined = self
                .standing
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(SystemBlock::new(joined));
        }

        if let Some(working_set) = &self.working_set {
            blocks.push(SystemBlock::new(working_set));
        }

        if let Some(last) = blocks.last_mut() {
            last.cache = true;
        }
        blocks
    }
}

/// The turn zone. Rebuilt every turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    recalled: Vec<ConceptId>,
    recall: String,
    /// §8.3's three lines, host-computed. Turn content, never the prefix: putting the current time
    /// in the system prompt is the obvious placement and it breaks the cache on every single turn.
    frame: String,
    history: Vec<Message>,
}

impl Turn {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            recalled: Vec::new(),
            recall: String::new(),
            frame: String::new(),
            history: Vec::new(),
        }
    }

    pub fn push(&mut self, message: Message) {
        self.history.push(message);
    }

    /// Claims pre-fetch surfaced for this turn. Turn content, never the prefix.
    pub fn set_recalled(&mut self, concepts: Vec<ConceptId>) {
        self.recalled = concepts;
    }

    #[must_use]
    pub fn recalled(&self) -> &[ConceptId] {
        &self.recalled
    }

    /// What pre-fetch found, as the text the model will read.
    ///
    /// Replaced every turn and never appended to history, because it is derived: letting it
    /// accumulate would grow the turn zone without bound and re-send stale recall for ever.
    /// Sets the temporal frame for this turn. Rendered once per turn from one clock read.
    pub fn set_frame(&mut self, text: impl Into<String>) {
        self.frame = text.into();
    }

    #[must_use]
    pub fn frame(&self) -> &str {
        &self.frame
    }

    pub fn set_recall(&mut self, text: impl Into<String>) {
        self.recall = text.into();
    }

    #[must_use]
    pub fn recall(&self) -> &str {
        &self.recall
    }

    #[must_use]
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.history.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Summarizes older turns down to `keep` most recent messages.
    ///
    /// Only ever touches history. The prefix is not an input here, which is what makes a standing
    /// instruction impossible to compact away.
    pub fn compact(&mut self, keep: usize, summary: impl Into<String>) {
        if self.history.len() <= keep {
            return;
        }
        let tail = self.history.split_off(self.history.len() - keep);
        self.history = std::iter::once(Message::user(summary.into()))
            .chain(tail)
            .collect();
    }
}

/// Assembles a request from the two zones.
#[must_use]
pub fn build(prefix: &Prefix, turn: &Turn, role: ModelRole, max_tokens: u32) -> Request {
    let mut messages = Vec::with_capacity(turn.history().len() + 2);
    // The frame first, so every distance in the recall below is read against a stated now.
    if !turn.frame().is_empty() {
        messages.push(Message::user(turn.frame().to_owned()));
    }
    // Retrieval lands in the turn, never in the prefix. §8.1: the two are compatible only because
    // of that, and putting it in the prefix would miss the provider cache on every single turn.
    if !turn.recall().is_empty() {
        messages.push(Message::user(format!(
            "What you already know that may bear on this:\n{}",
            turn.recall()
        )));
    }
    messages.extend_from_slice(turn.history());
    Request {
        role,
        system: prefix.blocks(),
        messages,
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_prefix_block_carries_the_cache_breakpoint() {
        let mut prefix = Prefix::new("You are Loki.");
        prefix.add_standing(Standing::session("Do nothing until told."));
        prefix.set_working_set("Sabharish works on infra.");

        let blocks = prefix.blocks();
        assert_eq!(blocks.len(), 3);
        assert!(!blocks[0].cache);
        assert!(!blocks[1].cache);
        assert!(blocks[2].cache);
    }

    #[test]
    fn a_bare_prefix_still_marks_a_breakpoint() {
        let blocks = Prefix::new("You are Loki.").blocks();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].cache);
    }

    #[test]
    fn compaction_never_touches_a_standing_instruction() {
        let mut prefix = Prefix::new("You are Loki.");
        prefix.add_standing(Standing::session("Do nothing until told."));

        let mut turn = Turn::new();
        for i in 0..200 {
            turn.push(Message::user(format!("message {i}")));
        }
        turn.compact(10, "Earlier: the user asked about infra.");

        assert_eq!(turn.len(), 11);
        let rendered = prefix
            .blocks()
            .iter()
            .map(|b| b.text.clone())
            .collect::<String>();
        assert!(rendered.contains("Do nothing until told."));
    }

    #[test]
    fn compaction_keeps_the_most_recent_turns() {
        let mut turn = Turn::new();
        for i in 0..20 {
            turn.push(Message::user(format!("m{i}")));
        }
        turn.compact(3, "summary");

        let history = turn.history();
        assert_eq!(history[0].content, "summary");
        assert_eq!(history[1].content, "m17");
        assert_eq!(history[3].content, "m19");
    }

    #[test]
    fn compaction_below_the_threshold_is_a_no_op() {
        let mut turn = Turn::new();
        turn.push(Message::user("only one"));
        turn.compact(10, "summary");
        assert_eq!(turn.len(), 1);
    }

    #[test]
    fn ending_a_session_drops_session_scoped_instructions_only() {
        let mut prefix = Prefix::new("You are Loki.");
        prefix.add_standing(Standing::session("Just for now."));
        prefix.add_standing(Standing::persistent("Always be brief."));
        prefix.end_session();

        assert_eq!(prefix.standing().len(), 1);
        assert_eq!(prefix.standing()[0].text, "Always be brief.");
    }

    #[test]
    fn recall_lands_in_turn_content_not_the_prefix() {
        let prefix = Prefix::new("You are Loki.");
        let mut turn = Turn::new();
        turn.set_recalled(vec![ConceptId::new("people/meera.md")]);
        turn.push(Message::user("who is meera"));

        let req = build(&prefix, &turn, ModelRole::Primary, 1024);
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(turn.recalled().len(), 1);
    }
}
