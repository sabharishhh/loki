//! Dev harness.
//!
//! Exists so the core can be driven and tested without building the Mac app. Grows into a
//! read-eval loop over `loki_core` as Phase 1 lands.

fn main() {
    println!("loki-core {}", loki_core::VERSION);
}
