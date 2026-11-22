//! Properties of address parsing over generated inputs: it never panics,
//! canonical forms are stable, and joining links keeps those promises.

use data_encoding::{BASE32_NOPAD, HEXLOWER};
use omnid_core::nostr::{Nostr, NostrTarget};
use omnid_core::{Network, Source};
use proptest::prelude::*;

/// A valid-looking v3 onion: 32 key bytes, 2 checksum bytes, version 3.
fn onion() -> impl Strategy<Value = String> {
    prop::array::uniform32(any::<u8>()).prop_flat_map(|key| {
        any::<[u8; 2]>().prop_map(move |checksum| {
            let mut raw = key.to_vec();
            raw.extend(checksum);
            raw.push(3);
            format!("{}.onion", BASE32_NOPAD.encode(&raw).to_ascii_lowercase())
        })
    })
}

fn host() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z][a-z0-9]{0,10}\\.(com|org|example|net)",
        "[a-z][a-z0-9]{0,10}\\.i2p",
        onion(),
        (1u8..=223, any::<u8>(), any::<u8>(), 1u8..=254).prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
    ]
}

fn url() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("http"), Just("https")],
        host(),
        prop::option::of(1u16..=65535),
        "(/[a-zA-Z0-9_.~-]{0,8}){0,4}/?",
        prop::option::of("[a-z]{1,5}=[a-z0-9]{0,5}"),
    )
        .prop_map(|(scheme, host, port, path, query)| {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let query = query.map(|q| format!("?{q}")).unwrap_or_default();
            format!("{scheme}://{host}{port}{path}{query}")
        })
}

fn magnet() -> impl Strategy<Value = String> {
    (any::<[u8; 20]>(), prop::option::of("[A-Za-z0-9 ]{1,20}")).prop_map(|(hash, name)| {
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        match name {
            Some(name) => format!("magnet:?xt=urn:btih:{hex}&dn={}", name.replace(' ', "+")),
            None => format!("magnet:?xt=urn:btih:{hex}"),
        }
    })
}

fn freenet_key() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("USK"), Just("SSK"), Just("CHK")],
        "[A-Za-z0-9~-]{43},[A-Za-z0-9~-]{43},AQACAAE",
        "(/[a-z0-9-]{1,10}){1,3}/?",
    )
        .prop_map(|(kind, key, path)| format!("freenet:{kind}@{key}{path}"))
}

/// Bytes in base 58 or 36, most significant digit first, a leading zero
/// byte as a leading zero digit.
fn base_x(bytes: &[u8], alphabet: &[u8]) -> String {
    let mut digits: Vec<u32> = Vec::new();
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for digit in &mut digits {
            carry += *digit << 8;
            *digit = carry % alphabet.len() as u32;
            carry /= alphabet.len() as u32;
        }
        while carry > 0 {
            digits.push(carry % alphabet.len() as u32);
            carry /= alphabet.len() as u32;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let lead = std::iter::repeat_n(alphabet[0] as char, zeros);
    lead.chain(digits.iter().rev().map(|&d| alphabet[d as usize] as char)).collect()
}

const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// A sha2-256 CID's digest, codec (dag-pb or raw) and the base it is
/// written in: 0 is CIDv0 where the codec allows it.
fn cid_parts() -> impl Strategy<Value = ([u8; 32], u8, u8)> {
    (any::<[u8; 32]>(), prop_oneof![Just(0x70u8), Just(0x55u8)], 0u8..5)
}

fn write_cid((digest, codec, base): ([u8; 32], u8, u8)) -> String {
    let multihash = [&[0x12, 0x20][..], &digest].concat();
    let cid = [&[0x01, codec][..], &multihash].concat();
    match base {
        0 if codec == 0x70 => base_x(&multihash, BASE58),
        0 | 1 => format!("b{}", BASE32_NOPAD.encode(&cid).to_ascii_lowercase()),
        2 => format!("B{}", BASE32_NOPAD.encode(&cid)),
        3 => format!("f{}", HEXLOWER.encode(&cid)),
        _ => format!("k{}", base_x(&cid, BASE36)),
    }
}

fn ipfs() -> impl Strategy<Value = String> {
    let path = "(/[a-zA-Z0-9_.~-]{1,8}){0,3}/?";
    prop_oneof![
        (cid_parts(), path, 0u8..4).prop_map(|(cid, path, form)| {
            let cid = write_cid(cid);
            match form {
                0 => format!("ipfs://{cid}{path}"),
                1 => format!("/ipfs/{cid}{path}"),
                2 => format!("https://ipfs.io/ipfs/{cid}{path}"),
                _ => format!("{cid}{path}"),
            }
        }),
        ("[a-z][a-z0-9-]{0,8}[a-z0-9]\\.(org|tech|io)", path).prop_map(|(name, path)| format!("ipns://{name}{path}")),
    ]
}

/// A note, a profile or an article, written as `nostr:` URIs or bare.
fn nostr() -> impl Strategy<Value = String> {
    let target = prop_oneof![
        any::<[u8; 32]>().prop_map(NostrTarget::Event),
        any::<[u8; 32]>().prop_map(NostrTarget::Profile),
        (any::<u32>(), any::<[u8; 32]>(), "[a-z0-9-]{0,20}")
            .prop_map(|(kind, author, identifier)| NostrTarget::Address { kind, author, identifier }),
    ];
    (target, any::<bool>()).prop_map(|(target, bare)| {
        let uri = Nostr { target, relays: vec![] }.to_string();
        if bare { uri.trim_start_matches("nostr:").to_owned() } else { uri }
    })
}

fn address() -> impl Strategy<Value = String> {
    prop_oneof![4 => url(), 1 => magnet(), 1 => freenet_key(), 1 => ipfs(), 1 => nostr()]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn parsing_never_panics(input in "\\PC{0,200}") {
        let _ = Source::parse(&input);
    }

    #[test]
    fn canonical_forms_parse_back_to_themselves(input in address()) {
        let source = Source::parse(&input).unwrap();
        let canonical = source.to_string();
        prop_assert_eq!(Source::parse(&canonical).unwrap(), source.clone(), "{}", canonical);
        prop_assert_eq!(Source::parse(&canonical).unwrap().to_string(), canonical);
    }

    #[test]
    fn hosts_decide_the_network(input in url()) {
        let source = Source::parse(&input).unwrap();
        let host = source.url().unwrap().host_str().unwrap().to_owned();
        let expected = if host.ends_with(".onion") {
            Network::Onion
        } else if host.ends_with(".i2p") {
            Network::I2p
        } else {
            Network::Clearnet
        };
        prop_assert_eq!(source.network(), expected);
    }

    #[test]
    fn joining_never_panics_and_yields_canonical_sources(base in address(), href in "\\PC{0,80}") {
        let base = Source::parse(&base).unwrap();
        if let Some(joined) = base.join(&href) {
            let canonical = joined.to_string();
            prop_assert_eq!(Source::parse(&canonical).unwrap(), joined);
        }
    }

    #[test]
    fn a_cid_in_any_base_is_one_address(parts in cid_parts(), other in 0u8..5) {
        let (digest, codec, _) = parts;
        let a = Source::parse(&format!("ipfs://{}/", write_cid(parts))).unwrap();
        let b = Source::parse(&format!("ipfs://{}/", write_cid((digest, codec, other)))).unwrap();
        prop_assert_eq!(&a, &b);
        prop_assert!(a.to_string().starts_with("ipfs://b"), "{}", a);
    }

    #[test]
    fn relative_paths_stay_on_the_same_site(base in prop_oneof![url(), freenet_key(), ipfs()], path in "[a-z0-9]{1,8}(/[a-z0-9]{1,8}){0,2}") {
        let base = Source::parse(&base).unwrap();
        let joined = base.join(&path).unwrap();
        prop_assert_eq!(joined.network(), base.network());
        prop_assert_eq!(joined.site_key(), base.site_key());
    }
}
