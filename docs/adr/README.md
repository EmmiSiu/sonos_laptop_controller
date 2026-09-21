# Architecture Decision Records

One file per decision that was not obvious, written when it was made rather than reconstructed
later.

The value is not the decision — that is visible in the code. It is the **alternative that was
rejected and why**, which is invisible in the code and is the first thing anyone asks six
months later.

| # | Decision | Status |
| - | -------- | ------ |
| [0001](0001-rust-for-the-core.md) | Rust for the core | accepted |
| [0002](0002-tauri-over-electron.md) | Tauri rather than Electron | accepted |
| [0003](0003-hand-rolled-upnp.md) | Hand-rolled UPnP rather than a framework | accepted |
| [0004](0004-test-only-allocation-probe.md) | One `unsafe` exemption, for a test-only allocator | accepted |
| [0005](0005-pure-reducer-orchestration.md) | Orchestration as a pure reducer | accepted |
| [0006](0006-no-tls-on-the-stream.md) | No TLS on the audio stream | accepted |

## Writing one

Copy the shape of an existing record: context, the options considered, the decision, the
consequences including the bad ones, and what would make us revisit it.

A record that lists only the option we picked is not an ADR, it is a description.
