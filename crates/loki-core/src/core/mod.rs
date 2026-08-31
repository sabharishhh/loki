//! Ring 0. Locked.
//!
//! Internals here can be rewritten freely as long as Ring 1 holds. What is locked is the
//! structure and the contracts, not the tuning.
//!
//! Built so far:
//!
//! - [`ids`] holds the identifier newtypes the event stream uses.
//! - [`vocab`] holds the small closed sets the events are described in.
//! - [`payload`] holds the tool call payloads events carry.
//! - [`event`] is the one event stream every consumer reads.
//! - [`sink`] is how consumers subscribe to it.
//! - [`render`] turns events into plain sentences or a dense trace.
//!
//! Planned, not built yet:
//!
//! - `cycle` is the fixed nine-step loop.
//! - `prompt` is the two-zone prompt, frozen prefix and turn content.
//! - `cancel` is cooperative cancellation, one token per in-flight tool.
//! - `tier` is host-assigned reversibility tiers.
//! - `undo` is the persistent undo journal.
//! - `checkpoint` is session-scoped resume points.

pub mod event;
pub mod ids;
pub mod payload;
pub mod render;
pub mod sink;
pub mod vocab;
