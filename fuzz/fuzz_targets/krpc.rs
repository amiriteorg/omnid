#![no_main]

// A KRPC datagram from any DHT node.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_dht::fuzzing::krpc(data));
