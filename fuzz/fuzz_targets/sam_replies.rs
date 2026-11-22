#![no_main]

// What an I2P router's SAM bridge sends back.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::sam_replies(data));
