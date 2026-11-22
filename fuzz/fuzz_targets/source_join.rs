#![no_main]

// A link found on a page: the input's first line is the page's address, the rest
// the link as written. Where the link leads parses back from its canonical form,
// and that form, as a link on the same page, leads there too.

use libfuzzer_sys::fuzz_target;
use omnid_core::Source;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let (page, href) = input.split_once('\n').unwrap_or((input, ""));
    let Ok(page) = Source::parse(page) else { return };
    let Some(joined) = page.join(href) else { return };
    let canonical = joined.to_string();
    assert_eq!(Source::parse(&canonical).as_ref(), Ok(&joined), "{href:?} on {page} became {canonical:?}");
    assert_eq!(page.join(&canonical).as_ref(), Some(&joined), "{canonical:?} on {page}");
});
