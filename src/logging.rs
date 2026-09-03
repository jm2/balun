//! Process-wide diagnostic logging to standard error.
//!
//! Both binaries call [`init`] first thing. `RUST_LOG` selects what is
//! written, as in Tributary; without it Balun logs its own crate at `info`.
//! Log lines carry only closed categories, GStreamer's native error domain,
//! code, and text, HTTP status codes, and the device names and addresses
//! ADR-0002 allows. Stream URLs, `DeviceAuth`, and query values never reach a
//! log line because no logged type carries them.

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
