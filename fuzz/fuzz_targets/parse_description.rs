#![no_main]

//! Fuzzes the UPnP device description parser.
//!
//! The document arrives from whatever answered a multicast probe. It is XML, it is
//! attacker-controlled, and it is parsed before anything about the device is known.
//!
//! Covers: RQ-SEC-004

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = rincon_discovery::description::parse(text);
});
