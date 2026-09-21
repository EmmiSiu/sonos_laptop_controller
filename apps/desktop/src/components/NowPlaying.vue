<script setup lang="ts">
/**
 * The connected state.
 *
 * Three things have to be on this screen at once: that it is working, how well, and the
 * latency caveat. The last one is not a footnote — a user who discovers the 1-2 second buffer
 * by starting a film concludes the app is broken, and they are not wrong to.
 */
import { computed } from "vue";

import { displayText } from "../lib/sanitize";
import type { SessionDto } from "../lib/bindings";

const props = defineProps<{ session: SessionDto }>();
const emit = defineEmits<{ stop: []; volume: [level: number] }>();

const degraded = computed(() => props.session.detail.state === "degraded");

const health = computed(() => {
  const detail = props.session.detail;
  if (detail.state === "streaming") {
    return detail.health;
  }
  return detail.state === "degraded" ? "poor" : "good";
});

const healthLabel = computed(
  () =>
    ({
      good: "Clean",
      fair: "Occasional hiccups",
      poor: "Dropping audio",
      lost: "Disconnected",
    })[health.value] ?? "Clean",
);

const counters = computed(() => props.session.counters);

/** Twelve bars driven by the drop counters, so the meter reflects something real. */
const bars = computed(() => {
  const dropped = props.session.droppedFrames;
  const served = Math.max(1, counters.value.frames_served);
  const quality = Math.max(0, 1 - dropped / served);
  return Array.from({ length: 12 }, (_, index) => ({
    lit: index < Math.round(quality * 12),
    key: index,
  }));
});

function onVolume(event: Event): void {
  const target = event.target as HTMLInputElement;
  emit("volume", Number(target.value));
}
</script>

<template>
  <div class="flex flex-1 flex-col gap-6 px-6 py-7">
    <div class="space-y-1">
      <div class="flex items-center gap-2">
        <span
          class="h-2 w-2 rounded-full"
          :class="degraded ? 'bg-amber-400 animate-breathe' : 'bg-accent animate-breathe'"
          aria-hidden="true"
        />
        <h2 class="text-base font-medium text-slate-100">
          Playing on {{ displayText(session.room ?? "your speaker") }}
        </h2>
      </div>
      <p class="text-xs" :class="degraded ? 'text-amber-300' : 'text-slate-500'">
        {{ healthLabel }}
      </p>
    </div>

    <div class="flex items-end gap-1" role="img" :aria-label="`Audio health: ${healthLabel}`">
      <span
        v-for="bar in bars"
        :key="bar.key"
        class="w-full rounded-sm transition-colors"
        :class="[
          bar.lit ? (degraded ? 'bg-amber-400/80' : 'bg-accent/80') : 'bg-ink-700',
          bar.key % 3 === 0 ? 'h-8' : bar.key % 2 === 0 ? 'h-6' : 'h-4',
        ]"
      />
    </div>

    <label class="space-y-1.5 text-xs text-slate-500">
      <span>Speaker volume</span>
      <input
        type="range"
        min="0"
        max="100"
        step="1"
        class="w-full accent-[#6ee7b7]"
        @change="onVolume"
      />
    </label>

    <p
      v-if="session.latencyNote"
      class="rounded-lg bg-ink-800/70 p-3 text-[11px] leading-relaxed text-slate-400 ring-1 ring-ink-700"
    >
      {{ session.latencyNote }}
    </p>

    <dl class="grid grid-cols-3 gap-2 text-center">
      <div class="rounded-lg bg-ink-800/50 py-2">
        <dt class="text-[10px] uppercase tracking-wider text-slate-600">Dropped</dt>
        <dd class="font-mono text-xs text-slate-300">
          {{ session.droppedFrames }}
        </dd>
      </div>
      <div class="rounded-lg bg-ink-800/50 py-2">
        <dt class="text-[10px] uppercase tracking-wider text-slate-600">Underruns</dt>
        <dd class="font-mono text-xs text-slate-300">{{ counters.underruns }}</dd>
      </div>
      <div class="rounded-lg bg-ink-800/50 py-2">
        <dt class="text-[10px] uppercase tracking-wider text-slate-600">Reconnects</dt>
        <dd class="font-mono text-xs text-slate-300">{{ counters.reconnects }}</dd>
      </div>
    </dl>

    <button
      type="button"
      class="mt-auto rounded-lg bg-ink-800 px-4 py-2 text-sm font-medium text-slate-200 ring-1 ring-ink-700 transition hover:bg-ink-700"
      @click="$emit('stop')"
    >
      Stop
    </button>
  </div>
</template>
