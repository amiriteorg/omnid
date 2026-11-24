#![no_main]

// A Gemini server's header line: two digits, a space, a meta, CRLF.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::gemini_header(data));
