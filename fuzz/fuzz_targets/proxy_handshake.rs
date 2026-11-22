#![no_main]

// A client of the SOCKS5 and HTTP proxy, up to the request it makes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid::fuzzing::proxy_handshake(data));
