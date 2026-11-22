#![no_main]

// A body under Content-Encoding, decoded no further than the download cap.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::http_decode(data));
