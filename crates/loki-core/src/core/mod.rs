//! Ring 0. Locked.
//!
//! Internals here can be rewritten freely as long as Ring 1 holds. What is locked is the
//! structure and the contracts, not the tuning.
//!
//! Built so far:
//!
//! - [`attempt`] is the bounded loop the search and tool loops both run.
//! - [`ids`] holds the identifier newtypes the event stream uses.
//! - [`vocab`] holds the small closed sets the events are described in.
//! - [`payload`] holds the tool call payloads events carry.
//! - [`event`] is the one event stream every consumer reads.
//! - [`sink`] is how consumers subscribe to it.
//! - [`render`] turns events into plain sentences or a dense trace.
//! - [`prompt`] is the two-zone prompt, frozen prefix and turn content.
//! - [`budget`] is the spend ceiling, checked before a model call.
//! - [`ledger`] is the persistent spend record, fed by the event stream.
//! - [`checkpoint`] is session-scoped resume points.
//! - [`cycle`] is the fixed nine-step loop.
//!
//! Planned, not built yet:
//!
//! - `cancel` is cooperative cancellation, one token per in-flight tool.
//! - `tier` is host-assigned reversibility tiers.
//! - `undo` is the persistent undo journal.

pub mod attempt;
pub mod budget;
pub mod checkpoint;
pub mod cycle;
pub mod event;
pub mod ids;
pub mod ledger;
pub mod payload;
pub mod prompt;
pub mod render;
pub mod sink;
pub mod temporal;
pub mod trigger;
pub mod vocab;
pub mod websearch;
