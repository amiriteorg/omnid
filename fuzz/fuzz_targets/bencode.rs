#![no_main]

// Bencode as the DHT client decodes it, under each of its limits.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_dht::fuzzing::bencode(data));
