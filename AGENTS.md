# AGENTS.md — the contract for AI contributors

This file is the brief for any AI assistant working in this repository. It is written for a
model, not for a person: it is prescriptive, it states the failure modes it expects, and it
tells you what to do when you are unsure rather than leaving you to guess.

Human contributors should read [`CONTRIBUTING.md`](CONTRIBUTING.md) instead — it says the same
things more briefly and with fewer instructions about your own behaviour.

---

## 1. Orientation: read these, in this order

1. [`specs/README.md`](specs/README.md) — how requirements are identified and enforced.
2. The spec governing the crate you are about to touch. The mapping is in §3.
3. [`specs/SPEC-007-security.md`](specs/SPEC-007-security.md) — the threat model. Every crate
   touches it, because every crate handles input from an untrusted device.
4. The module-level `//!` docs of the file you are editing. They explain *why* the code is
   shaped the way it is, which is the part that is expensive to rediscover.

Do not start by reading all the code. The specs are shorter, they are authoritative, and they
are where the reasoning lives.

---

## 2. The one rule that overrides the rest

**The spec is the source of truth. The code is an implementation of it.**

When the code and the spec disagree, that is a bug in one of them, and which one is a decision
for a human. Say so, propose the fix for both, and do not silently make the code match the spec
or the spec match the code.

If you need behaviour that no requirement covers:

- **Small and obviously in scope** (a helper, a test, a doc fix) → do it.
- **New observable behaviour** → write the requirement first, in the same PR, and say in your
  summary that you are proposing a spec change.
- **Contradicts a non-goal** → stop and ask. The non-goals in SPEC-000 §3 were chosen
  deliberately and are the most common thing an assistant erodes without noticing.

---

## 3. Where things live

| You are changing… | Crate | Spec |
| ----------------- | ----- | ---- |
| Domain types, validators, redaction, counters | `rincon-core` | SPEC-000, SPEC-007, SPEC-008 |
| Audio capture, the ring buffer, sample conversion | `rincon-audio` | SPEC-001 |
| SSDP, device descriptions | `rincon-discovery` | SPEC-003 |
| SOAP, DIDL, grouping, volume | `rincon-control` | SPEC-004 |
| The HTTP server, tokens, the WAV header | `rincon-stream` | SPEC-002 |
| Session orchestration | `rincon-engine` | SPEC-005 |
| Tauri commands, the Vue interface | `apps/desktop` | SPEC-006 |
| Fakes used by other crates' tests | `rincon-testkit` | — |

`rincon-core` performs **no I/O**. If you are about to add a networking or filesystem dependency
to it, you are in the wrong crate.

---

## 4. Definition of done

A change is not finished until all of these hold. Run `just verify` — it is this list, in order.

```
✓ cargo fmt --all -- --check
✓ cargo clippy --workspace --all-targets --all-features -- -D warnings
✓ cargo test --workspace --all-features
✓ node scripts/spec-guard.mjs
```

`clippy` runs at `pedantic` + `nursery` with warnings as errors, **including in tests**. This
is not negotiable and it is not a style preference: the lints that matter here
(`unwrap_used`, `panic`, `indexing_slicing`, `float_cmp`) each correspond to a way this specific
program fails audibly in a user's living room.

### When a lint fights you

In order of preference:

1. **Restructure the code** so the lint does not apply. Usually the lint is right.
2. **`#[expect(lint, reason = "…")]`** with a reason that states the invariant making it safe.
   `#[expect]` over `#[allow]`: it fails the build when the situation stops applying.
3. **Argue for removing the lint from the workspace** in your summary, with the class of code
   it is wrong about. This has happened once — `integer_division`, which fires on every correct
   line of frame arithmetic — and the reasoning is recorded in the workspace `Cargo.toml`.

Never add a bare `#[allow]` to make a warning go away. The only blanket allowances in the tree
are the test-module preamble in §6 and one documented `unsafe` exemption
([ADR-0004](docs/adr/0004-test-only-allocation-probe.md)).

---

## 5. Writing tests

Every test that proves a requirement declares it:

```rust
/// Covers: RQ-STRM-004, RQ-SEC-006
#[tokio::test]
async fn rq_strm_004_rejects_unallowlisted_peer() { /* ... */ }
```

`spec-guard` reads those annotations. A requirement with no annotation fails the build; an
annotation naming a requirement that no longer exists also fails the build.

### What a good test looks like here

- **Name the failure, not the function.** `rejects_unallowlisted_peer` beats `test_permits`.
- **Assert the consequence.** For `RQ-STRM-004`, checking the status is `403` is half the test;
  also assert that `frames_served == 0`, because the requirement is that audio never reached
  the stranger.
- **Explain the failure in the message.** `assert!(cond, "the oldest block survived an overrun;
  newest-first ordering is broken")` tells the next person what broke. `assert!(cond)` does not.
- **Prefer a property over three examples** for anything that parses or converts. Hostile input
  is generated better than it is imagined; every parser in this repo has a
  "arbitrary bytes never panic" property for that reason.
- **Prefer a golden fixture** for anything that goes on the wire. A SOAP envelope or a WAV
  header is compared byte-for-byte, because a single wrong byte there produces silence with no
  error anywhere.
- **Never assert on timing** unless the requirement is about timing, and then assert a generous
  ceiling. CI runners are shared and slow.

### The fakes

`rincon-testkit` provides a fake Sonos that answers real SSDP and real SOAP, and
`rincon-audio::synthetic` provides deterministic audio. Use them. A test that needs a speaker in
the room is a test that never runs in CI, which is where regressions are actually caught.

---

## 6. House style

The codebase has a voice. Match it.

- **Module docs explain why, not what.** Every `//!` block answers "why is this file shaped like
  this?" — the nesting trap in `description.rs`, the endless-file trick in `wav.rs`, the
  drop-oldest argument in `ring.rs`. A module doc that restates the type names is noise.
- **Comments mark decisions and traps.** Explain the non-obvious choice, the bug a line
  prevents, or the invariant a cast relies on. Do not narrate control flow.
- **Errors are typed, with a user sentence and a retry classification.** There are no `String`
  errors anywhere. If you find yourself writing one, you are deferring a decision the interface
  will have to make badly later.
- **Invariants live in types.** `Volume` cannot be 101. `DeviceId` cannot be empty.
  `SampleRate` cannot be zero. When you are about to validate a value at a call site, ask
  whether it should have been unrepresentable instead.
- **British-leaning spelling in prose** (`normalise`, `behaviour`), US spelling in identifiers
  that mirror external APIs. Do not churn existing text either way.

Every test module starts with exactly this preamble, produced by
`python scripts/normalize-test-allows.py`:

```rust
#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
```

Do not extend that list to silence a new warning in a test. Fix the test.

---

## 7. Things that are easy to get wrong here

These are real traps in this codebase. Each has cost someone time already.

| Trap | What happens | Where it is handled |
| ---- | ------------ | ------------------- |
| Taking the first `<UDN>` in a device description | You target a `MediaRenderer` sub-device; transport commands are silently ignored | `discovery/description.rs` |
| Comparing `uuid:RINCON_x` with `RINCON_x` | Grouping never matches; the app looks broken only in households that group | `control::uuid_of` |
| Single-escaping DIDL metadata | The speaker accepts the command and plays nothing, with no error | `control/didl.rs` |
| Comparing an IPv4 peer against an IPv4-mapped IPv6 allowlist entry | The allowlist rejects the speaker it was built for; presents as a firewall bug | `stream/config.rs::same_host` |
| Dropping the *newest* audio on overrun | Latency grows permanently instead of recovering | `audio/ring.rs` |
| Allocating in the capture callback | An audible click, on someone else's machine, intermittently | `audio/ring.rs`, proven by a counting allocator |
| Assuming a 32-bit WAV `data` field can describe a long session | It cannot — 6 h 45 m at CD stereo is the ceiling | `stream/wav.rs`, SPEC-002 §4 |
| Binding the stream server to `0.0.0.0` | System audio offered on the VPN and every other interface | `stream/config.rs` |

---

## 8. Dependencies

Adding one is a decision, not a convenience.

- It must pass `cargo deny`: permissive licence, no advisories, crates.io only.
- Prefer the standard library. Sixteen bytes of hex do not justify the `hex` crate.
- Prefer a well-known primitive over hand-rolling a hard one. We use `crossbeam-queue` for the
  lock-free ring rather than writing our own, and `subtle` for constant-time comparison rather
  than hoping a loop is not optimised away. Owning the *policy* is the goal; owning the
  *primitive* is a liability.
- Say in your summary what you added and what it replaced.

---

## 9. What to do when you are unsure

State the uncertainty and keep going with everything that does not depend on it. Then:

- **Ambiguous requirement** → implement the reading you judge correct, say which reading you
  chose and why, and propose the spec wording that would remove the ambiguity.
- **The spec is wrong** → say so plainly. This has already happened once: SPEC-002 originally
  required a WAV `data` chunk that outlasted 24 hours, which a 32-bit field cannot express. The
  right move was to correct the spec, add a new requirement for the real property, and say so —
  not to quietly implement something that could not satisfy it.
- **A test fails and you do not understand why** → do not weaken the assertion. Two of the first
  failures in this repo were genuine bugs the tests caught; one was an over-specified property.
  Work out which before touching either side.
- **The change would be large** → describe the plan before writing it.

---

## 10. Reporting your work

In your summary, state:

1. What you changed, and which requirements it touches.
2. The exact commands you ran and their outcome. If `cargo test` failed, paste the failure; do
   not report success for a suite you did not run.
3. Anything you skipped, and why.
4. Any spec change you are proposing.

Do not claim a build is green unless you ran it. On Windows, `cargo` may be blocked by Smart App
Control (see [`docs/development.md`](docs/development.md#smart-app-control)) — if so, say that
you could not build natively and what you ran instead, rather than assuming it would have
passed.

**Do not add AI attribution to commits or pull requests.** No `Co-Authored-By`, no "generated
with" footer. This project attributes work to the person who submitted it.
