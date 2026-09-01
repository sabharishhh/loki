//! Loki core.
//!
//! Ring structure, from `.agent/ARCHITECTURE.md`:
//!
//! - [`core`] is Ring 0, locked. The loop, locking discipline, cancellation, RAII guards, the
//!   event stream, the typestate gate, the two-zone prompt, tier assignment, the undo journal.
//! - [`ports`] is Ring 1, versioned. Interfaces the core defines. A change here needs a version
//!   bump and a migration note, never a silent edit.
//! - [`adapters`] is Ring 2, free to add. Implementations of ports. Ring 2 never talks to Ring 2.
//!
//! The dependency rule runs one way. `adapters` depends on `ports`. `core` depends on `ports`.
//! Nothing depends on `adapters`, which `tests/rings.rs` checks rather than trusting.

pub mod adapters;
pub mod core;
pub mod error;
pub mod memory;
pub mod paths;
pub mod ports;
pub mod runtime;

pub use error::Error;

/// Crate version, surfaced across the bridge so the app can prove the core is linked.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
