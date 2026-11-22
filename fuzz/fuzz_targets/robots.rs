#![no_main]

// A robots.txt: the input's first line is the path asked about, the rest the file.

use libfuzzer_sys::fuzz_target;
use omnid_core::Source;

fuzz_target!(|data: &[u8]| {
    let (path, body) = match data.iter().position(|&b| b == b'\n') {
        Some(end) => (&data[..end], &data[end + 1..]),
        None => (&data[..0], data),
    };
    let Ok(page) = Source::parse(&format!("http://example.com/{}", String::from_utf8_lossy(path))) else { return };
    let agent = omnid_crawl::CrawlConfig::default().robots_agent;
    let _ = omnid_crawl::fuzzing::robots(&agent, body, &page.to_string());
});
