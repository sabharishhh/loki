//! Ring 0. Locked.
//!
//! Internals here can be rewritten freely as long as Ring 1 holds. What is locked is the
//! structure and the contracts, not the tuning.
//!
//! Planned contents, none implemented yet:
//!
//! - `cycle` is the fixed nine-step loop.
//! - `event` is the one event stream every consumer reads.
//! - `prompt` is the two-zone prompt, frozen prefix and turn content.
//! - `cancel` is cooperative cancellation, one token per in-flight tool.
//! - `tier` is host-assigned reversibility tiers.
//! - `undo` is the persistent undo journal.
//! - `checkpoint` is session-scoped resume points.
