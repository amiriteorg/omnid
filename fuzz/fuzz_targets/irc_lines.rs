#![no_main]

// What an IRC server sends: lines of messages, LIST's replies among them.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::irc_lines(data));
