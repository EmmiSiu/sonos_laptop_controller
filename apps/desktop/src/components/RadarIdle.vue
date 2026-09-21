<script setup lang="ts">
/**
 * The idle and scanning state.
 *
 * The radar is not decoration: a scan takes two seconds with no intermediate feedback, and a
 * static "Searching..." for two seconds reads as a hang. The animation is pure CSS so it costs
 * nothing, and it respects `prefers-reduced-motion` via the global stylesheet.
 */
defineProps<{ scanning: boolean }>();
defineEmits<{ scan: [] }>();
</script>

<template>
  <div class="flex flex-1 flex-col items-center justify-center gap-8 px-6 text-center">
    <div class="relative grid h-32 w-32 place-items-center">
      <template v-if="scanning">
        <span
          v-for="delay in [0, 800, 1600]"
          :key="delay"
          class="absolute h-32 w-32 rounded-full border border-accent/50 animate-radar"
          :style="{ animationDelay: `${delay}ms` }"
          aria-hidden="true"
        />
      </template>
      <span
        class="relative grid h-14 w-14 place-items-center rounded-full bg-ink-800 ring-1 ring-ink-600"
        :class="scanning ? 'animate-breathe' : ''"
      >
        <svg viewBox="0 0 24 24" class="h-6 w-6 text-accent" fill="none" stroke="currentColor"
             stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
          <path d="M4 9v6M8 6v12M12 3v18M16 6v12M20 9v6" />
        </svg>
      </span>
    </div>

    <div class="space-y-1.5">
      <h2 class="text-base font-medium text-slate-100">
        {{ scanning ? "Looking for speakers" : "Play this computer on a Sonos" }}
      </h2>
      <p class="max-w-xs text-xs leading-relaxed text-slate-500">
        {{
          scanning
            ? "Checking every network this computer is on."
            : "Whatever your laptop is playing \u2014 a browser tab, a film, a DAW \u2014 sent to a speaker on your Wi-Fi."
        }}
      </p>
    </div>

    <button
      type="button"
      class="rounded-lg bg-accent px-5 py-2 text-sm font-medium text-ink-950 transition hover:bg-accent-dim disabled:opacity-40"
      :disabled="scanning"
      @click="$emit('scan')"
    >
      {{ scanning ? "Searching\u2026" : "Find my speakers" }}
    </button>
  </div>
</template>
