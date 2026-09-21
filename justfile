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

# Everything CI runs, in CI's order. Green here means green there.
verify: fmt-check lint test spec-guard frontend
    @echo "All checks passed."

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
    cargo test -p rincon-engine -- --nocapture rq_eng_010 rq_eng_011

# SPEC-008: build a redacted diagnostics bundle.
mvt-diagnostics:
    cargo test -p rincon-core -- --nocapture rq_obs_006 rq_obs_007

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

# Licences, advisories, sources, and bans.
deny:
    cargo deny check --all-features

# Known vulnerabilities in the dependency tree.
audit:
    cargo audit --deny warnings
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
