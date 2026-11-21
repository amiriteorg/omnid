//! Properties of address parsing over generated inputs: it never panics,
//! canonical forms are stable, and joining links keeps those promises.

use data_encoding::BASE32_NOPAD;
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

fn address() -> impl Strategy<Value = String> {
    prop_oneof![4 => url(), 1 => magnet(), 1 => freenet_key()]
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
    fn relative_paths_stay_on_the_same_site(base in prop_oneof![url(), freenet_key()], path in "[a-z0-9]{1,8}(/[a-z0-9]{1,8}){0,2}") {
        let base = Source::parse(&base).unwrap();
        let joined = base.join(&path).unwrap();
        prop_assert_eq!(joined.network(), base.network());
        prop_assert_eq!(joined.site_key(), base.site_key());
    }
}
