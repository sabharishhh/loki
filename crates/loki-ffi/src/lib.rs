//! The bridge.
//!
//! A small C ABI plus two registered callbacks the core invokes with serialized events and token
//! chunks. UniFFI is heavier than this surface needs.
//!
//! The whole surface from the architecture is declared here so the header is stable from the
//! start. Calls whose subsystem does not exist yet return [`LokiStatus::Unsupported`] rather than
//! being absent, because Ring 1 is versioned and adding a symbol later is a breaking change.
//!
//! Panics cannot cross this boundary. Since Rust 1.81 a panic escaping an `extern "C"` function
//! aborts the process rather than unwinding into the caller.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Arc;

use loki_core::adapters::anthropic::Anthropic;
use loki_core::adapters::clock::SystemClock;
use loki_core::adapters::openai::Openai;
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, TokenSink};
use loki_core::core::event::Event;
use loki_core::core::ledger::Ledger;
use loki_core::core::prompt::{Prefix, Standing};
use loki_core::core::sink::{Broadcast, EventSink};
use loki_core::core::vocab::Cents;
use loki_core::core::vocab::Locality;
use loki_core::memory::consolidate::ModelExtractor;
use loki_core::memory::gate::TierScope;
use loki_core::memory::handle::Memory;
use loki_core::memory::index::{Index, Recalled};
use loki_core::memory::resolve::ModelMatcher;
use loki_core::ports::model::ModelProvider;
use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Result of a bridge call.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LokiStatus {
    Ok = 0,
    /// A pointer was null, or a string was not valid UTF-8.
    InvalidArgument = 1,
    /// The core exists but cannot serve this call right now.
    NotReady = 2,
    /// The subsystem behind this call is not built yet.
    Unsupported = 3,
}

/// Receives one serialized [`Event`] as JSON.
pub type LokiEventCallback = extern "C" fn(json: *const c_char, user_data: *mut c_void);

/// Receives one chunk of response text as it streams.
pub type LokiTokenCallback = extern "C" fn(text: *const c_char, user_data: *mut c_void);

/// Which provider to construct.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LokiProvider {
    Anthropic = 0,
    Openai = 1,
}

struct Callbacks {
    event: LokiEventCallback,
    token: LokiTokenCallback,
    user_data: *mut c_void,
}

// SAFETY: `user_data` is owned by the caller, which guarantees it outlives the core and is safe to
// touch from any thread. That contract is stated on `loki_core_new`.
unsafe impl Send for Callbacks {}
unsafe impl Sync for Callbacks {}

impl EventSink for Callbacks {
    fn emit(&self, event: &Event) {
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        let Ok(c_string) = CString::new(json) else {
            return;
        };
        (self.event)(c_string.as_ptr(), self.user_data);
    }
}

impl TokenSink for Callbacks {
    fn token(&self, text: &str) {
        let Ok(c_string) = CString::new(text) else {
            return;
        };
        (self.token)(c_string.as_ptr(), self.user_data);
    }
}

/// An opaque handle to a running core.
pub struct LokiCore {
    runtime: Runtime,
    core: Arc<AsyncMutex<Loop>>,
    cancel: std::sync::Mutex<CancellationToken>,
    ledger: Option<Arc<Ledger>>,
    /// Held alongside the loop so the memory screens can read the store without a turn running.
    memory: Option<Arc<Memory>>,
    /// What pre-fetch surfaced on the last turn, for the `In play` rail (§9.2 of the design).
    recalled_slot: std::sync::Arc<std::sync::Mutex<Vec<Recalled>>>,
    /// What the last turn cited (§12.7). Same shape and the same reason as `recalled`: the rail
    /// asks after the turn, and the turn is long gone by then.
    cited_slot: std::sync::Arc<std::sync::Mutex<Vec<loki_core::core::websearch::Cited>>>,
    /// Where fetched bytes live, so an icon can be read back by hash.
    evidence: Option<std::sync::Arc<loki_core::memory::evidence::Evidence>>,
    /// The same clock the loop reads, so a screen and a turn never disagree about today.
    clock: Arc<dyn loki_core::ports::clock::Clock>,
    /// The session transcript, and the session's token counters behind it.
    journal: Arc<loki_core::adapters::journal::Journal>,
}

const SYSTEM: &str = "You are Loki, a personal assistant that runs on the user's Mac. \
Answer plainly. Do not use em dashes.";

const DEFAULT_CEILING: Cents = Cents::new(2000);

/// Reads a C string, rejecting null and invalid UTF-8.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string valid for the duration of the call.
unsafe fn as_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller contract above guarantees a valid NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Creates a core.
///
/// Returns null if any argument is invalid or the runtime cannot start.
///
/// `model` may be null, which uses the provider's default.
///
/// # Safety
/// `api_key` must be a valid NUL-terminated string, and `model` must be null or one. `user_data`
/// is passed back to both callbacks unchanged and must remain valid, and safe to use from any
/// thread, until `loki_core_free`. The callbacks are invoked from runtime worker threads, not the
/// caller's thread.
///
/// The key is taken here as an argument only until `SecretStore` lands. It is then read from the
/// Keychain and this parameter goes away.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_core_new(
    provider: LokiProvider,
    api_key: *const c_char,
    model: *const c_char,
    event: LokiEventCallback,
    token: LokiTokenCallback,
    user_data: *mut c_void,
) -> *mut LokiCore {
    // SAFETY: contract above.
    let Some(api_key) = (unsafe { as_str(api_key) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: contract above. Null is allowed and means "the provider's default".
    let model = unsafe { as_str(model) };

    let Ok(runtime) = Runtime::new() else {
        return std::ptr::null_mut();
    };

    let callbacks = Arc::new(Callbacks {
        event,
        token,
        user_data,
    });

    // The ledger is a second consumer of the same stream. If it cannot be opened the app still
    // runs, it just does not remember what it spent.
    let (ledger, spent) = Ledger::default_path()
        .and_then(|path| {
            let ledger = Arc::new(Ledger::open(&path)?);
            let spent = ledger.spent_this_month()?;
            Ok((Some(ledger), spent))
        })
        .unwrap_or((None, 0));

    let mut events = Broadcast::new().with(Arc::clone(&callbacks) as Arc<dyn EventSink>);
    if let Some(ledger) = &ledger {
        events = events.with(Arc::clone(ledger) as Arc<dyn EventSink>);
    }

    // Every prompt, reply and memory event, appended to one file per install. Opened before the
    // loop so the banner lands before anything else, and silent if it cannot write: a diagnostic
    // that stops the app it is diagnosing is worse than no diagnostic.
    let journal = Arc::new(loki_core::paths::journal().map_or_else(
        |_| loki_core::adapters::journal::Journal::silent(),
        |path| loki_core::adapters::journal::Journal::open(&path, loki_core::VERSION),
    ));
    events = events.with(Arc::clone(&journal) as Arc<dyn EventSink>);

    // The stream is finished before the transport is built, because §21.7's egress event has to
    // reach the same sinks as everything else, the journal included. A second stream for the wire
    // is a stream nobody reads.
    let events: Arc<dyn EventSink> = Arc::new(events);
    let Ok(http) = loki_core::adapters::egress::Http::new(Arc::clone(&events)) else {
        return std::ptr::null_mut();
    };
    let egress: Arc<dyn loki_core::ports::egress::Egress> = Arc::new(http);
    let provider: Arc<dyn ModelProvider> = match provider {
        LokiProvider::Anthropic => {
            let p = Anthropic::new(egress, api_key);
            Arc::new(match model {
                Some(m) => p.with_model(m),
                None => p,
            })
        }
        LokiProvider::Openai => {
            let p = Openai::new(egress, api_key);
            Arc::new(match model {
                Some(m) => p.with_model(m),
                None => p,
            })
        }
    };
    let provider: Arc<dyn ModelProvider> = Arc::new(loki_core::adapters::journal::Journalled::new(
        provider,
        Arc::clone(&journal),
    ));

    let clock: Arc<dyn loki_core::ports::clock::Clock> = Arc::new(SystemClock);
    let core = Loop::new(
        Arc::clone(&provider),
        Arc::clone(&events),
        callbacks as Arc<dyn TokenSink>,
        Arc::clone(&clock),
        Prefix::new(SYSTEM),
        Budget::resuming(DEFAULT_CEILING, spent),
    );

    // Opened here rather than lazily, so the working set reaches the frozen prefix before the
    // first turn. A store that will not open leaves a working assistant with no recall rather
    // than no assistant: the conversation is the floor, and memory is what it earns on top.
    let mut core = core;
    let memory = runtime.block_on(async {
        let memory = open_memory(Arc::clone(&events)).await?;
        core.attach_memory(Arc::clone(&memory)).await.ok()?;
        // §18.2: a session that ended without a close left its buffer on disk, and its turns are
        // claims nobody extracted. Picked up here, before the first turn, so a crash costs a delay
        // rather than a session. Failing is not fatal: an assistant with stale memory beats none.
        let extractor = ModelExtractor::new(provider.as_ref(), CancellationToken::new());
        let matcher = ModelMatcher::new(provider.as_ref(), CancellationToken::new());
        let _ = core
            .catch_up(
                &extractor,
                &matcher,
                &loki_core::memory::consolidate::Unbounded,
            )
            .await;
        Some(memory)
    });

    Box::into_raw(Box::new(LokiCore {
        runtime,
        core: Arc::new(AsyncMutex::new(core)),
        cancel: std::sync::Mutex::new(CancellationToken::new()),
        ledger,
        memory,
        recalled_slot: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        cited_slot: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        // The store is optional for the same reason memory is: a build that cannot open it is an
        // assistant whose citations have no icons, not one that will not start.
        evidence: loki_core::memory::evidence::Evidence::default_location()
            .ok()
            .map(std::sync::Arc::new),
        clock,
        journal,
    }))
}

/// Destroys a core. Accepts null.
///
/// # Safety
/// `core` must be null or a pointer from [`loki_core_new`] that has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_core_free(core: *mut LokiCore) {
    if core.is_null() {
        return;
    }
    // SAFETY: the caller contract above guarantees this came from `Box::into_raw`.
    drop(unsafe { Box::from_raw(core) });
}

/// Starts a turn and returns immediately.
///
/// Output arrives on the token callback, progress on the event callback. A failure ends as a
/// `task_finished` event with status `failed`, not as a return value.
///
/// # Safety
/// Both pointers must be valid. `text` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_send_message(core: *mut LokiCore, text: *const c_char) -> LokiStatus {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    // SAFETY: contract above.
    let Some(text) = (unsafe { as_str(text) }) else {
        return LokiStatus::InvalidArgument;
    };

    let Ok(cancel) = core.cancel.lock() else {
        return LokiStatus::NotReady;
    };
    let cancel = cancel.clone();
    let text = text.to_owned();
    let handle = Arc::clone(&core.core);

    // The rails read these after the turn has ended, so they are copied out of the loop while it
    // is still holding them. Without this `loki_recalled` returns an empty list forever, which is
    // what it did from the day it was written (B-73).
    let recalled_slot = std::sync::Arc::clone(&core.recalled_slot);
    let cited_slot = std::sync::Arc::clone(&core.cited_slot);
    core.runtime.spawn(async move {
        let mut core = handle.lock().await;
        let _ = core.turn_with(text, cancel).await;
        if let Ok(mut held) = recalled_slot.lock() {
            *held = core.last_recalled().to_vec();
        }
        if let Ok(mut held) = cited_slot.lock() {
            *held = core.last_cited().to_vec();
        }
    });

    LokiStatus::Ok
}

/// Signals the running turn to stop.
///
/// Tools stop at their next await point, guards drop, and an `interrupted` event lands. A fresh
/// token is installed so the next message is not cancelled before it starts.
///
/// # Safety
/// `core` must be a valid pointer from [`loki_core_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_interrupt(core: *mut LokiCore) -> LokiStatus {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    let Ok(mut cancel) = core.cancel.lock() else {
        return LokiStatus::NotReady;
    };
    cancel.cancel();
    *cancel = CancellationToken::new();
    LokiStatus::Ok
}

/// Adds a standing instruction, which compaction can never remove.
///
/// # Safety
/// Both pointers must be valid. `text` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_add_standing(
    core: *mut LokiCore,
    text: *const c_char,
    persistent: bool,
) -> LokiStatus {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    // SAFETY: contract above.
    let Some(text) = (unsafe { as_str(text) }) else {
        return LokiStatus::InvalidArgument;
    };

    let instruction = if persistent {
        Standing::persistent(text)
    } else {
        Standing::session(text)
    };

    let handle = Arc::clone(&core.core);
    core.runtime.block_on(async move {
        handle.lock().await.add_standing(instruction);
    });
    LokiStatus::Ok
}

/// Spend today, in millionths of a cent. Returns 0 if the ledger is unavailable.
///
/// Micro-cents rather than cents because one call costs a fraction of a cent, and the interface
/// should be able to show a running figure rather than a long run of zeroes.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_spend_today(core: *mut LokiCore) -> u64 {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    core.ledger
        .as_ref()
        .and_then(|l| l.spent_today().ok())
        .unwrap_or(0)
}

/// Spend this calendar month, in millionths of a cent. What the ceiling is measured against.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_spend_month(core: *mut LokiCore) -> u64 {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    core.ledger
        .as_ref()
        .and_then(|l| l.spent_this_month().ok())
        .unwrap_or(0)
}

/// Approves or rejects a Tier 3 action. Requires the tool registry, which is Phase 4.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_confirm_action(
    _core: *mut LokiCore,
    _action: u64,
    _approved: bool,
) -> LokiStatus {
    LokiStatus::Unsupported
}

/// Reverses a journaled action. Requires the undo journal, which is Phase 4.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_undo_action(_core: *mut LokiCore, _action: u64) -> LokiStatus {
    LokiStatus::Unsupported
}

/// Picks a winner between two conflicting claims (§9.7 rule 4, §9.8's one tap).
///
/// The store deliberately refuses to guess when two stated claims collide, so this is the only
/// thing that resolves one. Without it a surfaced conflict keeps the concept out of use forever.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. `concept` must be a valid C
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_resolve_conflict(
    core: *mut LokiCore,
    concept: *const c_char,
    keep: u32,
) -> LokiStatus {
    // SAFETY: contract above.
    unsafe {
        edit_claim(core, concept, |memory, path, today| async move {
            memory.settle(&path, keep, today).await
        })
    }
}

/// Replaces what a claim says, on the user's word (§17.3's edit).
///
/// A supersession, not an overwrite: the old wording keeps its window so the timeline can still
/// show it.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. `concept` and `text` must be
/// valid C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_amend_claim(
    core: *mut LokiCore,
    concept: *const c_char,
    ordinal: u32,
    text: *const c_char,
) -> LokiStatus {
    // SAFETY: contract above.
    let Some(text) = (unsafe { as_str(text) }) else {
        return LokiStatus::InvalidArgument;
    };
    let text = text.to_owned();
    // SAFETY: contract above.
    unsafe {
        edit_claim(core, concept, |memory, path, today| async move {
            memory.amend(&path, ordinal, &text, today).await
        })
    }
}

/// Retires a claim on the user's word, with nothing in its place (§17.3's delete).
///
/// Retired, never removed. A store that deletes on a tap cannot show what it used to think.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. `concept` must be a valid C
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_forget_claim(
    core: *mut LokiCore,
    concept: *const c_char,
    ordinal: u32,
) -> LokiStatus {
    // SAFETY: contract above.
    unsafe {
        edit_claim(core, concept, |memory, path, today| async move {
            memory.forget(&path, ordinal, today).await
        })
    }
}

/// Folds one entity card into another (§9.4).
///
/// Never called by the core itself. A wrong merge silently hides a true fact while a split leaves
/// two visible rows, so this only ever runs because a person looked at both and said yes.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. `from` and `into` must be valid
/// C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_merge_entities(
    core: *mut LokiCore,
    from: *const c_char,
    into: *const c_char,
) -> LokiStatus {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    // SAFETY: contract above.
    let (Some(from), Some(into)) = (unsafe { as_str(from) }, unsafe { as_str(into) }) else {
        return LokiStatus::InvalidArgument;
    };
    let (from, into) = (from.to_owned(), into.to_owned());
    let Some(memory) = core.memory.as_ref() else {
        return LokiStatus::InvalidArgument;
    };
    let memory = Arc::clone(memory);
    let today = core.clock.today();
    core.runtime.block_on(async move {
        match memory.merge(&from, &into, today).await {
            Ok(()) => LokiStatus::Ok,
            Err(_) => LokiStatus::NotReady,
        }
    })
}

/// Drops one of the other names an entity answers to (§17.3).
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Both strings must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_forget_alias(
    core: *mut LokiCore,
    concept: *const c_char,
    form: *const c_char,
) -> LokiStatus {
    // SAFETY: contract above.
    let Some(form) = (unsafe { as_str(form) }) else {
        return LokiStatus::InvalidArgument;
    };
    let form = form.to_owned();
    // SAFETY: contract above.
    unsafe {
        edit_claim(core, concept, |memory, path, today| async move {
            memory.forget_alias(&path, &form, today).await
        })
    }
}

/// Closes an edge, on the user's word. Closed rather than deleted: it was true until now.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Every string must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_forget_relation(
    core: *mut LokiCore,
    concept: *const c_char,
    label: *const c_char,
    to: *const c_char,
) -> LokiStatus {
    // SAFETY: contract above.
    let (Some(label), Some(to)) = (unsafe { as_str(label) }, unsafe { as_str(to) }) else {
        return LokiStatus::InvalidArgument;
    };
    let (label, to) = (label.to_owned(), to.to_owned());
    // SAFETY: contract above.
    unsafe {
        edit_claim(core, concept, |memory, path, today| async move {
            memory.forget_relation(&path, &label, &to, today).await
        })
    }
}

/// The three hand edits share a shape: check the pointers, run on the runtime, map the error.
///
/// # Safety
/// `core` must be null or valid, `concept` must be null or a valid C string.
unsafe fn edit_claim<F, Fut>(core: *mut LokiCore, concept: *const c_char, run: F) -> LokiStatus
where
    F: FnOnce(Arc<loki_core::memory::handle::Memory>, String, jiff::civil::Date) -> Fut,
    Fut: std::future::Future<Output = Result<(), loki_core::memory::handle::MemoryError>>,
{
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    // SAFETY: contract above.
    let Some(path) = (unsafe { as_str(concept) }) else {
        return LokiStatus::InvalidArgument;
    };
    let Some(memory) = core.memory.as_ref() else {
        return LokiStatus::InvalidArgument;
    };
    let memory = Arc::clone(memory);
    let path = path.to_owned();
    let today = core.clock.today();
    core.runtime.block_on(async move {
        match run(memory, path, today).await {
            Ok(()) => LokiStatus::Ok,
            Err(_) => LokiStatus::NotReady,
        }
    })
}

/// Grants a tool a set of capabilities. Requires the tool registry, which is Phase 4.
///
/// # Safety
/// All pointers must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_set_grant(
    _core: *mut LokiCore,
    _tool: *const c_char,
    _capabilities_json: *const c_char,
) -> LokiStatus {
    LokiStatus::Unsupported
}

/// Starts a connector's authorization flow. Requires connectors, which are Phase 4.
///
/// # Safety
/// All pointers must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_connect(
    _core: *mut LokiCore,
    _connector: *const c_char,
) -> LokiStatus {
    LokiStatus::Unsupported
}

/// Revokes a connector and forgets its credentials. Requires connectors, which are Phase 4.
///
/// # Safety
/// All pointers must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_disconnect(
    _core: *mut LokiCore,
    _connector: *const c_char,
) -> LokiStatus {
    LokiStatus::Unsupported
}

/// Returns the core version.
///
/// # Safety
/// The caller owns the returned pointer and must release it with [`loki_string_free`].
#[unsafe(no_mangle)]
pub extern "C" fn loki_version() -> *mut c_char {
    CString::new(loki_core::VERSION).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Releases a string returned by this library. Accepts null.
///
/// # Safety
/// `ptr` must be null, or a pointer returned by this library and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller contract above guarantees this came from `CString::into_raw`.
    drop(unsafe { CString::from_raw(ptr) });
}

/// What pre-fetch surfaced for the last turn, as JSON, for the `In play` rail.
///
/// A rail that shows what memory contributed is how the user can tell precision is working, and
/// it is where `not true` lives. Returns `[]` when memory is off or nothing was recalled.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
/// What pre-fetch surfaced for the last turn, as JSON, for the `In play` rail.
///
/// A rail that shows what memory contributed is how the user can tell precision is working, and
/// it is where `not true` lives. Returns `[]` when memory is off or nothing was recalled.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
/// What the last turn cited, as JSON (§12.7).
///
/// The icon travels as base64 rather than a path, because the interface has no business reading the
/// evidence store and a path it could read would be a second way in. Absent when the site offered
/// none or it could not be fetched, and the interface falls back to a letter (§9.4 of the design).
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_cited(core: *mut LokiCore) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("[]");
    };
    let cited = core
        .cited_slot
        .lock()
        .map(|held| held.clone())
        .unwrap_or_default();
    let rows: Vec<_> = cited
        .iter()
        .enumerate()
        .map(|(at, source)| {
            let icon = source
                .icon_hash
                .as_ref()
                .zip(core.evidence.as_ref())
                .and_then(|(hash, store)| {
                    store.get(&loki_core::core::ids::ContentHash::new(hash.clone()))
                })
                .map(|bytes| base64(&bytes));
            serde_json::json!({
                "id": at + 1,
                "url": source.url,
                "title": source.title,
                "excerpt": source.text.chars().take(280).collect::<String>(),
                "icon": icon,
                "read": source.read,
            })
        })
        .collect();
    json_string(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_owned()))
}

/// Standard base64, for handing bytes to Swift through a C string.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..group.len()].copy_from_slice(group);
        let packed = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for slot in 0..4 {
            if slot <= group.len() {
                out.push(ALPHABET[((packed >> (18 - 6 * slot)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What pre-fetch surfaced for the last turn, as JSON, for the `In play` rail.
///
/// A rail that shows what memory contributed is how the user can tell precision is working, and
/// it is where `not true` lives. Returns `[]` when memory is off or nothing was recalled.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_recalled(core: *mut LokiCore) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("[]");
    };
    let recalled = core
        .recalled_slot
        .lock()
        .map(|held| held.clone())
        .unwrap_or_default();
    let rows: Vec<_> = recalled
        .iter()
        .map(|r| {
            serde_json::json!({
                "path": r.path,
                "name": r.name,
                "text": r.text,
                "ordinal": r.ordinal,
                "score": r.score.value(),
                "fromSession": r.layer == loki_core::memory::index::Layer::Live,
            })
        })
        .collect();
    json_string(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_owned()))
}

/// Marks a recalled claim wrong (§9.9, and the rail's one-click `not true`).
///
/// Drops its confidence and flags it, rather than deleting. Nothing is removed by a single tap:
/// the timeline is where a claim is actually edited or deleted.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. `path` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_not_true(
    core: *mut LokiCore,
    path: *const c_char,
    ordinal: u32,
) -> LokiStatus {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return LokiStatus::InvalidArgument;
    };
    // SAFETY: contract above.
    let Some(path) = (unsafe { as_str(path) }) else {
        return LokiStatus::InvalidArgument;
    };
    let Some(memory) = core.memory.as_ref() else {
        return LokiStatus::InvalidArgument;
    };
    let memory = Arc::clone(memory);
    let path = path.to_owned();
    core.runtime.block_on(async move {
        match memory.contradict(&path, ordinal).await {
            Ok(()) => LokiStatus::Ok,
            Err(_) => LokiStatus::NotReady,
        }
    })
}

/// The memory timeline, newest first, as JSON (§17.3).
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_timeline(core: *mut LokiCore, limit: u32) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("[]");
    };
    let Some(memory) = core.memory.as_ref() else {
        return json_string("[]");
    };
    let memory = Arc::clone(memory);
    let text = core
        .runtime
        .block_on(async move { memory.timeline(limit as usize).await.unwrap_or_default() });
    json_string(&serde_json::to_string(&text).unwrap_or_else(|_| "[]".to_owned()))
}

/// What Loki knows, grouped by entity, as JSON (§17.3).
///
/// The trust surface reads this rather than `log.md`: a log answers what changed, and the screen
/// has to answer what Loki thinks it knows. Every row still names the file it came from.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_knowledge(core: *mut LokiCore) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("{\"entities\":[]}");
    };
    let Some(memory) = core.memory.as_ref() else {
        return json_string("{\"entities\":[]}");
    };
    let memory = Arc::clone(memory);
    let today = core.clock.today();
    let knowledge = core
        .runtime
        .block_on(async move { memory.knowledge(today).await.unwrap_or_default() });
    json_string(
        &serde_json::to_string(&knowledge).unwrap_or_else(|_| "{\"entities\":[]}".to_owned()),
    )
}

/// What this session has spent in tokens, as JSON (§21.3).
///
/// Per session, not per day: the ledger already answers the day and the month, and the number that
/// matters for consolidation health is how big the prompt has grown while you have been talking.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_session_tokens(core: *mut LokiCore) -> *mut c_char {
    const EMPTY: &str = r#"{"input":0,"output":0,"context":0,"calls":0}"#;
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string(EMPTY);
    };
    json_string(&serde_json::to_string(&core.journal.tokens()).unwrap_or_else(|_| EMPTY.to_owned()))
}

/// Where the session transcript is written, so the interface can point at it.
///
/// # Safety
/// Free the result with [`loki_string_free`].
#[unsafe(no_mangle)]
pub extern "C" fn loki_journal_path() -> *mut c_char {
    let path = loki_core::paths::journal()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    CString::new(path).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Past sessions, newest first, as JSON, for the sidebar (§9.1 of the design system).
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_sessions(core: *mut LokiCore, limit: u32) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("[]");
    };
    let Some(memory) = core.memory.as_ref() else {
        return json_string("[]");
    };
    let memory = Arc::clone(memory);
    let rows = core
        .runtime
        .block_on(async move { memory.sessions(limit as usize).await.unwrap_or_default() });
    json_string(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_owned()))
}

/// Closes the session and returns the summary lines as JSON (§17.4).
///
/// Up to three lines, and an empty array when nothing happened. Silence is the design: a card
/// saying "learned nothing today" teaches people to ignore the card.
///
/// # Safety
/// `core` must be null or a valid pointer from [`loki_core_new`]. Free the result with
/// [`loki_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_end_session(core: *mut LokiCore) -> *mut c_char {
    // SAFETY: contract above.
    let Some(core) = (unsafe { core.as_ref() }) else {
        return json_string("[]");
    };
    let Some(memory) = core.memory.as_ref() else {
        return json_string("[]");
    };
    let memory = Arc::clone(memory);
    let loop_handle = Arc::clone(&core.core);
    // The session's totals go in before the consolidation lines, so the transcript ends with a
    // summary of what the session cost rather than with the last thing it learned.
    core.journal.totals();
    let lines = core.runtime.block_on(async move {
        let cancel = CancellationToken::new();
        // Bounded here rather than by the caller (B-48).
        //
        // This is a blocking call across the bridge, so a `Task.cancel()` on the Swift side cannot
        // reach it: the app looked like it gave consolidation twenty seconds and in fact waited
        // for ever, which is why quitting stopped working as the pass got heavier. A timeout
        // around the await points is a bound that actually holds, and the cancellation token
        // stops the model call it was waiting on rather than leaving it running.
        //
        // Losing one session's consolidation costs a re-derivation. The episode is still on disk
        // and §18.2 picks it up on the next launch.
        let work = consolidate_now(&memory, &loop_handle, cancel.clone());
        let Ok(lines) = tokio::time::timeout(CLOSE_BUDGET, work).await else {
            cancel.cancel();
            return Vec::new();
        };
        lines
    });
    json_string(&serde_json::to_string(&lines).unwrap_or_else(|_| "[]".to_owned()))
}

/// How long quitting will wait for consolidation before giving up on it.
///
/// A person pressing cmd-Q has said what they want. Consolidation is worth a pause and is not
/// worth an app that will not close.
const CLOSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

async fn consolidate_now(
    memory: &Arc<Memory>,
    loop_handle: &Arc<AsyncMutex<Loop>>,
    cancel: CancellationToken,
) -> Vec<String> {
    {
        let guard = loop_handle.lock().await;
        let provider = guard.provider();
        let extractor = ModelExtractor::new(provider.as_ref(), cancel.clone());
        let matcher = ModelMatcher::new(provider.as_ref(), cancel);
        let today = jiff::Zoned::now().date();
        let Ok(report) = memory
            .close(
                &extractor,
                &matcher,
                &loki_core::memory::consolidate::Unbounded,
                today,
            )
            .await
        else {
            return Vec::new();
        };
        drop(guard);
        let rows = memory.timeline_rows(&report, today).await;

        // §8.1's one accepted cache miss: what was just learned has to be usable on the very next
        // turn, not after a relaunch.
        if !rows.is_empty() {
            let mut guard = loop_handle.lock().await;
            let _ = guard.refresh_working_set().await;
        }
        loki_core::memory::timeline::summary(&rows, report.rejected.as_deref())
    }
}

/// Hands a string to Swift. Null on an interior nul byte, which the caller treats as empty.
fn json_string(text: &str) -> *mut c_char {
    CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Opens the memory store for this session.
///
/// Returns `None` rather than an error: every failure here has the same remedy, which is to carry
/// on without recall, and a store that cannot be opened must not stop the app from answering.
async fn open_memory(events: Arc<dyn EventSink>) -> Option<Arc<Memory>> {
    let root = loki_core::paths::memory().ok()?;
    let index = loki_core::paths::index()
        .ok()
        .and_then(|path| Index::open(&path).ok())
        .or_else(|| Index::in_memory().ok())?;
    let now = jiff::Zoned::now();
    // The session id is the moment it started, which is unique per launch and sorts.
    let session = now.strftime("%Y-%m-%dT%H-%M-%S").to_string();
    Memory::open(
        &root,
        index,
        session,
        now.date(),
        TierScope::normal(Locality::Cloud),
        events,
    )
    .await
    .ok()
    .map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static EVENTS: AtomicUsize = AtomicUsize::new(0);
    static TOKENS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn count_event(json: *const c_char, _user: *mut c_void) {
        assert!(!json.is_null());
        EVENTS.fetch_add(1, Ordering::Relaxed);
    }

    extern "C" fn count_token(text: *const c_char, _user: *mut c_void) {
        assert!(!text.is_null());
        TOKENS.fetch_add(1, Ordering::Relaxed);
    }

    fn new_core() -> *mut LokiCore {
        let key = CString::new("test-key").unwrap();
        // SAFETY: the key is valid for this call and user_data is null, which the callbacks ignore.
        unsafe {
            loki_core_new(
                LokiProvider::Anthropic,
                key.as_ptr(),
                std::ptr::null(),
                count_event,
                count_token,
                std::ptr::null_mut(),
            )
        }
    }

    #[test]
    fn version_round_trips_and_frees() {
        let ptr = loki_version();
        assert!(!ptr.is_null());
        // SAFETY: `ptr` came from `loki_version`.
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert_eq!(s, loki_core::VERSION);
        // SAFETY: `ptr` is ours and not yet freed.
        unsafe { loki_string_free(ptr) };
    }

    #[test]
    fn free_tolerates_null() {
        // SAFETY: null is explicitly allowed.
        unsafe {
            loki_string_free(std::ptr::null_mut());
            loki_core_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn a_null_key_is_rejected_rather_than_dereferenced() {
        // SAFETY: null is explicitly allowed and must be rejected.
        let core = unsafe {
            loki_core_new(
                LokiProvider::Anthropic,
                std::ptr::null(),
                std::ptr::null(),
                count_event,
                count_token,
                std::ptr::null_mut(),
            )
        };
        assert!(core.is_null());
    }

    #[test]
    fn a_named_model_is_accepted() {
        let key = CString::new("test-key").unwrap();
        let model = CString::new("gpt-5-mini").unwrap();
        // SAFETY: both strings are valid for this call.
        let core = unsafe {
            loki_core_new(
                LokiProvider::Openai,
                key.as_ptr(),
                model.as_ptr(),
                count_event,
                count_token,
                std::ptr::null_mut(),
            )
        };
        assert!(!core.is_null());
        // SAFETY: `core` is live and not yet freed.
        unsafe { loki_core_free(core) };
    }

    #[test]
    fn calls_on_a_null_core_return_invalid_argument() {
        let text = CString::new("hello").unwrap();
        // SAFETY: null is explicitly allowed and must be rejected.
        unsafe {
            assert_eq!(
                loki_send_message(std::ptr::null_mut(), text.as_ptr()),
                LokiStatus::InvalidArgument
            );
            assert_eq!(
                loki_interrupt(std::ptr::null_mut()),
                LokiStatus::InvalidArgument
            );
        }
    }

    #[test]
    fn a_core_can_be_created_used_and_freed() {
        let core = new_core();
        assert!(!core.is_null());

        let text = CString::new("Be brief.").unwrap();
        // SAFETY: `core` is live and the string is valid.
        unsafe {
            assert_eq!(loki_add_standing(core, text.as_ptr(), true), LokiStatus::Ok);
            assert_eq!(loki_interrupt(core), LokiStatus::Ok);
            loki_core_free(core);
        }
    }

    #[test]
    fn spend_queries_are_safe_on_a_null_core() {
        // SAFETY: null is explicitly allowed and must return zero rather than crash.
        unsafe {
            assert_eq!(loki_spend_today(std::ptr::null_mut()), 0);
            assert_eq!(loki_spend_month(std::ptr::null_mut()), 0);
        }
    }

    #[test]
    fn unbuilt_subsystems_report_unsupported_rather_than_lying() {
        let core = new_core();
        let name = CString::new("google").unwrap();
        // SAFETY: `core` is live and the string is valid.
        unsafe {
            assert_eq!(loki_confirm_action(core, 0, true), LokiStatus::Unsupported);
            assert_eq!(loki_undo_action(core, 0), LokiStatus::Unsupported);
            assert_eq!(loki_connect(core, name.as_ptr()), LokiStatus::Unsupported);
            assert_eq!(
                loki_disconnect(core, name.as_ptr()),
                LokiStatus::Unsupported
            );
            loki_core_free(core);
        }
    }

    #[test]
    fn a_turn_reaches_the_event_callback() {
        EVENTS.store(0, Ordering::Relaxed);
        let core = new_core();
        let text = CString::new("hello").unwrap();

        // SAFETY: `core` is live and the string is valid.
        unsafe {
            assert_eq!(loki_send_message(core, text.as_ptr()), LokiStatus::Ok);
        }

        // The turn runs on a worker thread and fails on the bad key, which still emits events.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            EVENTS.load(Ordering::Relaxed) > 0,
            "no events reached Swift"
        );

        // SAFETY: `core` is live and not yet freed.
        unsafe { loki_core_free(core) };
    }
}
