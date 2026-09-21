<script setup lang="ts">
/**
 * The speaker picker.
 *
 * Cards carry the room name the user recognises, the model, and the subnet — the last of those
 * because a household with a guest network genuinely needs to tell two "Kitchen" speakers
 * apart, and the full address is neither needed nor safe to show.
 */
import { displayText } from "../lib/sanitize";
import type { DeviceDto } from "../lib/bindings";

defineProps<{ devices: DeviceDto[]; busy: boolean }>();
defineEmits<{ connect: [id: string]; rescan: [] }>();
</script>

<template>
  <div class="flex flex-1 flex-col gap-3 overflow-y-auto px-5 py-5">
    <div class="flex items-baseline justify-between">
      <h2 class="text-xs font-medium uppercase tracking-wider text-slate-500">
        {{ devices.length }} speaker{{ devices.length === 1 ? "" : "s" }}
      </h2>
      <button
        type="button"
        class="text-[11px] text-slate-500 transition hover:text-slate-300 disabled:opacity-40"
        :disabled="busy"
        @click="$emit('rescan')"
      >
        Search again
      </button>
    </div>

    <button
      v-for="device in devices"
      :key="device.id"
      type="button"
      class="group flex items-center gap-3 rounded-xl bg-ink-800/70 p-3.5 text-left ring-1 ring-ink-700 transition hover:bg-ink-700/70 hover:ring-accent/40 disabled:opacity-40"
      :disabled="busy"
      @click="$emit('connect', device.id)"
    >
      <span
        class="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-ink-700 text-accent ring-1 ring-ink-600"
        aria-hidden="true"
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor"
             stroke-width="1.6" stroke-linecap="round">
          <rect x="6" y="2.5" width="12" height="19" rx="2.5" />
          <circle cx="12" cy="15" r="3.2" />
          <circle cx="12" cy="7" r="1.1" />
        </svg>
      </span>

      <span class="min-w-0 flex-1">
        <span class="block truncate text-sm font-medium text-slate-100">
          {{ displayText(device.room) }}
        </span>
        <span class="block truncate text-[11px] text-slate-500">
          {{ displayText(device.model) }} &middot; {{ displayText(device.subnet, 24) }}
        </span>
      </span>

      <span
        class="text-[11px] font-medium text-slate-600 transition group-hover:text-accent"
      >
        Connect
      </span>
    </button>
  </div>
</template>
