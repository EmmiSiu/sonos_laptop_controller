# ADR-0005 — Orchestration as a pure reducer

**Status:** accepted · **Date:** 2026-09-20

## Context

Starting a session means: resolve the group coordinator, start capture, bind a server on an
interface the speaker can route back to, hand it the URL, tell it to play, and wait for it to
connect. Six steps, each of which can fail, with four OS resources that must be released on
every failure path.

The natural way to write this in Rust is an `async fn` with `?` at each step and a cleanup
block. That version is what Rincon does not do.

## Options

**Sequential `async fn`.** Shortest to write. Every failure path needs its own cleanup, testing
any of them needs mocking the whole stack, and "does the third step's failure release the
socket the second step opened?" is answerable only by reading carefully.

**An actor with internal mutable state.** Better isolation, still requires spinning up the
actor and its I/O to test a transition.

**A pure reducer plus a driver.** `step(state, event) -> (state, effects)`, with no I/O, no
clock, and no allocation beyond the effects. A separate driver executes effects and feeds the
answers back as events.

## Decision

The reducer.

## Consequences

**Good, and the reason for the decision:**

- Every transition the session can make is in one `match`, visible at once. A transition that
  is not written down cannot occur.
- Every failure path is a unit test that runs in microseconds with no socket and no speaker.
  `RQ-ENG-006` — "every exit from a live session releases capture, the server, and playback" —
  is a loop over every (state, exit event) pair, which is not a test anyone would write against
  an `async fn`.
- A late reply from a cancelled operation is inert, because the reducer has no arm for it. In
  the `async fn` version that is a bug class you find in production.
- The state the interface renders *is* the state the engine holds. There is no second copy to
  drift.

**Bad:**

- Two places to look: what happens (`machine.rs`) and how (`driver.rs`).
- Effects are data, so a new one means touching an enum, the reducer, and the driver. That is
  friction, and it is the point — an effect should be a deliberate addition.
- The reducer cannot await. Anything needing a clock becomes an event the driver produces,
  which is why `ArmPeerTimeout` exists as an effect rather than a `tokio::time::timeout`.

**A rule this creates:** the driver makes no decisions. If it grows an `if` that changes the
session's direction, that `if` belongs in the reducer. This is written in `driver.rs` and in
`AGENTS.md`, because it is the invariant most likely to erode.

## Revisit if

The effect enum grows past roughly twenty variants, at which point the indirection may cost
more than it buys.
