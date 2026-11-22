#![no_main]

// The Content-Range and Content-Disposition of a download's answer.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::download_headers(data));
