//! Process-wide diagnostic logging to standard error.
//!
//! Both binaries call [`init`] first thing. `RUST_LOG` selects what is
//! written, as in Tributary; without it Balun logs its own crate at `info`.
//! Native playback diagnostics retain closed labels and typed counters;
//! plugin error text, debug strings, and arbitrary caps values are discarded.
//! Other Balun logs retain the device names and addresses ADR-0002 allows.
//! GStreamer's own opt-in `GST_DEBUG` output bypasses this subscriber.

use tracing_subscriber::EnvFilter;

/// The filter applied when `RUST_LOG` is unset or unparsable.
pub const DEFAULT_FILTER: &str = "balun=info";

/// Install the standard-error subscriber once; later calls are no-ops.
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init();
        init();
    }
}
