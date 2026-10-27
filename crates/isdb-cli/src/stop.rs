//! SIGINT/SIGTERM handling: a process-wide stop flag polled by the loops.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// The process-wide stop flag (set by SIGINT/SIGTERM/SIGHUP).
pub fn flag() -> Arc<AtomicBool> {
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// Install the signal handler.  A second signal only repeats the notice:
/// the process never exits without stopping the radio first.
pub fn install() -> anyhow::Result<()> {
    let f = flag();
    ctrlc::set_handler(move || {
        if f.swap(true, Ordering::SeqCst) {
            info!(target: "isdb", "still stopping (waiting for the device to stop streaming)...");
        } else {
            info!(target: "isdb", "stop requested");
        }
    })?;
    Ok(())
}
