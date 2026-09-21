#!/usr/bin/env bash
# Local mirror of the CI `lint` + `test` jobs.
#
# On Windows, Smart App Control blocks the unsigned build scripts cargo generates, so the
# native toolchain cannot build crates with build scripts. Run this inside WSL instead:
#
#     wsl -d Ubuntu -- bash scripts/verify.sh
#
# See docs/development.md for the full explanation and the alternatives.
set -euo pipefail

if [[ -f "$HOME/.cargo/env" ]]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

# Keep build artefacts off the 9p mount: it is roughly ten times slower than ext4.
: "${CARGO_TARGET_DIR:=$HOME/.cache/rincon-target}"
export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"

cd "$(dirname "$0")/.."

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }

step "cargo fmt --check"
cargo fmt --all -- --check

step "cargo clippy (pedantic + nursery, warnings are errors)"
cargo clippy --workspace --all-targets --all-features -- -D warnings

step "cargo test"
cargo test --workspace --all-features

step "spec-guard: every requirement is covered by a test"
if command -v node >/dev/null 2>&1; then
  node scripts/spec-guard.mjs
else
  echo "node not found; skipping spec-guard (CI still enforces it)"
fi

printf '\n\033[1;32mAll checks passed.\033[0m\n'
