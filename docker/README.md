# Docker

Two things live here, and neither is how Rincon ships.

## `gate`

The CI environment, reproduced. Use it to debug a CI failure without guessing at toolchain
versions, or to run the full quality gate on a Windows machine where Smart App Control blocks
cargo from executing build scripts (see [`../docs/development.md`](../docs/development.md)).

```bash
docker compose -f docker/compose.yaml run --rm gate
```

## `mock-sonos`

A fake speaker on a container network, for exercising discovery and control from outside the
Rust test harness.

```bash
docker compose -f docker/compose.yaml up mock-sonos
```

## What Docker cannot do here

**Capture audio.** WASAPI is a Windows API, and a container has no sound device. The loopback
backend is absent from these images by design.

That is not a gap, it is the architecture working: everything above the `AudioCapture` trait —
discovery, control, the stream server, the whole session engine — runs and is tested here,
because each hardware dependency sits behind a trait with a fake. The capture backend itself is
the one thing that needs real hardware, and it is verified against the checklist in
[`../docs/manual-test-log.md`](../docs/manual-test-log.md).

Rincon is a desktop application. It is not distributed as a container and never will be: a
container that could reach your speakers and your sound card would need `--net=host` and
`--device /dev/snd`, at which point you have installed the app with extra steps.
