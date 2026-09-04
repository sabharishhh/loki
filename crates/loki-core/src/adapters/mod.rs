//! Ring 2. Free to add.
//!
//! An adapter does not know another adapter exists. Everything routes through Ring 1, which is
//! what stops one broken adapter taking the system down.
//!
//! Built so far: [`anthropic`], [`openai`], both streaming SSE through the shared [`sse`] helper
//! and both sending through [`egress`], plus [`clock`] and [`journal`].
//!
//! [`egress`] holds the only `reqwest::Client` in the tree, and `tests/rings.rs` fails if a second
//! one appears (§21.7).

pub mod anthropic;
pub mod clock;
pub mod egress;
pub mod journal;
pub mod openai;
pub mod pricing;
pub mod sse;
