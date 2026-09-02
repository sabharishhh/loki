//! Ring 2. Free to add.
//!
//! An adapter does not know another adapter exists. Everything routes through Ring 1, which is
//! what stops one broken adapter taking the system down.
//!
//! Built so far: [`anthropic`], [`openai`], both streaming SSE through the shared [`sse`] helper,
//! and [`clock`].

pub mod anthropic;
pub mod clock;
pub mod openai;
pub mod pricing;
pub mod sse;
