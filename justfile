# Rincon task runner.
#
# `just` is the canonical entry point, but nothing here is magic: every recipe is a command you
# could type. If `just` is not installed, `scripts/verify.sh` runs the same gate.
#
#   just              list every recipe
#   just verify       the full quality gate, in CI's order
#   just mvt-audio    the Minimum Viable Test for one module

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Keep build artefacts off a 9p mount when running under WSL: it is roughly ten times slower
# for the many small files cargo produces.
export CARGO_TERM_COLOR := "always"

# Default recipe: show what is available.
default:
    @just --list --unsorted

# ---------------------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------------------

# Everything CI runs on every platform, in CI's order.
#
# The desktop shell is not in here: it is a separate workspace that needs webkit2gtk on Linux,
# and requiring that to run the gate would make the gate something people skip. CI builds it on
# Windows and macOS; run `just shell` if you touched it.
verify: fmt-check lint test spec-guard frontend
    @echo "All checks passed. Run `just shell` too if you changed apps/desktop/src-tauri."

# Reformat everything.
fmt:
    cargo fmt --all

# Fail if anything is unformatted.
fmt-check:
    cargo fmt --all -- --check

# Clippy at pedantic + nursery, warnings as errors, tests included.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# The whole Rust suite.
test:
    cargo test --workspace --all-features

# One crate's tests, e.g. `just test-one rincon-stream`.
test-one crate:
    cargo test -p {{crate}} --all-features -- --nocapture

# Every requirement has a test, and every structural claim holds.
spec-guard:
    node scripts/spec-guard.mjs

# Rewrite the "Covered by" columns from the annotations that actually exist.
spec-sync:
    node scripts/spec-guard.mjs --write

# Typecheck, test, and build the interface.
frontend:
    cd apps/desktop && npm run typecheck && npm test && npx vite build

# Regenerate the TypeScript types from the Rust DTOs.
bindings:
    cargo run -q -p rincon-ipc --example bindings

# Lint the desktop shell. Its own workspace, so the root recipes do not reach it.
shell:
    cd apps/desktop/src-tauri && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings

# Regenerate the app icon from `scripts/make-icon.py`, then fan it out to every format.
icon:
    python scripts/make-icon.py
    cd apps/desktop && npx tauri icon src-tauri/icons/source.png

# ---------------------------------------------------------------------------------------
# Minimum Viable Tests — the demonstration each spec asks for
# ---------------------------------------------------------------------------------------

# SPEC-003: list the speakers on this network.
mvt-discovery:
    cargo run -p rincon-cli -- discover

# SPEC-001 + SPEC-002: serve system audio and print a URL to open in VLC.
mvt-stream:
    cargo run -p rincon-cli -- stream

# SPEC-004: play on a real speaker. `just mvt-control Kitchen`
mvt-control room:
    cargo run -p rincon-cli -- play --room "{{room}}"

# SPEC-005: a full session against fakes, with the leak assertions.
mvt-engine:
    cargo test -p rincon-engine rq_eng_010 -- --nocapture
    cargo test -p rincon-engine rq_eng_011 -- --nocapture

# SPEC-008: build a redacted diagnostics bundle.
mvt-diagnostics:
    cargo test -p rincon-core rq_obs_006 -- --nocapture
    cargo test -p rincon-core rq_obs_007 -- --nocapture

# SPEC-006: the interface, driven by fakes instead of the network.
mvt-ui:
    cd apps/desktop && npm run tauri dev

# ---------------------------------------------------------------------------------------
# Development
# ---------------------------------------------------------------------------------------

# Run the desktop app.
dev:
    cd apps/desktop && npm run tauri dev

# Run the desktop app against fakes: no speaker, no sound card.
dev-fake:
    cd apps/desktop && $env:RINCON_SYNTHETIC = "1"; npm run tauri dev

# What the network and the audio endpoint actually look like from here.
doctor:
    cargo run -p rincon-cli -- doctor

# Build the release installer.
bundle:
    cd apps/desktop && npm run tauri build

# Open the API documentation.
docs:
    cargo doc --workspace --no-deps --all-features --open

# ---------------------------------------------------------------------------------------
# Supply chain
# ---------------------------------------------------------------------------------------

# Licences, advisories, sources, and bans. Feature selection comes from deny.toml's [graph].
deny:
    cargo deny check

# Known vulnerabilities in the dependency tree.
audit:
    cargo audit --deny warnings
    cargo audit --deny warnings --file apps/desktop/src-tauri/Cargo.lock --ignore RUSTSEC-2024-0370 --ignore RUSTSEC-2025-0075 --ignore RUSTSEC-2025-0080 --ignore RUSTSEC-2025-0081 --ignore RUSTSEC-2025-0098 --ignore RUSTSEC-2025-0100 --ignore RUSTSEC-2024-0429
    cd apps/desktop && npm audit --audit-level=high

# Fuzz one parser for a while. `just fuzz parse_ssdp 300`
fuzz target seconds="60":
    cd fuzz && cargo +nightly fuzz run {{target}} -- -max_total_time={{seconds}}

# ---------------------------------------------------------------------------------------
# Escape hatches
# ---------------------------------------------------------------------------------------

# Run the whole gate inside WSL. Use this when Smart App Control blocks cargo on Windows;
# see docs/development.md.
verify-wsl:
    wsl -d Ubuntu -- bash scripts/verify.sh

# Remove every build artefact.
clean:
    cargo clean
    cd apps/desktop && rm -rf dist node_modules/.vite
