#!/usr/bin/env python3
"""Normalise the `allow` preamble of every `#[cfg(test)] mod tests` in the workspace.

Production code is held to `clippy::pedantic` + `nursery` with `-D warnings`. Test code
legitimately panics (that is what an assertion is) and does lossy numeric casts to build
fixtures, so it gets one consistent, reviewable exemption list instead of ad-hoc
`#[allow]`s scattered through the suites.
"""
import pathlib
import re
import sys

CANONICAL = """    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]"""

# Matches the existing inner-attribute block right after `mod tests {`.
PATTERN = re.compile(
    r"(#\[cfg\(test\)\]\s*\nmod tests \{\n)(    #!\[allow\([^)]*\)\]\n)?",
    re.MULTILINE,
)

def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    changed = []
    for path in list(root.glob("crates/**/*.rs")) + list(root.glob("apps/**/src-tauri/**/*.rs")):
        if "target" in path.parts:
            continue
        original = path.read_text(encoding="utf-8")
        updated = PATTERN.sub(lambda m: m.group(1) + CANONICAL + "\n", original)
        if updated != original:
            path.write_text(updated, encoding="utf-8")
            changed.append(path.relative_to(root).as_posix())
    for name in changed:
        print(f"normalised {name}")
    print(f"{len(changed)} file(s) updated")
    return 0

if __name__ == "__main__":
    sys.exit(main())
