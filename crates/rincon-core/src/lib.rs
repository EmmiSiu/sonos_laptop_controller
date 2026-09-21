//! Domain types, validators, and cross-cutting contracts shared by every Rincon crate.
//!
//! `rincon-core` performs no I/O. It holds the vocabulary the rest of the workspace speaks —
//! devices, audio formats, stream URLs — plus the validators that guard every trust boundary
//! described in [SPEC-007](../../../specs/SPEC-007-security.md).
//!
//! Keeping this crate I/O-free is deliberate: it is the only crate every other crate depends on,
//! so it is the one place where a hidden network call or a blocking read would be invisible in
//! review. It also means the entire domain layer is testable in microseconds.
//!
//! # Module map
//!
//! | Module | Responsibility |
//! | ------ | -------------- |
//! | [`audio`] | Sample formats and negotiated stream parameters |
//! | [`device`] | Validated device identity and addressing |
//! | [`diagnostics`] | Redacted, offline support bundles |
//! | [`error`] | The shared error vocabulary |
//! | [`limits`] | Byte caps and timeouts applied to every network read |
//! | [`metrics`] | Lock-free counters and the derived health model |
//! | [`net`] | Private-address validation (the SSRF guard) |
//! | [`stream_url`] | The URL handed to a speaker, with the token redacted in logs |
//! | [`telemetry`] | Logging setup with size-capped rotation |
//! | [`xml`] | DOCTYPE/entity rejection applied before any XML parse |

pub mod audio;
pub mod device;
pub mod diagnostics;
pub mod error;
pub mod limits;
pub mod metrics;
pub mod net;
pub mod stream_url;
pub mod telemetry;
pub mod xml;

pub use audio::{AudioFormat, ChannelCount, SampleEncoding, SampleRate};
pub use device::{Device, DeviceId, DeviceName};
pub use error::{CoreError, Result};
pub use metrics::{DropLayer, Health, SessionCounters};
pub use net::{NetError, ensure_lan_destination, is_lan_reachable};
pub use stream_url::StreamUrl;
pub use xml::{XmlGuardError, reject_doctype};

/// The product name, used in user agents, log headers, and diagnostics bundles.
pub const PRODUCT: &str = "Rincon";

/// The crate version, surfaced in diagnostics bundles.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
