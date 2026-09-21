#![no_main]

//! Fuzzes the SSDP datagram parser.
//!
//! Any host on the LAN can send this parser arbitrary bytes over UDP, unauthenticated
//! and unsolicited. A panic here is a remote denial of service against the app.
//!
//! Covers: RQ-SEC-004

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The property is simply that this returns. Any `Err` is a fine outcome; unwinding is not.
    let _ = rincon_discovery::ssdp::parse_response(data);
    let _ = rincon_discovery::ssdp::parse_headers(data);
});
