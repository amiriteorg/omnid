#![no_main]

// A fetched page: the input's first byte picks its Content-Type, the rest is the
// body. What comes out stays within extract.rs's caps.

use libfuzzer_sys::fuzz_target;
use omnid_crawl::extract::extract;

const TYPES: [Option<&str>; 11] = [
    None,
    Some("text/html"),
    Some("text/html; charset=windows-1252"),
    Some("text/html; charset=\"shift_jis\""),
    Some("application/xhtml+xml"),
    Some("text/plain"),
    Some("text/plain; charset=utf-16le"),
    Some("text/plain; charset=bogus"),
    Some("text/gemini"),
    Some("text/gemini; charset=iso-8859-1; lang=fr"),
    Some("text/x-gopher-menu"),
];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else { return };
    let page = extract(TYPES[usize::from(pick) % TYPES.len()], body);
    assert!(page.title.len() <= 300 && page.description.len() <= 500 && page.text.len() <= 100_000);
    assert!(page.keywords.len() <= 32 && page.links.len() <= 20_000);
});
