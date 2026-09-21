<script setup lang="ts">
/**
 * The failure state.
 *
 * One sentence of cause, one sentence of remediation, one button. Both sentences come from the
 * Rust side, where the failure taxonomy lives — the interface does not get to invent wording
 * for a condition it does not model.
 */
import { computed, ref } from "vue";

import * as api from "../lib/api";
import type { FailureReason } from "../lib/bindings";

const props = defineProps<{ reason: FailureReason; retryable: boolean }>();
defineEmits<{ retry: []; dismiss: [] }>();

const firewallCommand = ref<string | null>(null);
const copied = ref(false);

const MESSAGES: Record<FailureReason["kind"], { title: string; advice: string }> = {
  no_capture_device: {
    title: "No playback device found.",
    advice: "Open Windows sound settings and choose an output device.",
  },
  capture_busy: {
    title: "Another app has exclusive control of the audio device.",
    advice: "Close it, then try again.",
  },
  device_unreachable: {
    title: "The speaker stopped responding.",
    advice: "Search the network again.",
  },
  rejected_by_device: {
    title: "The speaker refused the command.",
    advice: "Stop what it is currently playing, then try again.",
  },
  firewall_suspected: {
    title: "The speaker could not reach your computer.",
    advice: "Windows Firewall is almost certainly blocking the incoming connection.",
  },
  network_isolated: {
    title: "Your Wi-Fi is keeping this computer and the speaker apart.",
    advice: "Turn off AP isolation, or put both on the same network.",
  },
  scan_failed: { title: "Could not search the network.", advice: "Check Wi-Fi, then try again." },
  internal: {
    title: "Something went wrong inside Rincon.",
    advice: "Copy a diagnostics bundle and open an issue.",
  },
};

const copy = computed(() => MESSAGES[props.reason.kind] ?? MESSAGES.internal);
const isFirewall = computed(() => props.reason.kind === "firewall_suspected");

async function showFirewallHelp(): Promise<void> {
  const advice = await api.firewallStatus();
  firewallCommand.value = advice.command;
}

async function copyCommand(): Promise<void> {
  if (!firewallCommand.value) {
    return;
  }
  await navigator.clipboard.writeText(firewallCommand.value);
  copied.value = true;
  setTimeout(() => {
    copied.value = false;
  }, 1600);
}
</script>

<template>
  <div class="flex flex-1 flex-col gap-5 overflow-y-auto px-6 py-8">
    <div class="space-y-1.5">
      <h2 class="text-base font-medium text-slate-100">{{ copy.title }}</h2>
      <p class="text-xs leading-relaxed text-slate-500">{{ copy.advice }}</p>
    </div>

    <div v-if="isFirewall" class="space-y-3">
      <button
        v-if="!firewallCommand"
        type="button"
        class="rounded-lg bg-ink-800 px-4 py-2 text-xs font-medium text-slate-200 ring-1 ring-ink-700 transition hover:bg-ink-700"
        @click="showFirewallHelp"
      >
        Show me the fix
      </button>

      <div v-else class="space-y-2">
        <p class="text-[11px] text-slate-500">
          Run this in an <strong class="text-slate-400">administrator</strong> terminal. Rincon
          does not run it for you \u2014 a button that silently elevates is not something you
          should have to trust.
        </p>
        <pre
          class="selectable overflow-x-auto rounded-lg bg-ink-950 p-3 font-mono text-[10px] leading-relaxed text-slate-300 ring-1 ring-ink-700"
        >{{ firewallCommand }}</pre>
        <button
          type="button"
          class="text-[11px] text-accent transition hover:text-accent-dim"
          @click="copyCommand"
        >
          {{ copied ? "Copied" : "Copy command" }}
        </button>
      </div>
    </div>

    <div class="mt-auto flex gap-2">
      <button
        v-if="retryable"
        type="button"
        class="flex-1 rounded-lg bg-accent px-4 py-2 text-sm font-medium text-ink-950 transition hover:bg-accent-dim"
        @click="$emit('retry')"
      >
        Try again
      </button>
      <button
        type="button"
        class="rounded-lg px-4 py-2 text-sm font-medium text-slate-400 ring-1 ring-ink-700 transition hover:text-slate-200"
        @click="$emit('dismiss')"
      >
        Start over
      </button>
    </div>
  </div>
</template>
