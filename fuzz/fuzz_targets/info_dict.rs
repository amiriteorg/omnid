#![no_main]

// A torrent's info dict, as a peer sends it or a .torrent file holds it.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(meta) = omnid_dht::parse_info_dict(data) else { return };
    assert!(meta.piece_length > 0);
    assert_eq!(meta.total_size, meta.files.iter().map(|f| f.size).sum::<u64>());
    // Names end up in logs, terminals and the UI.
    for text in std::iter::once(&meta.name).chain(meta.files.iter().map(|f| &f.path)) {
        assert!(!text.chars().any(char::is_control), "{text:?}");
    }
});
