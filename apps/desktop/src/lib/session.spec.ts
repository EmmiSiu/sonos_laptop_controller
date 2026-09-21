import { describe, expect, it } from "vitest";

import type { CounterSnapshot, CounterUpdateDto, SessionDto } from "./bindings";
import { stateLabel, withCounterUpdate } from "./session";

const zeroes: CounterSnapshot = {
  frames_captured: 0,
  frames_served: 0,
  underruns: 0,
  reconnects: 0,
  dropped_capture: 0,
  dropped_ring: 0,
  dropped_socket: 0,
  dropped_device: 0,
  dropped_no_consumer: 0,
};

const idle: SessionDto = {
  state: "idle",
  detail: { state: "idle" },
  room: null,
  counters: zeroes,
  droppedFrames: 0,
  latencyNote: null,
};

describe("withCounterUpdate", () => {
  // Covers: RQ-UI-012
  it("refreshes a live session without inventing a lifecycle transition", () => {
    const streaming: SessionDto = {
      ...idle,
      state: "streaming",
      detail: { state: "streaming", url: "http://192.168.1.xxx/s/<redacted>/stream.wav", health: "good" },
      room: "Kitchen",
    };
    const update: CounterUpdateDto = {
      counters: { ...zeroes, frames_captured: 48_000, frames_served: 47_900 },
      droppedFrames: 7,
    };

    const refreshed = withCounterUpdate(streaming, update);
    expect(refreshed.detail).toEqual(streaming.detail);
    expect(refreshed.counters.frames_served).toBe(47_900);
    expect(refreshed.droppedFrames).toBe(7);
  });

  // Covers: RQ-UI-012
  it("ignores a late counter event after the session stopped", () => {
    const update: CounterUpdateDto = {
      counters: { ...zeroes, frames_served: 10 },
      droppedFrames: 1,
    };
    expect(withCounterUpdate(idle, update)).toBe(idle);
  });
});

describe("stateLabel", () => {
  it("separates snake-case state names for the status chip", () => {
    expect(stateLabel("no_devices")).toBe("no devices");
    expect(stateLabel("devices_found")).toBe("devices found");
  });
});
