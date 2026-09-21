#![no_main]

//! Fuzzes the zone-group topology parser.
//!
//! Topology is attribute-driven rather than element-driven, so it exercises a different
//! path through the XML reader than the description parser does.
//!
//! Covers: RQ-SEC-004

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = rincon_control::topology::parse(text);
});
