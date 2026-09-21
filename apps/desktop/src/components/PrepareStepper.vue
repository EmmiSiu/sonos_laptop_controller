<script setup lang="ts">
/**
 * The five-step connect progress.
 *
 * This exists because of how connections fail. When one does, the user has already watched
 * which step it reached — "Starting the local stream" failing is a completely different
 * problem from "Waiting for the speaker to connect" failing — and that is most of the
 * diagnosis, delivered before they have to ask anyone.
 */
import { computed } from "vue";

import { displayText } from "../lib/sanitize";
import type { PrepareStep } from "../lib/bindings";

const props = defineProps<{ current: PrepareStep; room: string | null }>();
defineEmits<{ cancel: [] }>();

const STEPS: { id: PrepareStep; label: string }[] = [
  { id: "resolve_coordinator", label: "Finding the group leader" },
  { id: "start_capture", label: "Opening system audio" },
  { id: "bind_server", label: "Starting the local stream" },
  { id: "set_uri", label: "Pointing the speaker at it" },
  { id: "play", label: "Starting playback" },
  { id: "await_peer", label: "Waiting for the speaker to connect" },
];

const currentIndex = computed(() => STEPS.findIndex((step) => step.id === props.current));
</script>

<template>
  <div class="flex flex-1 flex-col gap-6 px-6 py-8">
    <div class="space-y-1">
      <h2 class="text-base font-medium text-slate-100">
        Connecting to {{ displayText(room ?? "your speaker") }}
      </h2>
      <p class="text-xs text-slate-500">This usually takes a couple of seconds.</p>
    </div>

    <ol class="space-y-2.5">
      <li
        v-for="(step, index) in STEPS"
        :key="step.id"
        class="flex items-center gap-3"
        :aria-current="index === currentIndex ? 'step' : undefined"
      >
        <span
          class="grid h-5 w-5 shrink-0 place-items-center rounded-full text-[10px] transition"
          :class="{
            'bg-accent text-ink-950': index < currentIndex,
            'bg-accent/20 text-accent ring-1 ring-accent animate-breathe': index === currentIndex,
            'bg-ink-800 text-slate-600 ring-1 ring-ink-700': index > currentIndex,
          }"
        >
          <svg
            v-if="index < currentIndex"
            viewBox="0 0 24 24"
            class="h-3 w-3"
            fill="none"
            stroke="currentColor"
            stroke-width="3"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <path d="M5 13l4 4L19 7" />
          </svg>
          <template v-else>{{ index + 1 }}</template>
        </span>
        <span
          class="text-xs transition"
          :class="index <= currentIndex ? 'text-slate-200' : 'text-slate-600'"
        >
          {{ step.label }}
        </span>
      </li>
    </ol>

    <button
      type="button"
      class="mt-auto rounded-lg px-4 py-2 text-xs font-medium text-slate-400 ring-1 ring-ink-700 transition hover:text-slate-200 hover:ring-ink-600"
      @click="$emit('cancel')"
    >
      Cancel
    </button>
  </div>
</template>
