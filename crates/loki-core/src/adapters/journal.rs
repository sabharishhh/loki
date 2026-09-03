//! A plain-text transcript of everything a session did, for reading over afterwards.
//!
//! §20.1 says the audit trail is the event stream, persisted per session, and §17.1 gives that
//! stream two renderers. This is a third: a file, written as it happens, meant to be read by a
//! person and grepped by one.
//!
//! **Ring 2 only, and that is the point.** Nothing in the core knows this exists. Events arrive
//! through [`EventSink`], which §6.4 already designed for extra consumers, and the prompts and
//! replies arrive through [`Journalled`], a decorator around the `ModelProvider` port. A
//! diagnostic that needed a core change would be a diagnostic worth arguing about.
//!
//! **It contains everything, in plain text.** Every prompt includes the working set, so anything
//! Loki knows about you is in this file, and so is every word of every message. It lives on your
//! machine and is never sent anywhere, but it is not redacted and is not meant to be shared.
//! `LOKI_LOG=off` turns it off.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use jiff::Zoned;
use tokio_util::sync::CancellationToken;

use crate::core::event::Event;
use crate::core::sink::EventSink;
use crate::ports::model::{Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request};

/// Where one line's label ends and its content begins. Fixed, so the file reads as columns.
const GUTTER: usize = 26;

/// Writes the session transcript.
///
/// Every session appends to the same file under a banner carrying its own id, so a day's use is
/// one document and a single session is one `grep` away.
/// What a session has spent, in tokens.
///
/// Three numbers rather than two, because they answer different questions. Input and output are
/// cumulative and tell you what the session cost. **Context is the last call's input** and tells
/// you how big the prompt has grown, which is the number §21.3 watches: if it climbs with session
/// count, consolidation is letting noise in, and every other symptom of that shows up months later
/// as the assistant getting vaguer instead of sharper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Tokens {
    /// Every input token this session has sent, cached prefix included.
    pub input: u64,
    /// Every output token it has received.
    pub output: u64,
    /// The most recent call's input. What the model is currently carrying.
    pub context: u64,
    /// Model calls made. Input divided by this is the average prompt size.
    pub calls: u64,
}

pub struct Journal {
    file: Mutex<Option<File>>,
    id: String,
    tokens: Mutex<Tokens>,
}

impl Journal {
    /// Opens the journal and writes a session banner.
    ///
    /// Never fails loudly: a diagnostic that stops the app it is diagnosing is worse than no
    /// diagnostic. A journal that cannot open its file quietly writes nothing.
    #[must_use]
    pub fn open(path: &Path, version: &str) -> Self {
        if std::env::var("LOKI_LOG").is_ok_and(|v| v.eq_ignore_ascii_case("off")) {
            return Self::silent();
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(file) = OpenOptions::new().create(true).append(true).open(path) else {
            return Self::silent();
        };

        let now = Zoned::now();
        let id = session_id(&now);
        let journal = Self {
            file: Mutex::new(Some(file)),
            id: id.clone(),
            tokens: Mutex::new(Tokens::default()),
        };
        journal.raw(&format!(
            "\n\n{}\nsession {id}   {}   loki {version}\n{}\n",
            "=".repeat(78),
            now.strftime("%A %-d %B %Y, %H:%M:%S %:z"),
            "=".repeat(78),
        ));
        journal
    }

    /// A journal that writes nothing, for tests and for `LOKI_LOG=off`.
    #[must_use]
    pub fn silent() -> Self {
        Self {
            file: Mutex::new(None),
            id: "off".to_owned(),
            tokens: Mutex::new(Tokens::default()),
        }
    }

    /// What this session has spent so far.
    ///
    /// Counted from the event stream rather than the ledger, because the ledger is per day and
    /// per month and this question is per session. Kept even when the file is silent, so turning
    /// the log off does not turn the interface's counter off with it.
    #[must_use]
    pub fn tokens(&self) -> Tokens {
        self.tokens
            .lock()
            .map_or_else(|e| *e.into_inner(), |guard| *guard)
    }

    /// Writes the session's totals. Called at close, and safe to call more than once.
    pub fn totals(&self) {
        let spent = self.tokens();
        self.line(
            "session",
            &format!(
                "{} calls, {} in, {} out, {} in context",
                spent.calls, spent.input, spent.output, spent.context
            ),
        );
    }

    /// This session's id, as it appears in the banner.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What the model was sent.
    ///
    /// The whole prompt, because a transcript that summarises the prompt cannot answer the
    /// question it exists for: why did it say that.
    pub fn sent(&self, request: &Request) {
        let system: usize = request.system.iter().map(|b| b.text.len()).sum();
        let turn: usize = request.messages.iter().map(|m| m.content.len()).sum();
        self.line(
            "prompt",
            &format!(
                "{:?} role, {} system blocks, {} messages, {} chars prefix, {turn} chars turn",
                request.role,
                request.system.len(),
                request.messages.len(),
                system,
            ),
        );
        for (at, block) in request.system.iter().enumerate() {
            self.block(&format!("system {}", at + 1), &block.text);
        }
        for message in &request.messages {
            self.block(&format!("{:?}", message.role), &message.content);
        }
    }

    /// What came back, and how long it took.
    pub fn received(&self, text: &str, ms: u128) {
        self.line("reply", &format!("{ms} ms, {} chars", text.len()));
        self.block("loki", text);
    }

    /// A line the caller wants in the transcript that no event covers.
    pub fn note(&self, label: &str, detail: &str) {
        self.line(label, detail);
    }

    /// `label ▸ detail`, timestamped, with the detail aligned into a column.
    fn line(&self, label: &str, detail: &str) {
        let stamp = Zoned::now().strftime("%H:%M:%S%.3f").to_string();
        let head = format!("[{stamp}] {label} ");
        let pad = GUTTER.saturating_sub(head.chars().count());
        self.raw(&format!("{head}{}▸ {detail}\n", " ".repeat(pad)));
    }

    /// An indented block of text, for anything multi-line.
    fn block(&self, label: &str, text: &str) {
        let mut out = format!("{:>width$}── {label} ──\n", "", width = GUTTER);
        for line in text.lines() {
            out.push_str(&format!("{:>width$}{line}\n", "", width = GUTTER + 2));
        }
        if text.is_empty() {
            out.push_str(&format!("{:>width$}(empty)\n", "", width = GUTTER + 2));
        }
        self.raw(&out);
    }

    fn raw(&self, text: &str) {
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush();
        }
    }
}

/// A short, sortable id. The time is already in the banner, so this only has to be distinct.
fn session_id(now: &Zoned) -> String {
    now.strftime("%y%m%d-%H%M%S").to_string()
}

impl EventSink for Journal {
    fn emit(&self, event: &Event) {
        // The trace renderer already turns an event into one line, so the journal reuses it rather
        // than growing a second vocabulary that could drift from it.
        if let Event::ModelCall {
            tokens_in,
            tokens_out,
            ..
        } = event
            && let Ok(mut spent) = self.tokens.lock()
        {
            spent.input = spent.input.saturating_add(u64::from(*tokens_in));
            spent.output = spent.output.saturating_add(u64::from(*tokens_out));
            spent.context = u64::from(*tokens_in);
            spent.calls = spent.calls.saturating_add(1);
        }

        let detail = crate::core::render::trace(event);
        let label = match event {
            Event::MemoryRecalled { .. } => "recall",
            Event::MemoryWritten { .. } => "memory",
            Event::ModelCall { .. } => "cost",
            Event::TaskStarted { .. } | Event::TaskFinished { .. } => "task",
            Event::Interrupted { .. } | Event::Resumed { .. } => "interrupt",
            Event::Blocked { .. } | Event::BudgetWarning { .. } => "blocked",
            _ => "event",
        };
        self.line(label, &detail);
    }
}

/// Wraps a provider so every request and every reply reaches the journal.
///
/// A decorator on the `ModelProvider` port rather than a hook in the loop. The port already exists
/// and already carries exactly the two things a transcript is missing, so this needs no change to
/// Ring 0 or Ring 1 at all.
pub struct Journalled {
    inner: std::sync::Arc<dyn ModelProvider>,
    journal: std::sync::Arc<Journal>,
}

impl Journalled {
    /// Not generic on purpose. The one caller already holds a boxed provider, and a blanket
    /// `impl ModelProvider for Arc<T>` to make a generic version work would be a Ring 1 change
    /// for a diagnostic.
    #[must_use]
    pub const fn new(
        inner: std::sync::Arc<dyn ModelProvider>,
        journal: std::sync::Arc<Journal>,
    ) -> Self {
        Self { inner, journal }
    }
}

#[async_trait]
impl ModelProvider for Journalled {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    async fn complete(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        self.journal.sent(&req);
        let started = std::time::Instant::now();
        let journal = std::sync::Arc::clone(&self.journal);

        let stream = match self.inner.complete(req, cancel).await {
            Ok(stream) => stream,
            Err(why) => {
                journal.note("failed", &why.to_string());
                return Err(why);
            }
        };

        // The reply is accumulated as it streams and written once, so the transcript reads as a
        // message rather than as a thousand fragments.
        Ok(Box::pin(async_stream::stream! {
            let mut stream = stream;
            let mut reply = String::new();
            while let Some(chunk) = stream.next().await {
                if let Ok(Chunk::Text(piece)) = &chunk {
                    reply.push_str(piece);
                }
                if let Err(why) = &chunk {
                    journal.note("failed", &why.to_string());
                }
                yield chunk;
            }
            journal.received(&reply, started.elapsed().as_millis());
        }))
    }
}

/// The journal's own file, for a caller that wants to tell the user where it is.
///
/// # Errors
/// Fails if the application support directory cannot be found.
pub fn path() -> Result<PathBuf, crate::Error> {
    crate::paths::journal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vocab::ModelRole;
    use crate::ports::model::{Message, SystemBlock};

    fn temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "loki-journal-{}-{label}-{:?}.log",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn a_session_writes_a_banner_and_a_prompt() {
        let path = temp("banner");
        let _ = std::fs::remove_file(&path);

        let journal = Journal::open(&path, "0.3.0");
        journal.sent(&Request {
            role: ModelRole::Primary,
            system: vec![SystemBlock::new("You are Loki.")],
            messages: vec![Message::user("hello")],
            max_tokens: 100,
        });
        journal.received("Hi.", 12);

        let text = std::fs::read_to_string(&path).expect("written");
        assert!(
            text.contains(&format!("session {}", journal.id())),
            "{text}"
        );
        assert!(text.contains("You are Loki."), "{text}");
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("Hi."), "{text}");
        assert!(text.contains("12 ms"), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    /// Two sessions in one file, each findable on its own.
    #[test]
    fn a_second_session_appends_under_its_own_banner() {
        let path = temp("append");
        let _ = std::fs::remove_file(&path);

        let first = Journal::open(&path, "0.3.0");
        first.note("you", "first session");
        drop(first);

        // Same second, so the ids can collide; the banners still have to be two.
        let second = Journal::open(&path, "0.3.0");
        second.note("you", "second session");

        let text = std::fs::read_to_string(&path).expect("written");
        assert_eq!(text.matches("session ").count(), 2, "{text}");
        assert!(text.contains("first session") && text.contains("second session"));
        let _ = std::fs::remove_file(&path);
    }

    /// A journal that cannot write must not take the app with it.
    #[test]
    fn a_journal_that_cannot_open_stays_quiet() {
        let journal = Journal::open(Path::new("/nowhere/at/all/loki.log"), "0.3.0");
        journal.note("you", "this goes nowhere");
        journal.received("and so does this", 1);
    }

    /// The three numbers answer different questions, so they accumulate differently.
    #[test]
    fn input_and_output_accumulate_and_context_is_the_latest_call() {
        use crate::core::ids::TaskId;
        use crate::core::vocab::{CostModel, Locality};

        let journal = Journal::silent();
        let call = |tokens_in, tokens_out| Event::ModelCall {
            task: TaskId::new(0),
            provider: "test".to_owned(),
            role: ModelRole::Primary,
            locality: Locality::Cloud,
            tokens_in,
            tokens_out,
            cost: CostModel::Free,
        };

        journal.emit(&call(1_000, 50));
        journal.emit(&call(1_400, 80));

        let spent = journal.tokens();
        assert_eq!(spent.input, 2_400, "input is cumulative");
        assert_eq!(spent.output, 130, "so is output");
        assert_eq!(
            spent.context, 1_400,
            "context is the last prompt, not the sum of every prompt"
        );
        assert_eq!(spent.calls, 2);
    }

    /// Turning the file off must not turn the counter off with it: the interface shows these.
    #[test]
    fn a_silent_journal_still_counts() {
        use crate::core::ids::TaskId;
        use crate::core::vocab::{CostModel, Locality};

        let journal = Journal::silent();
        journal.emit(&Event::ModelCall {
            task: TaskId::new(0),
            provider: "test".to_owned(),
            role: ModelRole::Primary,
            locality: Locality::Cloud,
            tokens_in: 10,
            tokens_out: 5,
            cost: CostModel::Free,
        });
        assert_eq!(journal.tokens().input, 10);
    }

    #[test]
    fn a_silent_journal_writes_nothing() {
        let path = temp("silent");
        let _ = std::fs::remove_file(&path);
        Journal::silent().note("you", "nothing");
        assert!(!path.exists());
    }
}
