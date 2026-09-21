#![no_main]

//! Fuzzes the SOAP response parser.
//!
//! A speaker -- or something pretending to be one -- answers every control command with
//! a document this parser reads, including the fault bodies that carry error detail.
//!
//! Covers: RQ-SEC-004

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = rincon_control::soap::parse_response(text);
});
