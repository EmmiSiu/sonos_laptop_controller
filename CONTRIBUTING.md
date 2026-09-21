# Contributing to Rincon

Thanks for looking. This document is short on ceremony and specific about the two things that
are unusual here: the spec-first workflow, and the lint bar.

If you are an AI assistant, read [`AGENTS.md`](AGENTS.md) instead — it says the same things with
the detail a model needs.

## Before you start

```bash
git clone https://github.com/EmmiSiu/rincon
cd rincon
just verify      # or: bash scripts/verify.sh
```

If that passes, your environment is correct. If it does not,
[`docs/development.md`](docs/development.md) covers the known environment problems, including
the Windows Smart App Control issue that blocks `cargo` from running build scripts.

## The workflow

Rincon is spec-first. That is not bureaucracy; it is the mechanism that keeps a project with
this much hidden complexity — real-time audio, an undocumented device protocol, an open port on
a home network — reviewable by one person on a weekend.

```
1. Is there a requirement for what you want to build?
   ├── yes → implement it, annotate your tests, open a PR
   └── no  → open a PR that changes only the spec, titled `spec: ...`
             get it reviewed, land it, then implement
```

Implementation PRs that also edit a spec will be asked to split. The friction is deliberate: it
is what stops an implementation detail from quietly becoming the requirement.

**Exception:** if you discover while implementing that a requirement is *wrong* — not merely
inconvenient — fix the spec in the same PR and say so prominently. That has already happened
once (`RQ-STRM-002`, which required something a 32-bit field cannot express), and catching it
was worth more than the process.

## Tests

Every test that proves a requirement declares it:

```rust
/// Covers: RQ-STRM-004
#[tokio::test]
async fn rq_strm_004_rejects_unallowlisted_peer() { /* ... */ }
```

`scripts/spec-guard.mjs` fails the build if a requirement has no test, or if a test names a
requirement that no longer exists. Run `just spec-guard` to check, or `just spec-sync` to
rewrite the "Covered by" columns from the annotations that actually exist.

Some conventions that make tests here useful rather than decorative:

- Assert the **consequence**, not just the status code. "Returns 403" is half of
  `RQ-STRM-004`; "and no audio was served" is the other half.
- Put the diagnosis in the assertion message. The next person to see the failure will not have
  your context.
- Use a property test for anything that parses or converts. Hostile input is generated better
  than it is imagined.
- Use a golden fixture for anything that goes on the wire. One wrong byte in a WAV header or a
  SOAP envelope produces silence with no error anywhere.
- Use the fakes in `rincon-testkit` and `rincon-audio::synthetic`. A test that needs a speaker
  in the room never runs in CI, which is where regressions are actually caught.

## The lint bar

`clippy::pedantic` + `clippy::nursery`, with `-D warnings`, **including in tests**.

This is stricter than most projects and it is deliberate. The lints that bite here —
`unwrap_used`, `panic`, `indexing_slicing`, `float_cmp` — each correspond to a way this specific
program fails audibly in someone's living room, usually intermittently, usually on a machine
you do not have.

When a lint is genuinely wrong for your code, in order of preference:

1. Restructure so it does not apply.
2. `#[expect(lint, reason = "…")]` with a reason that states the invariant. `#[expect]` rather
   than `#[allow]`, so the build tells you when the exemption stops being needed.
3. Argue in the PR for removing the lint from the workspace, naming the class of code it is
   wrong about.

Please do not add a bare `#[allow]`.

## Commits and pull requests

- Conventional-ish prefixes: `feat:`, `fix:`, `spec:`, `docs:`, `test:`, `chore:`, `refactor:`.
- One logical change per PR. A 2000-line PR does not get a real review, it gets an approval.
- The PR description says what changed, which requirements it touches, and what you ran.
- No AI attribution lines in commits or PR bodies.

## What gets rejected

Not to be discouraging — these are simply the things that reliably come back:

- A new dependency without a reason it beats the standard library or an existing one.
- `String` errors. Every failure here is a typed variant with a user-facing sentence.
- Validation at a call site where an invariant in a type would have worked.
- A feature that contradicts a non-goal in [`SPEC-000 §3`](specs/SPEC-000-product.md) without
  first changing that non-goal.
- Anything that phones home. Rincon makes no outbound internet connection, and that is a
  product promise, not an oversight.

## Where to start

- Issues labelled `good first issue`.
- The open questions at the bottom of each spec — several need someone with real hardware.
- [`docs/manual-test-log.md`](docs/manual-test-log.md) lists the checks CI cannot do. Running
  them on a speaker model nobody has tested is genuinely valuable.
