#![no_main]

// Anything a user might paste as an address. What parses has a canonical form that
// parses back to the same source, and is its own canonical form.

use libfuzzer_sys::fuzz_target;
use omnid_core::Source;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let Ok(source) = Source::parse(input) else { return };
    let canonical = source.to_string();
    let again = Source::parse(&canonical).unwrap_or_else(|e| panic!("{canonical:?} does not parse: {e}"));
    assert_eq!(again, source, "{input:?} became {canonical:?}");
    assert_eq!(again.to_string(), canonical);
    let _ = (source.site_key(), source.robots_txt());
});
