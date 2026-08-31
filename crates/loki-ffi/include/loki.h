/*
 * Loki core, C ABI.
 *
 * Hand-written while the surface is small. Move to cbindgen if it outgrows what stays readable.
 * Mirrors crates/loki-ffi/src/lib.rs. Keep the two in sync.
 */

#ifndef LOKI_H
#define LOKI_H

#ifdef __cplusplus
extern "C" {
#endif

/* Returns the core version. Caller owns the pointer and must pass it to loki_string_free. */
char *loki_version(void);

/* Releases a string returned by this library. Accepts NULL. Never free twice. */
void loki_string_free(char *ptr);

#ifdef __cplusplus
}
#endif

#endif /* LOKI_H */
