# ADR-0002 — Tauri rather than Electron

**Status:** accepted · **Date:** 2026-09-20

## Context

The interface is a handful of states: a scan, a list, a progress stepper, a now-playing panel,
and a failure panel. It needs to look current, be quick to iterate on, and run alongside a Rust
process that owns an audio device.

## Options

**Electron.** The most predictable path — one Chromium, identical rendering everywhere, the
largest ecosystem. It also ships a browser: ~150 MB installed, ~200 MB RSS idle. For a utility
that sits in the corner of a screen so a laptop can play music, that is absurd, and users
notice.

**Native Win32 / WinUI.** The smallest and fastest option, and the one that produces the best
Windows integration. It is also a rewrite for macOS and dead weight for webOS, and iterating on
a radar animation in XAML is slow enough that the interface would end up worse.

**egui / iced (Rust-native).** No WebView, one language, small binaries. Both are capable, and
neither makes the kind of interface this needs — a level meter, a soft radar, considered
typography — pleasant to build. We would fight the toolkit for every state.

**Tauri 2.** The system WebView rather than a bundled one: ~10 MB installer, ~40 MB RSS. The
backend is Rust, which we are already writing. The CSP is ours to set.

## Decision

Tauri 2, with Vue 3, TypeScript, and Tailwind in the renderer.

## Consequences

**Good.** The installer target in SPEC-000 G4 (under 15 MB) is reachable. The IPC boundary is
typed, and the types are generated from the Rust DTOs. A strict CSP with no `unsafe-inline` is
enforceable and enforced.

**Bad.** Rendering differs between WebView2 on Windows and WKWebView on macOS, so the interface
needs checking on both. Tauri 2 is younger than Electron and its ecosystem is thinner.

**The real cost.** A WebView is a browser, and this one renders strings supplied by
unauthenticated devices on the LAN. That is a genuine trust boundary and it is why SPEC-006
exists: opaque identifiers instead of addresses, text-only rendering, a strict CSP, and a
structural test that greps for `v-html`. Electron would have had the same boundary; Tauri makes
it cheaper to lock down because the capability set is opt-in.

**Deliberate consequence.** The shell lives in its own cargo workspace. Tauri needs webkit2gtk
on Linux, and pulling that into the root workspace would mean installing GTK in CI to test a
WAV header. The logic the shell wraps lives in `rincon-ipc`, which is a normal workspace member
tested on all three platforms.

## Revisit if

A WebView-rendering difference becomes a recurring source of bugs, or the interface stops being
the part of the product that benefits from fast iteration.
