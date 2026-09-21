<script setup lang="ts">
/**
 * The empty state.
 *
 * Deliberately a checklist rather than an apology. "No speakers found" tells the user nothing;
 * the three things that are actually wrong, in the order they are usually wrong, tells them
 * what to try. AP isolation in particular is invisible from the machine and is the cause a
 * user would never guess.
 */
defineProps<{ scanning: boolean }>();
defineEmits<{ scan: [] }>();

const checks = [
  {
    title: "Same network",
    detail: "This computer and the speaker have to be on the same Wi-Fi, not a guest network.",
  },
  {
    title: "AP isolation",
    detail:
      "Some routers stop devices from seeing each other. It is often called AP isolation, client isolation, or guest mode.",
  },
  {
    title: "VPN",
    detail: "A VPN can route the search away from your home network. Try disconnecting it.",
  },
];
</script>

<template>
  <div class="flex flex-1 flex-col gap-5 overflow-y-auto px-6 py-8">
    <div class="space-y-1">
      <h2 class="text-base font-medium text-slate-100">No speakers answered</h2>
      <p class="text-xs text-slate-500">
        The search ran, and nothing replied. These are the usual reasons, in order.
      </p>
    </div>

    <ol class="space-y-2.5">
      <li
        v-for="(check, index) in checks"
        :key="check.title"
        class="flex gap-3 rounded-lg bg-ink-800/60 p-3 ring-1 ring-ink-700"
      >
        <span
          class="grid h-5 w-5 shrink-0 place-items-center rounded-full bg-ink-700 text-[10px] font-medium text-slate-400"
        >
          {{ index + 1 }}
        </span>
        <div class="space-y-0.5">
          <p class="text-xs font-medium text-slate-200">{{ check.title }}</p>
          <p class="text-[11px] leading-relaxed text-slate-500">{{ check.detail }}</p>
        </div>
      </li>
    </ol>

    <button
      type="button"
      class="mt-auto rounded-lg bg-accent px-4 py-2 text-sm font-medium text-ink-950 transition hover:bg-accent-dim disabled:opacity-40"
      :disabled="scanning"
      @click="$emit('scan')"
    >
      {{ scanning ? "Searching\u2026" : "Search again" }}
    </button>
  </div>
</template>
