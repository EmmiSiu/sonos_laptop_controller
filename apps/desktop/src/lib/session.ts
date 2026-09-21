/** Pure presentation helpers for authoritative session data. */

import type { CounterUpdateDto, SessionDto, SessionState } from "./bindings";

/**
 * Applies a metric refresh only to a state that can own live counters.
 *
 * A final queued metrics event may cross the stop transition. Ignoring it outside streaming
 * keeps that stale event from painting numbers onto the next idle screen.
 */
export function withCounterUpdate(session: SessionDto, update: CounterUpdateDto): SessionDto {
  if (session.detail.state !== "streaming" && session.detail.state !== "degraded") {
    return session;
  }
  return { ...session, counters: update.counters, droppedFrames: update.droppedFrames };
}

/** Turns the wire's stable snake-case key into a compact human label. */
export function stateLabel(state: SessionState["state"]): string {
  return state.replaceAll("_", " ");
}
