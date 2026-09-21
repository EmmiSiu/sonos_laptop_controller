# Rincon — Specification Suite

Rincon is built **spec-first**. No production code is written before the spec that
justifies it exists, is reviewed, and carries acceptance criteria that a machine can check.

## Why this directory is load-bearing

The specs are not documentation-after-the-fact. They are the contract that:

1. **Humans** review before implementation (cheap to change a spec, expensive to change shipped audio code).
2. **AI agents** read as the authoritative source of intent — see [`../AGENTS.md`](../AGENTS.md).
3. **CI enforces**: `scripts/spec-guard.mjs` fails the build when a requirement has
   no test covering it, or when a test references a requirement that no longer exists.

## Requirement identifiers

Every normative statement carries a stable ID:

```
RQ-<MODULE>-<NNN>
```

| Prefix   | Module            | Crate               |
| -------- | ----------------- | ------------------- |
| `CORE`   | Domain & contracts| `rincon-core`       |
| `DISC`   | Discovery         | `rincon-discovery`  |
| `CTL`    | Device control    | `rincon-control`    |
| `AUD`    | Audio capture     | `rincon-audio`      |
| `STRM`   | Stream server     | `rincon-stream`     |
| `ENG`    | Orchestration     | `rincon-engine`     |
| `UI`     | IPC & interface   | `apps/desktop`      |
| `SEC`    | Security          | cross-cutting       |
| `OBS`    | Observability     | cross-cutting       |

IDs are **append-only**. A withdrawn requirement is marked `status: withdrawn`, never deleted
and never reused — otherwise historical test names silently start meaning something else.

## Traceability contract

A requirement is *covered* when at least one test declares it:

```rust
/// Covers: RQ-STRM-004
#[test]
fn rejects_request_from_unallowlisted_peer() { /* ... */ }
```

Run the check locally exactly as CI does:

```bash
just spec-guard
```

## Verification levels

Each requirement declares how it is proven. The stronger the level, the more it costs —
so the level is chosen deliberately, not by default.

| Level      | Meaning                                                              |
| ---------- | -------------------------------------------------------------------- |
| `unit`     | Deterministic in-process test, no I/O.                                |
| `property` | `proptest` over a generated input domain (parsers, buffers, codecs).  |
| `golden`   | Byte-exact comparison against a committed fixture.                    |
| `integration` | Real sockets against `rincon-testkit` fakes; no physical hardware. |
| `bench`    | `criterion` benchmark with a regression threshold.                    |
| `fuzz`     | `cargo-fuzz` target; runs nightly, not per-PR.                        |
| `manual`   | Requires physical hardware. Recorded in `docs/manual-test-log.md`.    |

`manual` is a last resort. Every `manual` requirement must also name the fake that
approximates it in CI, so a regression is *likely* to be caught before hardware.

## Index

| Spec | Title | Status |
| ---- | ----- | ------ |
| [SPEC-000](SPEC-000-product.md) | Product definition & scope | accepted |
| [SPEC-001](SPEC-001-audio-capture.md) | Audio capture (WASAPI loopback) | accepted |
| [SPEC-002](SPEC-002-stream-server.md) | HTTP stream server | accepted |
| [SPEC-003](SPEC-003-discovery.md) | Device discovery (SSDP) | accepted |
| [SPEC-004](SPEC-004-control.md) | Device control (UPnP/SOAP) | accepted |
| [SPEC-005](SPEC-005-engine.md) | Session orchestration | accepted |
| [SPEC-006](SPEC-006-ipc-ui.md) | IPC boundary & interface | accepted |
| [SPEC-007](SPEC-007-security.md) | Security & threat model | accepted |
| [SPEC-008](SPEC-008-observability.md) | Observability & diagnostics | accepted |

## Changing a spec

1. Open a PR that touches **only** the spec. Title it `spec: ...`.
2. Get it reviewed. Implementation PRs that also edit specs will be asked to split.
3. Land the spec, then implement against it.

The friction is intentional: it is the mechanism that stops an implementation detail
from quietly becoming the requirement.
