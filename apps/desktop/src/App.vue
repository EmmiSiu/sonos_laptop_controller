<script setup lang="ts">
/**
 * The whole interface.
 *
 * One component holding the state switch, with the visually distinct pieces split out. The
 * session state arrives from the backend already decided (`rincon-engine` owns the state
 * machine), so this file renders it and sends commands — it does not model anything.
 *
 * That is the point of the split: an interface that re-derives "am I connected?" from three
 * booleans is an interface that will eventually disagree with the backend.
 */

import { computed, onMounted, onUnmounted, ref } from "vue";

import DeviceList from "./components/DeviceList.vue";
import FailurePanel from "./components/FailurePanel.vue";
import NoDevices from "./components/NoDevices.vue";
import NowPlaying from "./components/NowPlaying.vue";
import PrepareStepper from "./components/PrepareStepper.vue";
import RadarIdle from "./components/RadarIdle.vue";
import * as api from "./lib/api";
import type { DeviceDto, IpcError, SessionDto } from "./lib/bindings";

const session = ref<SessionDto | null>(null);
const devices = ref<DeviceDto[]>([]);
const error = ref<IpcError | null>(null);
const busy = ref(false);

let unlisten: (() => void) | null = null;

const state = computed(() => session.value?.detail.state ?? "idle");
const scanning = computed(() => state.value === "scanning" || busy.value);

onMounted(async () => {
  unlisten = await api.onSession((next) => {
    session.value = next;
    // A fresh scan result replaces the list; anything else leaves it alone so the cards do
    // not vanish while a connection is being prepared.
    if (next.detail.state === "devices_found") {
      devices.value = next.detail.devices;
    }
  });
  session.value = await api.sessionState();
});

onUnmounted(() => unlisten?.());

/** Wraps a command so every failure lands in one place and the button cannot double-fire. */
async function run(action: () => Promise<unknown>): Promise<void> {
  if (busy.value) {
    return;
  }
  busy.value = true;
  error.value = null;
  try {
    await action();
  } catch (raw) {
    error.value = api.isIpcError(raw)
      ? raw
      : { code: "internal", message: String(raw), retryable: false };
  } finally {
    busy.value = false;
  }
}

const scan = () =>
  run(async () => {
    devices.value = await api.scanDevices();
  });

const connect = (id: string) => run(() => api.connect(id));
const disconnect = () => run(() => api.disconnect());
const setVolume = (level: number) => run(() => api.setVolume(level));
</script>

<template>
  <main class="flex h-screen flex-col overflow-hidden">
    <header
      class="flex shrink-0 items-center justify-between border-b border-ink-700/60 px-5 py-3"
    >
      <div class="flex items-baseline gap-2">
        <h1 class="text-sm font-semibold tracking-wide text-slate-100">Rincon</h1>
        <span class="text-[11px] text-slate-500">system audio &rarr; Sonos</span>
      </div>
      <span
        class="rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider"
        :class="{
          'bg-ink-700 text-slate-400': !session?.detail || state === 'idle',
          'bg-accent/15 text-accent': state === 'streaming',
          'bg-amber-500/15 text-amber-300': state === 'degraded',
          'bg-rose-500/15 text-rose-300': state === 'failed',
        }"
      >
        {{ session?.state ?? "idle" }}
      </span>
    </header>

    <section class="flex min-h-0 flex-1 flex-col">
      <RadarIdle
        v-if="state === 'idle' || state === 'scanning'"
        :scanning="scanning"
        @scan="scan"
      />

      <NoDevices v-else-if="state === 'no_devices'" :scanning="scanning" @scan="scan" />

      <DeviceList
        v-else-if="state === 'devices_found'"
        :devices="devices"
        :busy="busy"
        @connect="connect"
        @rescan="scan"
      />

      <PrepareStepper
        v-else-if="state === 'preparing' && session?.detail.state === 'preparing'"
        :current="session.detail.step"
        :room="session.room"
        @cancel="disconnect"
      />

      <NowPlaying
        v-else-if="session && (state === 'streaming' || state === 'degraded')"
        :session="session"
        @stop="disconnect"
        @volume="setVolume"
      />

      <FailurePanel
        v-else-if="session?.detail.state === 'failed'"
        :reason="session.detail.reason"
        :retryable="session.detail.retryable"
        @retry="scan"
        @dismiss="scan"
      />
    </section>

    <footer
      v-if="error"
      class="shrink-0 border-t border-rose-500/30 bg-rose-500/10 px-5 py-2.5 text-xs text-rose-200"
      role="alert"
    >
      {{ error.message }}
    </footer>
  </main>
</template>
