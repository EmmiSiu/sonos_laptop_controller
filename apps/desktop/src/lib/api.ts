/**
 * The typed client for the Rust backend.
 *
 * Every command goes through here, so there is exactly one place that knows the command names
 * and exactly one place that turns a rejected `invoke` into a typed {@link IpcError}. A
 * component that called `invoke` directly would have to re-derive both.
 *
 * The types come from `bindings.ts`, which is generated from the Rust DTOs and checked by a
 * Rust test — so renaming a field in Rust breaks this build rather than producing `undefined`
 * in front of a user.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  DeviceDto,
  DiagnosticsDto,
  FirewallDto,
  IpcError,
  SessionDto,
} from "./bindings";

/** The event the backend pushes on every state transition. The interface never polls. */
const SESSION_EVENT = "rincon://session";

/** Narrows an unknown rejection to the shape the backend promises. */
export function isIpcError(value: unknown): value is IpcError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value &&
    "retryable" in value
  );
}

/**
 * Turns anything a rejected `invoke` can produce into an {@link IpcError}.
 *
 * A rejection that is not the agreed shape means the backend panicked or a command is missing,
 * which is a bug in us rather than a condition the user can act on — so it is labelled as one
 * instead of being shown as a mysterious string.
 */
function toIpcError(raw: unknown): IpcError {
  if (isIpcError(raw)) {
    return raw;
  }
  return {
    code: "internal",
    message:
      typeof raw === "string" && raw.length > 0
        ? raw
        : "Rincon hit an unexpected problem. Copy a diagnostics bundle and open an issue.",
    retryable: false,
  };
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return (await invoke<T>(command, args)) satisfies T;
  } catch (raw) {
    throw toIpcError(raw);
  }
}

/** Runs a discovery scan. An empty array means "found nothing", not "failed". */
export function scanDevices(): Promise<DeviceDto[]> {
  return call<DeviceDto[]>("scan_devices");
}

/**
 * Connects to a speaker.
 *
 * Takes the opaque identifier from a previous scan. There is deliberately no overload that
 * accepts an address: the backend has no command that would accept one.
 */
export function connect(id: string): Promise<void> {
  return call<void>("connect", { id });
}

/** Stops the session and releases the audio device. */
export function disconnect(): Promise<void> {
  return call<void>("disconnect");
}

/** Sets the speaker's volume. Rejects with `invalid_argument` above 100. */
export function setVolume(level: number): Promise<number> {
  return call<number>("set_volume", { level });
}

/** Reads the current session state. Used once at startup; after that, events arrive. */
export function sessionState(): Promise<SessionDto> {
  return call<SessionDto>("session_state");
}

/** Firewall advice. The command is shown to the user; nothing here runs it. */
export function firewallStatus(): Promise<FirewallDto> {
  return call<FirewallDto>("firewall_status");
}

/** Builds a redacted diagnostics bundle. */
export function diagnostics(): Promise<DiagnosticsDto> {
  return call<DiagnosticsDto>("diagnostics");
}

/** Subscribes to session state pushes. Returns the unsubscribe function. */
export function onSession(handler: (session: SessionDto) => void): Promise<UnlistenFn> {
  return listen<SessionDto>(SESSION_EVENT, (event) => handler(event.payload));
}
