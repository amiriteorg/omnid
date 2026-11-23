#![no_main]

// What a Nostr relay's WebSocket server sends after the handshake: frames,
// fragments, pings and closes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnid_net::fuzzing::ws_frames(data));
