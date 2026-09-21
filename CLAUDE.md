# CLAUDE.md

This project keeps its AI contributor contract in [`AGENTS.md`](AGENTS.md), so that it applies
to every assistant rather than to one vendor's.

**Read [`AGENTS.md`](AGENTS.md) before making any change.** It covers what to read first, the
definition of done, how tests declare the requirements they cover, the house style, and the
specific traps in this codebase that have already cost someone an afternoon.

Two things worth repeating here, because they are the most commonly violated:

1. **`just verify` must pass.** That is `fmt`, `clippy` at pedantic + nursery with `-D warnings`
   (tests included), the full test suite, and `spec-guard`. Do not report a change as done
   without running it, and do not report it as passing if you did not.
2. **No AI attribution in commits or pull requests.** No `Co-Authored-By`, no "generated with"
   footer.
