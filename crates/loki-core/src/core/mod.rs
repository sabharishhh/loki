//! Ring 0. Locked.
//!
//! Internals here can be rewritten freely as long as Ring 1 holds. What is locked is the
//! structure and the contracts, not the tuning.
//!
//! Built so far:
//!
//! - [`ids`] holds the identifier newtypes the event stream uses.
//!
//! Planned, not built yet:
//!
//! - `cycle` is the fixed nine-step loop.
//! - `event` is the one event stream every consumer reads.
//! - `prompt` is the two-zone prompt, frozen prefix and turn content.
//! - `cancel` is cooperative cancellation, one token per in-flight tool.
//! - `tier` is host-assigned reversibility tiers.
//! - `undo` is the persistent undo journal.
//! - `checkpoint` is session-scoped resume points.

pub mod ids;
