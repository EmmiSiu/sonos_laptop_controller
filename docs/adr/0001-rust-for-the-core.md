# ADR-0001 — Rust for the core

**Status:** accepted · **Date:** 2026-09-20

## Context

The central constraint is a real-time audio callback with a deadline in single-digit
milliseconds, feeding an async HTTP server. Miss the deadline and the user hears a click. The
same program also parses unauthenticated input from the network and opens a listening socket on
a home LAN.

So the language has to give us three things at once: no unpredictable pauses, memory safety
against hostile parser input, and a way to make the callback-to-async boundary reviewable.

## Options

**Go.** Excellent concurrency, memory safe, fast to write. The garbage collector's
stop-the-world pauses are sub-millisecond in the common case and occasionally are not — and the
occasional case is the one the user hears. Ruled out on that alone.

**C++.** Total control, mature audio ecosystem, the language most real-time audio is written
in. Also the language in which a parser bug reading multicast from an untrusted LAN is a
remote memory-safety vulnerability. For a program that captures the user's conversations, that
trade is not available.

**C# / .NET.** Best Windows API story by a distance, and WASAPI bindings that already exist.
Same GC objection as Go, plus a runtime dependency in the installer, plus no realistic path to
webOS.

**Rust.** No runtime, no GC, memory safe, and — the deciding factor — `Send`/`Sync` turn the
real-time boundary into a compile error rather than a code review. A `MutexGuard` cannot
accidentally cross into the callback because the type system refuses.

## Decision

Rust, edition 2024. Core crates retain MSRV 1.85; the separately built Tauri shell requires
1.88 because that is the minimum supported by the patched `plist` dependency.

## Consequences

**Good.** The ring buffer's correctness is partly the compiler's problem. `cpal` gives us three
platforms from one trait. Cross-compilation for the eventual webOS ARM target is routine. The
lint configuration (`pedantic` + `nursery`, `-D warnings`) catches a class of bug that would
otherwise need review attention.

**Bad.** Compile times, which the CI cache mitigates and nothing fixes. A smaller contributor
pool than Go or C#. `async` Rust in particular has a learning curve that shows up in review.

**Accepted cost.** We write more type-level ceremony than a Go version would: `Volume`,
`SampleRate`, `DeviceId`, `StreamUrl` are all newtypes that a dynamically-typed version would
express as bare integers and strings. That ceremony is the mechanism by which the invariants
hold, so it is the feature, not the tax.

## Revisit if

Compile times become the dominant cost of contribution, or a platform we need has no Rust
toolchain. Neither is in sight.
