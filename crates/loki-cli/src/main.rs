//! Dev harness.
//!
//! Drives the core without the Mac app. Reads the model key from the environment, which is a
//! development stand-in only. The real key lives in the macOS Keychain behind `SecretStore`, and
//! that arrives in Phase 4.

use std::io::Write;
use std::sync::Arc;

use loki_core::adapters::{anthropic::Anthropic, openai::Openai};
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, TokenSink};
use loki_core::core::event::Event;
use loki_core::core::prompt::Prefix;
use loki_core::core::render;
use loki_core::core::sink::EventSink;
use loki_core::core::vocab::Cents;
use loki_core::ports::model::ModelProvider;
use tokio_util::sync::CancellationToken;

const SYSTEM: &str = "You are Loki, a personal assistant that runs on the user's Mac. \
Answer plainly. Do not use em dashes.";

/// Prints the plain view. The trace view reads the same events.
struct Plain;

impl EventSink for Plain {
    fn emit(&self, event: &Event) {
        if let Some(line) = render::plain(event) {
            eprintln!("  {line}");
        }
    }
}

/// Prints every event, with timings and costs.
struct Trace;

impl EventSink for Trace {
    fn emit(&self, event: &Event) {
        eprintln!("  {}", render::trace(event));
    }
}

struct Stdout;

impl TokenSink for Stdout {
    fn token(&self, text: &str) {
        print!("{text}");
        let _ = std::io::stdout().flush();
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `LOKI_PROVIDER` picks between the two when both keys are set. `LOKI_MODEL` overrides the
/// provider's default model.
fn provider() -> Result<Arc<dyn ModelProvider>, String> {
    let model = env("LOKI_MODEL");
    let anthropic = env("ANTHROPIC_API_KEY");
    let openai = env("OPENAI_API_KEY");

    let build_anthropic = |key: String| -> Result<Arc<dyn ModelProvider>, String> {
        let p = Anthropic::new(key).map_err(|e| e.to_string())?;
        Ok(Arc::new(match &model {
            Some(m) => p.with_model(m),
            None => p,
        }))
    };
    let build_openai = |key: String| -> Result<Arc<dyn ModelProvider>, String> {
        let p = Openai::new(key).map_err(|e| e.to_string())?;
        Ok(Arc::new(match &model {
            Some(m) => p.with_model(m),
            None => p,
        }))
    };

    match env("LOKI_PROVIDER").as_deref() {
        Some("openai") => {
            build_openai(openai.ok_or("LOKI_PROVIDER is openai but OPENAI_API_KEY is not set")?)
        }
        Some("anthropic") => build_anthropic(
            anthropic.ok_or("LOKI_PROVIDER is anthropic but ANTHROPIC_API_KEY is not set")?,
        ),
        Some(other) => Err(format!(
            "unknown LOKI_PROVIDER {other}, use anthropic or openai"
        )),
        None => match (anthropic, openai) {
            (Some(key), _) => build_anthropic(key),
            (None, Some(key)) => build_openai(key),
            (None, None) => Err("set ANTHROPIC_API_KEY or OPENAI_API_KEY".to_owned()),
        },
    }
}

#[tokio::main]
async fn main() {
    let provider = match provider() {
        Ok(provider) => provider,
        Err(message) => {
            eprintln!("loki-core {}\n{message}", loki_core::VERSION);
            std::process::exit(1);
        }
    };

    let events: Arc<dyn EventSink> = if std::env::var("LOKI_TRACE").is_ok() {
        Arc::new(Trace)
    } else {
        Arc::new(Plain)
    };

    let mut core = Loop::new(
        Arc::clone(&provider),
        events,
        Arc::new(Stdout),
        Prefix::new(SYSTEM),
        Budget::new(Cents::new(500)),
    );

    let cancel = CancellationToken::new();
    let interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt.cancel();
        }
    });

    eprintln!("loki-core {} via {}", loki_core::VERSION, provider.id());
    eprintln!("Type a message. Ctrl-C interrupts, Ctrl-D quits.\n");

    loop {
        eprint!("> ");
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("{e}");
                break;
            }
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match core.turn_with(line, cancel.clone()).await {
            Ok(outcome) => println!("\n  [{:?}]", outcome.status),
            Err(e) => eprintln!("\n  {e}"),
        }
    }
}
