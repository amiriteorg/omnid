#![no_main]

// A server's answer to the crawler's GET over a Tor or I2P stream.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::http_response(data));
