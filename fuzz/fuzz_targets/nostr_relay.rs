#![no_main]

// A Nostr relay's answer to a subscription, one message a line.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::nostr_relay(data));
