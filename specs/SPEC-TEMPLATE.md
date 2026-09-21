---
id: SPEC-NNN
title: <short imperative title>
status: draft            # draft | accepted | implemented | withdrawn
owner: <github handle>
crate: <crate or app this governs>
depends_on: []           # other SPEC ids
supersedes: null
last_reviewed: YYYY-MM-DD
---

# SPEC-NNN — <title>

## 1. Problem

What breaks or is impossible today. Written from the user's or the system's point of view,
not the implementation's. One paragraph.

## 2. Goals

- Concrete, observable outcomes.

## 3. Non-goals

- Explicitly out of scope. This section prevents scope creep more than any other.

## 4. Interface contract

The public surface this module exposes, as Rust signatures. This is the part other
modules may depend on; everything else is an implementation detail and may change freely.

```rust
// ...
```

## 5. Requirements

Each row is normative and testable. "SHOULD"/"MAY" statements belong in §7, not here.

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-XXX-001` | The system MUST ... | `unit` | `crate::module::tests::test_name` |

## 6. Failure modes

| Condition | Detection | Response | User-visible result |
| --------- | --------- | -------- | ------------------- |

## 7. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |

## 8. Security considerations

Trust boundaries crossed, untrusted inputs accepted, and the mitigation for each.
Cross-reference SPEC-007.

## 9. Minimum Viable Test (MVT)

The single demonstration that proves this module works. Must be executable by a
newcomer from a clean checkout with one command.

```bash
just mvt-<module>
```

## 10. Open questions

- [ ] ...
