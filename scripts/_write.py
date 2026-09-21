"""Shared helper: write UTF-8 with LF endings regardless of host platform."""
def write(path: str, text: str) -> None:
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)
