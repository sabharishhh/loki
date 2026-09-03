/*
 * Loki core, C ABI.
 *
 * Hand-written while the surface is small. Mirrors crates/loki-ffi/src/lib.rs; keep the two in
 * sync. Move to cbindgen if it outgrows what stays readable.
 *
 * Threading: loki_send_message returns immediately and the turn runs on a worker thread. Both
 * callbacks are invoked from that thread, not the caller's. Hop to the main queue before touching
 * UI state.
 */

#ifndef LOKI_H
#define LOKI_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LokiCore LokiCore;

typedef enum {
    LOKI_OK = 0,
    /* A pointer was null, or a string was not valid UTF-8. */
    LOKI_INVALID_ARGUMENT = 1,
    /* The core exists but cannot serve this call right now. */
    LOKI_NOT_READY = 2,
    /* The subsystem behind this call is not built yet. */
    LOKI_UNSUPPORTED = 3
} LokiStatus;

typedef enum {
    LOKI_PROVIDER_ANTHROPIC = 0,
    LOKI_PROVIDER_OPENAI = 1
} LokiProvider;

/* One serialized event as JSON. Valid for the duration of the call only. */
typedef void (*LokiEventCallback)(const char *json, void *user_data);

/* One chunk of response text. Valid for the duration of the call only. */
typedef void (*LokiTokenCallback)(const char *text, void *user_data);

/*
 * Creates a core. Returns NULL on failure.
 *
 * user_data is passed back to both callbacks unchanged and must stay valid, and safe to use from
 * any thread, until loki_core_free.
 *
 * model may be NULL, which uses the provider's default.
 *
 * api_key is an argument only until SecretStore lands. It is then read from the Keychain and this
 * parameter goes away.
 */
LokiCore *loki_core_new(LokiProvider provider,
                        const char *api_key,
                        const char *model,
                        LokiEventCallback event,
                        LokiTokenCallback token,
                        void *user_data);

/* Destroys a core. Accepts NULL. Never free twice. */
void loki_core_free(LokiCore *core);

/* Starts a turn and returns immediately. Output arrives on the callbacks. */
LokiStatus loki_send_message(LokiCore *core, const char *text);

/* Signals the running turn to stop. A fresh token is installed for the next message. */
LokiStatus loki_interrupt(LokiCore *core);

/* Adds a standing instruction, which compaction can never remove. */
LokiStatus loki_add_standing(LokiCore *core, const char *text, bool persistent);

/* Spend today and this month, in millionths of a cent. Zero if the ledger is unavailable. */
uint64_t loki_spend_today(LokiCore *core);
uint64_t loki_spend_month(LokiCore *core);

/* Memory. Every char* result must be freed with loki_string_free. */

/* What pre-fetch surfaced for the last turn, as a JSON array. "[]" when memory is off. */
char *loki_recalled(LokiCore *core);

/* Marks a recalled claim wrong. Drops its confidence; deletes nothing. */
LokiStatus loki_not_true(LokiCore *core, const char *path, uint32_t ordinal);

/* The memory timeline, newest first, as a JSON array of sentences. */
char *loki_timeline(LokiCore *core, uint32_t limit);

/* What Loki knows, grouped by entity, as JSON. The trust surface reads this. */
char *loki_knowledge(LokiCore *core);

/* Replaces what a claim says, on the user's word. A supersession, not an overwrite. */
LokiStatus loki_amend_claim(LokiCore *core, const char *concept, uint32_t ordinal,
                            const char *text);

/* Retires a claim with nothing in its place. Retired, never removed. */
LokiStatus loki_forget_claim(LokiCore *core, const char *concept, uint32_t ordinal);

/* What Loki knows, grouped by entity, as JSON. The trust surface reads this. */
char *loki_knowledge(LokiCore *core);

/* Replaces what a claim says, on the user's word. A supersession, not an overwrite. */
LokiStatus loki_amend_claim(LokiCore *core, const char *concept, uint32_t ordinal,
                            const char *text);

/* Retires a claim with nothing in its place. Retired, never removed. */
LokiStatus loki_forget_claim(LokiCore *core, const char *concept, uint32_t ordinal);

/* Past sessions, newest first, as a JSON array of day strings. */
char *loki_sessions(LokiCore *core, uint32_t limit);

/* This session's token spend as JSON: input, output, context, calls. */
char *loki_session_tokens(LokiCore *core);

/* Where the session transcript is written. */
char *loki_journal_path(void);

/* Consolidates the session and returns up to three summary lines as a JSON array. */
char *loki_end_session(LokiCore *core);

/* Phase 4. Returns LOKI_UNSUPPORTED until the tool registry exists. */
LokiStatus loki_confirm_action(LokiCore *core, uint64_t action, bool approved);

/* Phase 4. Returns LOKI_UNSUPPORTED until the undo journal exists. */
LokiStatus loki_undo_action(LokiCore *core, uint64_t action);

/* Confirms which side of a conflict is right. Keeps the claim at `keep`, retires its rivals
   on the same attribute, and marks the concept confirmed by a person. */
LokiStatus loki_resolve_conflict(LokiCore *core, const char *concept, uint32_t keep);

/* Phase 4. Returns LOKI_UNSUPPORTED until the tool registry exists. */
LokiStatus loki_set_grant(LokiCore *core, const char *tool, const char *capabilities_json);

/* Phase 4. Returns LOKI_UNSUPPORTED until connectors exist. */
LokiStatus loki_connect(LokiCore *core, const char *connector);

/* Phase 4. Returns LOKI_UNSUPPORTED until connectors exist. */
LokiStatus loki_disconnect(LokiCore *core, const char *connector);

/* Returns the core version. Caller owns the pointer and must pass it to loki_string_free. */
char *loki_version(void);

/* Releases a string returned by this library. Accepts NULL. Never free twice. */
void loki_string_free(char *ptr);

#ifdef __cplusplus
}
#endif

#endif /* LOKI_H */
