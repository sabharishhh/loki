//! Ring 2. Free to add.
//!
//! An adapter does not know another adapter exists. Everything routes through Ring 1, which is
//! what stops one broken adapter taking the system down.
//!
//! Built so far: [`anthropic`], [`openai`]. Both stream SSE through the shared [`sse`] helper.

pub mod anthropic;
pub mod openai;
pub mod sse;
