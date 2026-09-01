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
use loki_core::adapters::openai::Openai;
use loki_core::core::budget::Budget;
use loki_core::core::cycle::{Loop, TokenSink};
use loki_core::core::event::Event;
use loki_core::core::ledger::Ledger;
use loki_core::core::prompt::{Prefix, Standing};
use loki_core::core::sink::{Broadcast, EventSink};
use loki_core::core::vocab::Cents;
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

    let provider: Arc<dyn ModelProvider> = match provider {
        LokiProvider::Anthropic => match Anthropic::new(api_key) {
            Ok(p) => Arc::new(match model {
                Some(m) => p.with_model(m),
                None => p,
            }),
            Err(_) => return std::ptr::null_mut(),
        },
        LokiProvider::Openai => match Openai::new(api_key) {
            Ok(p) => Arc::new(match model {
                Some(m) => p.with_model(m),
                None => p,
            }),
            Err(_) => return std::ptr::null_mut(),
        },
    };

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

    let core = Loop::new(
        provider,
        Arc::new(events),
        callbacks as Arc<dyn TokenSink>,
        Prefix::new(SYSTEM),
        Budget::resuming(DEFAULT_CEILING, spent),
    );

    Box::into_raw(Box::new(LokiCore {
        runtime,
        core: Arc::new(AsyncMutex::new(core)),
        cancel: std::sync::Mutex::new(CancellationToken::new()),
        ledger,
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

    core.runtime.spawn(async move {
        let mut core = handle.lock().await;
        let _ = core.turn_with(text, cancel).await;
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

/// Picks a winner between two conflicting claims. Requires memory, which is Phase 2.
///
/// # Safety
/// All pointers must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_resolve_conflict(
    _core: *mut LokiCore,
    _concept: *const c_char,
    _keep: u32,
) -> LokiStatus {
    LokiStatus::Unsupported
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
