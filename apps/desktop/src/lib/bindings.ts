// GENERATED FILE — do not edit.
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
export interface IpcError {
  code: ErrorCode;
  message: string;
  retryable: boolean;
}

/** A speaker, as the interface sees it. Carries an opaque id, never a routable address. */
export interface DeviceDto {
  id: string;
  room: string;
  model: string;
  subnet: string;
}

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
  | { kind: "no_capture_device" }
  | { kind: "capture_busy" }
  | { kind: "device_unreachable" }
  | { kind: "rejected_by_device"; detail: { upnp_code: number; description: string } }
  | { kind: "firewall_suspected" }
  | { kind: "network_isolated" }
  | { kind: "scan_failed"; detail: { detail: string } }
  | { kind: "internal"; detail: { detail: string } };

/** The authoritative session state. The stream URL arrives already redacted. */
export type SessionState =
  | { state: "idle" }
  | { state: "scanning" }
  | { state: "devices_found"; devices: DeviceDto[] }
  | { state: "no_devices" }
  | { state: "preparing"; step: PrepareStep }
  | { state: "streaming"; url: string; health: Health }
  | { state: "degraded"; url: string; reason: DegradeReason }
  | { state: "stopping" }
  | { state: "failed"; reason: FailureReason; retryable: boolean };

/** Counters for the live session. */
export interface CounterSnapshot {
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
}

/** Everything the interface needs to render one frame of the session. */
export interface SessionDto {
  state: string;
  detail: SessionState;
  room: string | null;
  counters: CounterSnapshot;
  /** Frames a listener would have heard and did not. Excludes the pre-connection window. */
  droppedFrames: number;
  /** Present whenever audio is flowing. Never hide this. */
  latencyNote: string | null;
}

/** Advice for a suspected firewall block. The command is shown, never executed. */
export interface FirewallDto {
  suspected: boolean;
  command: string;
  explanation: string;
}

/** A redacted support bundle. */
export interface DiagnosticsDto {
  text: string;
}

/** The sentence shown while connected. Duplicated from Rust so a test can compare them. */
export const LATENCY_NOTE = "Sonos speakers buffer 1-2 seconds. Great for music; video will look out of sync.";
