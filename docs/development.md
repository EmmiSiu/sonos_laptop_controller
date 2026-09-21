# Development environment

Everything you need to build Rincon, and the three environment problems that have actually
stopped people.

---

## The short version

```bash
git clone https://github.com/EmmiSiu/rincon
cd rincon
just verify        # fmt + clippy + tests + spec-guard, exactly as CI runs them
```

If that is green, you are set up correctly.

---

## Toolchain

| Tool | Version | Why |
| ---- | ------- | --- |
| Rust | stable; core 1.85+, desktop 1.88+ | Edition 2024, workspace lints, patched Tauri dependencies |
| Node | 20+ | The frontend build and `spec-guard` |
| `just` | any | The task runner. Optional: `scripts/verify.sh` does the same thing |

`rust-toolchain.toml` pins the channel, so `rustup` installs the right one on first `cargo`
invocation. The core MSRV is the workspace `rust-version` and CI builds against exactly it.
The Tauri shell is a separate workspace with MSRV 1.88: `plist` 1.9+ is the first release that
uses a non-vulnerable XML parser, and accepting its higher compiler floor is safer than pinning
the shipped desktop app to an advisory-bearing transitive dependency.

### Windows

Tauri and WASAPI both want the MSVC toolchain:

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
```

The Build Tools installer needs elevation. If you run it non-interactively and it exits with
**1602**, that is "user cancelled" — the UAC prompt appeared with nobody to answer it. Run it
from an elevated shell, or accept the prompt.

WebView2 is required for the desktop app and ships with Windows 11 and current Windows 10.

### Linux / macOS / WSL

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt-get install -y build-essential pkg-config     # Debian/Ubuntu
```

Every crate except the Windows capture backend builds and tests here. `rincon-audio` compiles
with its synthetic backend, so the whole workspace — including the stream server and the engine
— is exercised without a sound card.

---

## Smart App Control

**Symptom.** `cargo build` fails on any crate with a build script:

```
error: failed to run custom build command for `crossbeam-utils v0.8.23`
Caused by:
  could not execute process `…\target\debug\build\crossbeam-utils-…\build-script-build`
Caused by:
  A Control Application directive blocked this file. (os error 4551)
```

**Cause.** Windows **Smart App Control** blocks unsigned executables. Cargo compiles build
scripts into `target/` and runs them, and a freshly compiled `build-script-build.exe` is by
definition unsigned and reputation-less. This affects every Rust project on the machine, not
just Rincon.

Confirm it:

```powershell
(Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy" -Name VerifiedAndReputablePolicyState).VerifiedAndReputablePolicyState
# 0 = off, 1 = enforcement (this is the problem), 2 = evaluation
```

**Three ways out, in the order most people should try them:**

### 1. Build in WSL (recommended for day-to-day work)

Smart App Control does not apply inside WSL, and everything except the Windows capture backend
builds there.

```bash
wsl -d Ubuntu -u root -- apt-get update && apt-get install -y build-essential pkg-config
wsl -d Ubuntu -- bash -lc 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
wsl -d Ubuntu -- bash scripts/verify.sh
```

`scripts/verify.sh` puts the build directory on the WSL filesystem rather than on `/mnt/c`,
because the 9p mount is roughly ten times slower for the many small files cargo produces.

You get: the full test suite, clippy, `spec-guard`, and every crate but the loopback backend.
You do not get: a Windows binary, or a WASAPI capture you can hear.

### 2. Let CI produce the Windows binaries

`.github/workflows/ci.yml` builds and tests on `windows-latest`, where Smart App Control is not
enabled, and `release.yml` produces the installer. If you only occasionally need a Windows
build, this is the least invasive answer.

### 3. Turn Smart App Control off

> **Read this before you do it.** Smart App Control can be turned **off**, and it cannot be
> turned back **on** without reinstalling Windows. Microsoft documents this. It is a one-way
> door, and it is a real reduction in the machine's defences — not just for this project.

If you accept that: Windows Security → App & browser control → Smart App Control settings →
Off. Then `cargo build` works natively, including WASAPI loopback and the Tauri bundle.

For a machine that is primarily a development box, this is a defensible trade. For a machine
that is also someone's daily driver, option 1 is better.

---

## Working on the audio path

The capture backend needs real hardware, so most of the loop is:

```bash
cargo run -p rincon-cli -- discover          # find speakers
cargo run -p rincon-cli -- stream            # serve system audio, print the URL
cargo run -p rincon-cli -- play --room Kitchen
cargo run -p rincon-cli -- doctor            # interfaces, firewall, endpoint
```

Point VLC at the printed URL to check the audio path without involving a speaker. That isolates
"the capture and server are correct" from "the speaker accepts it", which are the two halves
that fail for completely different reasons.

## Working on the interface

```bash
cd apps/desktop
npm install
npm run tauri dev
```

With `RINCON_FAKE_DEVICES=1`, the backend answers `scan_devices` from `rincon-testkit` instead
of the network, so the whole interface — including the five-step connect stepper and every
failure state — is reachable without a speaker.

## Logs

```bash
RINCON_LOG=debug cargo run -p rincon-cli -- stream
RINCON_LOG=rincon_stream=trace,rincon_discovery=debug cargo run -p rincon-cli -- discover
```

Audio samples never appear in a log: buffers are wrapped in a type whose only rendering is
`<480 samples>`. The stream token never appears either; `StreamUrl` and `SessionToken` both
redact in `Debug` and `Display`, because `tracing` renders `Debug` for structured fields.

## Common problems

| Symptom | Cause | Fix |
| ------- | ----- | --- |
| `os error 4551` on build | Smart App Control | See above |
| Build Tools installer exits 1602 | UAC prompt unanswered | Run elevated |
| `discover` finds nothing, Sonos app works | AP client isolation, or probing the wrong interface | `rincon-cli doctor` lists what was probed |
| Speaker accepts `Play`, stays silent | Wrong interface bound — the URL is not routable from the speaker | Check `reached_via` in `doctor` output |
| `cargo test` hangs on `rincon-stream` | A test bound a port that a previous run leaked | The tests use ephemeral ports; a hang here is a bug worth reporting |
| Frontend types out of date | DTOs changed without regenerating | `just bindings` |
