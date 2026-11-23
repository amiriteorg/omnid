#![no_main]

// A Nostr event as the crawler indexes it. A signature costs a writer nothing,
// so the event and a channel's messages are taken as given, unchecked. The
// first byte picks the kind among those the crawler treats apart.

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use omnid_net::NostrEvent;

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let Ok((pick, any, id, pubkey, tags, content)) =
        <(u8, u32, [u8; 32], [u8; 32], Vec<Vec<String>>, String)>::arbitrary(&mut input)
    else {
        return;
    };
    let kind = [0, 1, 40, 42, 30023, 30000 + any % 10000, any][usize::from(pick) % 7];
    let event = NostrEvent { id, pubkey, created_at: 1, kind, tags, content };
    let messages: Vec<NostrEvent> = Vec::<String>::arbitrary(&mut input)
        .unwrap_or_default()
        .into_iter()
        .map(|content| NostrEvent { kind: 42, tags: vec![], content, ..event.clone() })
        .collect();
    omnid_crawl::fuzzing::nostr_fields(&event, &messages);
});
