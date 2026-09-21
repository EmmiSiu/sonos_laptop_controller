# ADR-0004 — One `unsafe` exemption, for a test-only allocator

**Status:** accepted · **Date:** 2026-09-20

## Context

`unsafe_code = "deny"` is set workspace-wide, and SPEC-007 `RQ-SEC-010` requires an ADR for any
exemption. This is the only one.

`RQ-AUD-001` says the audio producer path must not allocate. That is the difference between a
clean stream and an intermittent click on somebody else's machine, and it is exactly the kind
of property that decays — someone adds a `format!` to a log line inside a callback and nobody
notices for three releases.

## Options

**Assert it in a comment.** Free, and worth nothing. This is the state the requirement was in
before this decision.

**Review discipline.** Depends on every future reviewer knowing the rule and spotting a
`Vec::push` behind two layers of helper. Not a mechanism.

**A heap-profiling tool in CI.** `dhat`, `valgrind`, or similar. Heavier, platform-specific,
and answers "how much did the program allocate" rather than "did this code path allocate at
all" — which is the question.

**A counting global allocator, under `#[cfg(test)]`.** Implementing `GlobalAlloc` requires
`unsafe`, because the trait is unsafe. Everything else about it is twenty lines.

## Decision

A counting allocator in `rincon-audio`, gated on `#[cfg(test)]`, with
`#[allow(unsafe_code, reason = "...")]` on the module.

## Why it is safe

Every method forwards directly to `std::alloc::System`, which upholds the `GlobalAlloc`
contract. The added bookkeeping touches only a thread-local `Cell<Option<u64>>`, which cannot
itself allocate and so cannot re-enter the allocator. `try_with` is used rather than `with`,
because a thread-local may already be destroyed during thread teardown.

The counter is thread-local rather than global, so tests running in parallel do not perturb one
another's measurements.

## Scope

`#[cfg(test)]` means no `unsafe` reaches a shipped binary. A release build does not contain
this module.

## Consequences

**Good.** `RQ-AUD-001` is proven rather than asserted: the test runs two hundred callbacks,
including during ring overrun, and fails if a single allocation occurs. A regression is a red
build, not a support ticket.

**Guard against the obvious failure.** A probe that always reported zero would make the
requirement vacuous, so a second test allocates deliberately and asserts the probe notices.

**Bad.** The workspace can no longer claim "zero `unsafe`" without a footnote. The footnote is
this file.

## Revisit if

A stable, safe API for counting allocations appears, or the probe's cost in review attention
exceeds the value of the property it proves.
