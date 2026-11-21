//! Peak memory of parsing hostile info dicts. The info dict is parsed after
//! its SHA-1 matches, but whoever made the torrent chose those bytes, so a
//! crawled magnet can carry the worst input the parser accepts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator; only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes allocated at the high point of `f`, above what was live before it.
fn peak_of(f: impl FnOnce()) -> usize {
    let before = CURRENT.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    f();
    PEAK.load(Ordering::SeqCst) - before
}

/// An info dict whose `files` list repeats `entry` until it is `size` bytes.
fn padded_files(entry: &[u8], size: usize) -> Vec<u8> {
    let mut dict = b"d5:filesl".to_vec();
    while dict.len() < size {
        dict.extend_from_slice(entry);
    }
    dict.extend_from_slice(b"e4:name1:x12:piece lengthi16384ee");
    dict
}

/// One test, so no other test's allocations land in the counters.
#[test]
fn hostile_info_dicts_stay_small() {
    let ten_mib = 10 << 20; // DhtConfig::default().max_metadata_bytes
    let cases = [
        // Two bytes per value: empty lists under an unknown key.
        ("empty lists", {
            let mut dict = b"d6:lengthi1e4:name1:x12:piece lengthi16384e1:xl".to_vec();
            while dict.len() < ten_mib {
                dict.extend_from_slice(b"le");
            }
            dict.extend_from_slice(b"ee");
            dict
        }),
        // The most files per byte: each file is a dict, an int, a list and a string.
        ("tiny files", padded_files(b"d6:lengthi0e4:pathl1:aee", ten_mib)),
    ];
    for (name, dict) in cases {
        let peak = peak_of(|| drop(omnid_dht::parse_info_dict(&dict)));
        println!("{name}: {} bytes of input, peak {:.1} MiB", dict.len(), peak as f64 / (1 << 20) as f64);
        assert!(peak < 20 << 20, "{name}: peak {peak} bytes");
    }
}
