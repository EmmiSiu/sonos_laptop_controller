//! Library half of the desktop shell.
//!
//! Tauri's mobile targets require a `lib` entry point; the desktop binary in `main.rs` is the
//! one that actually runs. Nothing of substance lives here — see `rincon-ipc` for the logic
//! and `main.rs` for the wiring.

/// Re-exported so the binary and any future mobile entry point share one definition.
pub use rincon_ipc::Api;
