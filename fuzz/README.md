# Fuzzing

Four targets, one per parser that reads bytes from the network:

| Target | Input | Why it is fuzzed |
| ------ | ----- | ---------------- |
| `parse_ssdp` | UDP datagram | Any LAN host can send this, unsolicited |
| `parse_description` | XML document | Served by whatever answered a probe |
| `parse_soap` | XML document | Every control response, including faults |
| `parse_topology` | XML document | Attribute-driven, a different code path |

## Running

```bash
rustup toolchain install nightly
cargo install cargo-fuzz

cd fuzz
cargo +nightly fuzz run parse_ssdp -- -max_total_time=300
```

## Why this is not on the pull-request path

A five-minute fuzz run finds nothing the property tests did not, and a run long enough to find
something new is longer than anyone will wait for a review. So each parser carries a cheap
"arbitrary bytes never panic" property that runs on every PR, and the thorough version runs
overnight in `.github/workflows/fuzz.yml`.

## When a target finds something

The workflow uploads the crashing input as an artifact. Reproduce it with:

```bash
cargo +nightly fuzz run parse_ssdp fuzz/artifacts/parse_ssdp/crash-<hash>
```

Then add it to the adversarial corpus in the parser's unit tests before fixing it, so the
regression is caught by `cargo test` rather than only by the next nightly run.
