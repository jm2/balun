//! GTK-free core services for Balun.

// Windows network-change callbacks need the crate's only unsafe code; see
// `discovery::changes::windows`. Every other target forbids it outright.
#![cfg_attr(not(windows), forbid(unsafe_code))]

#[cfg(test)]
mod adversarial;

pub mod controller;
pub mod discovery;
pub mod domain;
pub mod hdhr;
pub mod localization;
pub mod logging;
#[cfg(feature = "playback")]
pub mod playback;
pub mod settings;

// rust-i18n's t! expansion looks up these generated helpers at the crate root.
use localization::catalog::{_rust_i18n_t, _rust_i18n_try_translate};
