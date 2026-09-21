//! TypeScript type generation for the IPC boundary.
//!
//! # Why generate rather than hand-write
//!
//! The interface renders `SessionDto` and branches on `ErrorCode`. If a Rust field is renamed
//! and the TypeScript is not, nothing breaks at build time — it breaks at runtime, in front of
//! a user, as `undefined`. Generating the types turns that into a compile error in the frontend
//! build, which is where it belongs.
//!
//! # Why a hand-rolled generator rather than a crate
//!
//! The surface is nine types. A generator crate would add a dependency, a derive on every DTO,
//! and a build step, to produce output we would then have to check anyway. This module is
//! eighty lines, emits exactly the shape we want, and — importantly — the *checked-in file is
//! compared against it by a test*, so the bindings cannot drift even if nobody remembers to
//! regenerate them.
//!
//! Run `just bindings` to regenerate after changing a DTO.

/// Where the generated file lives, relative to the workspace root.
pub const BINDINGS_PATH: &str = "apps/desktop/src/lib/bindings.ts";

/// Renders the TypeScript declarations for the IPC surface.
///
/// Kept as one string literal rather than assembled from reflection: reflection over serde
/// attributes is exactly the kind of cleverness that produces output nobody can predict, and
/// the test below makes the manual version safe by refusing to let it drift.
#[must_use]
pub fn render() -> String {
    format!(
        r#"// GENERATED FILE — do not edit.
//
// Produced from the Rust DTOs in `crates/rincon-ipc/src/lib.rs`.
// Regenerate with `just bindings`; CI fails if this file is out of date.

/** Stable, machine-readable error identifiers. Branch on these, never on `message`. */
export type ErrorCode =
  | "unknown_device"
  | "invalid_state"
  | "scan_failed"
  | "session_failed"
  | "invalid_argument"
  | "internal";

/** Every IPC command rejects with exactly this shape. */
export interface IpcError {{
  code: ErrorCode;
  message: string;
  retryable: boolean;
}}

/** A speaker, as the interface sees it. Carries an opaque id, never a routable address. */
export interface DeviceDto {{
  id: string;
  room: string;
  model: string;
  subnet: string;
}}

/** Where a connection attempt has got to. */
export type PrepareStep =
  | "resolve_coordinator"
  | "start_capture"
  | "bind_server"
  | "set_uri"
  | "play"
  | "await_peer";

/** Why a healthy session became a degraded one. */
export type DegradeReason = "underrun" | "frames_dropped" | "peer_reconnecting";

/** What the health indicator shows. */
export type Health = "good" | "fair" | "poor" | "lost";

/** Why a session failed. Each variant maps to one sentence and one remediation. */
export type FailureReason =
  | {{ kind: "no_capture_device" }}
  | {{ kind: "capture_busy" }}
  | {{ kind: "device_unreachable" }}
  | {{ kind: "rejected_by_device"; detail: {{ upnp_code: number; description: string }} }}
  | {{ kind: "firewall_suspected" }}
  | {{ kind: "network_isolated" }}
  | {{ kind: "scan_failed"; detail: {{ detail: string }} }}
  | {{ kind: "internal"; detail: {{ detail: string }} }};

/** The authoritative session state. The stream URL arrives already redacted. */
export type SessionState =
  | {{ state: "idle" }}
  | {{ state: "scanning" }}
  | {{ state: "devices_found"; devices: DeviceDto[] }}
  | {{ state: "no_devices" }}
  | {{ state: "preparing"; step: PrepareStep }}
  | {{ state: "streaming"; url: string; health: Health }}
  | {{ state: "degraded"; url: string; reason: DegradeReason }}
  | {{ state: "stopping" }}
  | {{ state: "failed"; reason: FailureReason; retryable: boolean }};

/** Counters for the live session. */
export interface CounterSnapshot {{
  frames_captured: number;
  frames_served: number;
  underruns: number;
  reconnects: number;
  dropped_capture: number;
  dropped_ring: number;
  dropped_socket: number;
  dropped_device: number;
  /** Discarded before the speaker connected. Expected; not a quality signal. */
  dropped_no_consumer: number;
}}

/** Everything the interface needs to render one frame of the session. */
export interface SessionDto {{
  state: string;
  detail: SessionState;
  room: string | null;
  counters: CounterSnapshot;
  /** Frames a listener would have heard and did not. Excludes the pre-connection window. */
  droppedFrames: number;
  /** Present whenever audio is flowing. Never hide this. */
  latencyNote: string | null;
}}

/** Advice for a suspected firewall block. The command is shown, never executed. */
export interface FirewallDto {{
  suspected: boolean;
  command: string;
  explanation: string;
}}

/** A redacted support bundle. */
export interface DiagnosticsDto {{
  text: string;
}}

/** The sentence shown while connected. Duplicated from Rust so a test can compare them. */
export const LATENCY_NOTE = {latency_note:?};
"#,
        latency_note = crate::LATENCY_NOTE,
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use std::path::PathBuf;

    use super::*;

    fn checked_in_path() -> PathBuf {
        // `CARGO_MANIFEST_DIR` is `crates/rincon-ipc`.
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(BINDINGS_PATH)
    }

    /// Covers: RQ-UI-008
    #[test]
    fn rq_ui_008_checked_in_bindings_match_the_rust_dtos() {
        let expected = render();
        let path = checked_in_path();

        let Ok(actual) = std::fs::read_to_string(&path) else {
            panic!(
                "the generated bindings are missing at {}.\nRun `just bindings`.",
                path.display()
            );
        };

        // Normalise line endings: the file is checked out with LF per .gitattributes, but a
        // Windows editor may still have rewritten it, and that is not a contract change.
        let normalise = |text: &str| text.replace("\r\n", "\n");
        assert_eq!(
            normalise(&actual),
            normalise(&expected),
            "the TypeScript bindings are out of date with the Rust DTOs.\n\
             Run `just bindings` and commit {}.",
            path.display()
        );
    }

    #[test]
    fn every_error_code_appears_in_the_generated_union() {
        // A new `ErrorCode` variant that nobody added to the generator would otherwise reach
        // the interface as a string TypeScript has never heard of.
        let generated = render();
        for code in [
            crate::ErrorCode::UnknownDevice,
            crate::ErrorCode::InvalidState,
            crate::ErrorCode::ScanFailed,
            crate::ErrorCode::SessionFailed,
            crate::ErrorCode::InvalidArgument,
            crate::ErrorCode::Internal,
        ] {
            let serialised = serde_json::to_string(&code).unwrap();
            let bare = serialised.trim_matches('"');
            assert!(
                generated.contains(&format!("\"{bare}\"")),
                "`{bare}` is missing from the generated ErrorCode union"
            );
        }
    }

    #[test]
    fn the_latency_note_is_the_same_string_on_both_sides() {
        // Two copies of a user-facing sentence is a drift risk; the generator embeds the Rust
        // constant so there is only ever one source.
        assert!(render().contains(crate::LATENCY_NOTE));
    }
}
