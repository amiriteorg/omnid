#![no_main]

// What a BitTorrent peer sends during a metadata exchange.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_dht::fuzzing::peer_wire(data));
