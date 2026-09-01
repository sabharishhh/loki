//! The async runtime.
//!
//! Built here rather than by each caller, so the thread count and naming are one decision instead
//! of several. Tokio parks on kqueue, so an idle runtime costs nothing.

use crate::error::Error;

/// Worker threads.
///
/// The core is IO bound: it waits on a provider, on disk, on a subprocess. More workers than that
/// needs would be threads parked forever, and each one reserves a stack.
const WORKERS: usize = 4;

/// Builds the runtime the core runs on.
///
/// # Errors
/// Fails if the OS refuses to start the runtime's threads.
pub fn build() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .thread_name("loki-worker")
        .enable_all()
        .build()
        .map_err(Error::Runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_starts_and_runs_work() {
        let runtime = build().expect("runtime");
        assert_eq!(runtime.block_on(async { 1 + 1 }), 2);
    }

    #[test]
    fn timers_and_io_are_enabled() {
        let runtime = build().expect("runtime");
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                tokio::time::sleep(std::time::Duration::from_millis(1)),
            )
            .await
            .expect("timer driver missing");
        });
    }
}
