//! IPFS addresses: content identifiers (CIDs) under `/ipfs/` and IPNS names
//! under `/ipns/`, each in the one form the index keys it by.
//!
//! - A CID is canonical as CIDv1 in base32 (`bafy…`), the form subdomain
//!   gateways use, so a CIDv0 (`Qm…`) and the same CID in any other base are
//!   one address.
//! - An IPNS key is canonical as a CIDv1 in base36 (`k51…`), and so is a
//!   legacy peer ID (`12D3Koo…`, `Qm…`) naming the same key.
//! - A DNSLink name (`docs.ipfs.tech`) is canonical as its lowercase hostname.
//!
//! Only the structure is checked: the multibase, the version and the
//! multihash's length. Whether the content matches is the gateway's to verify.

use data_encoding::{BASE32_NOPAD, HEXLOWER_PERMISSIVE};

/// Longer than any CID with a [`MAX_DIGEST`]-byte digest, in any base; also
/// bounds the quadratic base-58 and base-36 conversions.
const MAX_ID: usize = 256;
/// The largest digest taken, kubo's limit for identity (inlined) CIDs.
const MAX_DIGEST: u64 = 128;
const DAG_PB: u8 = 0x70;
const LIBP2P_KEY: u8 = 0x72;
const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// Top-level names no DNSLink lookup should be made for: they are not in the
/// DNS, and asking for one would tell the resolver which site was wanted.
const NOT_DNS: [&str; 4] = ["onion", "i2p", "localhost", "local"];

/// Public gateways that serve `/ipfs/<CID>/…` and `/ipns/<name>/…` paths, or
/// `<CID>.ipfs.<gateway>` subdomains. A link through one is the content
/// itself, which is fetched from the configured gateway instead.
pub(crate) const KNOWN_GATEWAYS: [&str; 10] = [
    "ipfs.io",
    "dweb.link",
    "gateway.ipfs.io",
    "cloudflare-ipfs.com",
    "cf-ipfs.com",
    "gateway.pinata.cloud",
    "w3s.link",
    "nftstorage.link",
    "4everland.io",
    "ipfs.filebase.io",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Namespace {
    Ipfs,
    Ipns,
}

impl Namespace {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("ipfs") {
            Some(Namespace::Ipfs)
        } else if name.eq_ignore_ascii_case("ipns") {
            Some(Namespace::Ipns)
        } else {
            None
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Namespace::Ipfs => "ipfs",
            Namespace::Ipns => "ipns",
        }
    }

    /// The canonical form of `id` in this namespace, if it is a CID or an
    /// IPNS name.
    pub(crate) fn canonical(self, id: &str) -> Option<String> {
        match self {
            Namespace::Ipfs => canonical_cid(id),
            Namespace::Ipns => canonical_ipns(id),
        }
    }
}

pub(crate) fn canonical_cid(id: &str) -> Option<String> {
    let cid = decode_cid(id)?;
    Some(format!("b{}", BASE32_NOPAD.encode(&cid).to_ascii_lowercase()))
}

fn canonical_ipns(name: &str) -> Option<String> {
    match ipns_key(name) {
        Some(key) => Some(format!("k{}", encode_base(&key, BASE36))),
        None => dnslink_name(name),
    }
}

/// Whether `name` is an IPNS key, as opposed to a DNSLink name.
pub(crate) fn is_ipns_key(name: &str) -> bool {
    ipns_key(name).is_some()
}

/// A subdomain gateway's label for a DNSLink name, back to the name:
/// `en-wikipedia--on--ipfs-org` is `en.wikipedia-on-ipfs.org`.
pub(crate) fn uninline_dnslink(label: &str) -> String {
    label.replace("--", "\0").replace('-', ".").replace('\0', "-")
}

/// A CID's bytes as CIDv1.
fn decode_cid(id: &str) -> Option<Vec<u8>> {
    if id.len() == 46 && id.starts_with("Qm") {
        let multihash = decode_base(id, BASE58)?;
        if multihash.len() != 34 || multihash[..2] != [0x12, 0x20] {
            return None;
        }
        return Some([&[0x01, DAG_PB][..], &multihash].concat());
    }
    decode_cidv1(id)
}

/// A CIDv1 in one of the multibases people paste: base32 (`b`, `B`), base58
/// (`z`), base16 (`f`, `F`) or base36 (`k`, `K`).
fn decode_cidv1(id: &str) -> Option<Vec<u8>> {
    if id.is_empty() || id.len() > MAX_ID || !id.is_ascii() {
        return None;
    }
    let (base, digits) = id.split_at(1);
    let bytes = match base {
        "b" | "B" => BASE32_NOPAD.decode(digits.to_ascii_uppercase().as_bytes()).ok()?,
        "z" => decode_base(digits, BASE58)?,
        "f" | "F" => HEXLOWER_PERMISSIVE.decode(digits.as_bytes()).ok()?,
        "k" | "K" => decode_base(&digits.to_ascii_lowercase(), BASE36)?,
        _ => return None,
    };
    let mut at = 0;
    let version = varint(&bytes, &mut at)?;
    varint(&bytes, &mut at)?; // the content's codec: any
    (version == 1 && multihash_len(&bytes[at..]) == Some(bytes.len() - at)).then_some(bytes)
}

/// An IPNS key's CIDv1 bytes: from a libp2p-key CID, or from a legacy peer
/// ID, which is a multihash in base58.
fn ipns_key(name: &str) -> Option<Vec<u8>> {
    if let Some(cid) = decode_cidv1(name) {
        return (cid.get(1) == Some(&LIBP2P_KEY)).then_some(cid);
    }
    if name.len() > MAX_ID {
        return None;
    }
    let multihash = decode_base(name, BASE58)?;
    if multihash_len(&multihash) != Some(multihash.len()) {
        return None;
    }
    Some([&[0x01, LIBP2P_KEY][..], &multihash].concat())
}

fn dnslink_name(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let top = name.rsplit('.').next()?;
    let valid = name.len() <= 253
        && name.contains('.')
        && name.split('.').all(label_ok)
        && !top.bytes().all(|b| b.is_ascii_digit())
        && !NOT_DNS.contains(&top);
    valid.then_some(name)
}

/// The length of the multihash `bytes` starts with, if it is there whole:
/// a hash function code, a digest length and the digest.
fn multihash_len(bytes: &[u8]) -> Option<usize> {
    let mut at = 0;
    varint(bytes, &mut at)?;
    let len = varint(bytes, &mut at)?;
    if len > MAX_DIGEST {
        return None;
    }
    let end = at + len as usize;
    (end <= bytes.len()).then_some(end)
}

/// An unsigned varint as multiformats has them: LEB128, at most 9 bytes, in
/// its shortest form.
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes.get(*at + i)?;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            if i > 0 && byte == 0 {
                return None;
            }
            *at += i + 1;
            return Some(value);
        }
    }
    None
}

/// Base-N digits, most significant first, to bytes; each leading zero digit
/// is a zero byte, as base58btc and base36 have it.
fn decode_base(digits: &str, alphabet: &[u8]) -> Option<Vec<u8>> {
    if digits.is_empty() || digits.len() > MAX_ID {
        return None;
    }
    let base = alphabet.len() as u32;
    // Little-endian while it is built.
    let mut bytes: Vec<u8> = Vec::with_capacity(digits.len());
    for c in digits.bytes() {
        let mut carry = alphabet.iter().position(|&a| a == c)? as u32;
        for byte in &mut bytes {
            carry += u32::from(*byte) * base;
            *byte = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push(carry as u8);
            carry >>= 8;
        }
    }
    let zeros = digits.bytes().take_while(|&c| c == alphabet[0]).count();
    bytes.extend(std::iter::repeat_n(0, zeros));
    bytes.reverse();
    Some(bytes)
}

fn encode_base(bytes: &[u8], alphabet: &[u8]) -> String {
    let base = alphabet.len() as u32;
    // Little-endian while it is built.
    let mut digits: Vec<u8> = Vec::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for digit in &mut digits {
            carry += u32::from(*digit) << 8;
            *digit = (carry % base) as u8;
            carry /= base;
        }
        while carry > 0 {
            digits.push((carry % base) as u8);
            carry /= base;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    std::iter::repeat_n(alphabet[0] as char, zeros)
        .chain(digits.iter().rev().map(|&d| alphabet[usize::from(d)] as char))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty UnixFS directory, as CIDv0 and as CIDv1 in each base, from
    /// `ipfs cid format` (kubo 0.43.1).
    const EMPTY_DIR_V0: &str = "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn";
    const EMPTY_DIR: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
    /// An ed25519 key's peer ID and its libp2p-key CIDs, computed with
    /// Python's integers and checked with kubo.
    const PEER_ID: &str = "12D3KooWRqzDGv6bWUyeQRA2GsiZJCGnw4eQGwcjV39aM87DL4hM";
    const KEY: &str = "k51qzi5uqu5dm49nrgwrw7omae4hd3dwbtd7s12zlq7vf2nphu4x6xv25dcl3c";
    const KEY_BASE32: &str = "bafzaajaiaejcb3rfkoed2oqgmxnux3e3xfxtovm77qleprgqu64xcj3d36maynwi";

    #[test]
    fn a_cid_in_any_base_is_one_address() {
        for form in [
            EMPTY_DIR_V0,
            EMPTY_DIR,
            "BAFYBEICZSSCDSBS7FFQZ55ASQDF3SMV6KLCW3GOFSZVWLYARCI47BGF354",
            "zdj7WbTaiJT1fgatdet9Ei9iDB5hdCxkbVyhyh8YTUnXMiwYi",
            "f0170122059948439065f29619ef41280cbb932be52c56d99c5966b65e0111239f098bbef",
            "k2jmtxtlhjl3fhmgndf92e48by79ryjuvqp3y2qgehpao6v3lurvnmcv",
        ] {
            assert_eq!(canonical_cid(form).as_deref(), Some(EMPTY_DIR), "{form}");
        }
        // Raw blocks and identity CIDs keep their codec.
        assert_eq!(canonical_cid("bafkqaaa").as_deref(), Some("bafkqaaa"));
        let raw = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
        assert_eq!(canonical_cid(raw).as_deref(), Some(raw));
    }

    #[test]
    fn refuses_what_is_not_a_cid() {
        for bad in [
            "",
            "b",
            "docs",
            "bafy",
            "localhost",
            "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3N0", // 0 is not base58
            "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3N",  // too short for a v0
            &EMPTY_DIR[..EMPTY_DIR.len() - 2],                // digest cut short
            "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354aa", // trailing bytes
            "x0170122059948439065f29619ef41280cbb932be52c56d99c5966b65e0111239f098bbef",
            "bé",
        ] {
            assert_eq!(canonical_cid(bad), None, "{bad:?}");
        }
        // CIDv1 version byte 2, the rest as the empty directory's.
        let v2 = format!("f02{}", &"f0170122059948439065f29619ef41280cbb932be52c56d99c5966b65e0111239f098bbef"[3..]);
        assert_eq!(canonical_cid(&v2), None);
    }

    #[test]
    fn caps_digests_and_ids() {
        // Identity multihashes of 128 bytes, the cap, and of 129.
        let identity = |len: u8| {
            let mut cid = vec![0x01, 0x55, 0x00, 0x80 | len, 0x01];
            cid.extend(vec![0u8; 128 + usize::from(len)]);
            format!("b{}", BASE32_NOPAD.encode(&cid).to_ascii_lowercase())
        };
        assert!(canonical_cid(&identity(0)).is_some());
        assert_eq!(canonical_cid(&identity(1)), None);
        assert_eq!(canonical_cid(&"z".repeat(MAX_ID + 1)), None);
        assert_eq!(canonical_cid(&format!("k{}", "z".repeat(MAX_ID))), None);
    }

    #[test]
    fn varints_are_minimal_and_short() {
        let read = |bytes: &[u8]| varint(bytes, &mut 0);
        assert_eq!(read(&[0x01]), Some(1));
        assert_eq!(read(&[0x81, 0x01]), Some(129));
        assert_eq!(read(&[0x80, 0x00]), None, "not the shortest form");
        assert_eq!(read(&[0xff; 10]), None, "more than 9 bytes");
        assert_eq!(read(&[0x80]), None, "cut short");
    }

    #[test]
    fn ipns_keys_are_base36_cids() {
        for form in [KEY, KEY_BASE32, PEER_ID, &KEY.to_ascii_uppercase()] {
            assert_eq!(Namespace::Ipns.canonical(form).as_deref(), Some(KEY), "{form}");
        }
        assert_eq!(
            Namespace::Ipns.canonical("Qmb5BztnNMK67e4n5Q8xMCR9bb9muh3tK8P6iTCeTJrxPu").as_deref(),
            Some("k2k4r8o4rt0kmz8mc2062vxrto0t9jr99fempdlf5443rjp8o3x9x09w"),
            "an RSA key's peer ID"
        );
        assert!(is_ipns_key(PEER_ID));
        // A content CID is not a key.
        assert_eq!(Namespace::Ipns.canonical(EMPTY_DIR), None);
    }

    #[test]
    fn dnslink_names_are_hostnames() {
        assert_eq!(Namespace::Ipns.canonical("Docs.IPFS.tech").as_deref(), Some("docs.ipfs.tech"));
        assert_eq!(Namespace::Ipns.canonical("en.wikipedia-on-ipfs.org").as_deref(), Some("en.wikipedia-on-ipfs.org"));
        for bad in ["docs", "1.2.3.4", "-x.org", "x-.org", "a..org", "x.onion", "site.i2p", "x.localhost", "é.org"] {
            assert_eq!(Namespace::Ipns.canonical(bad), None, "{bad}");
        }
        assert!(!is_ipns_key("docs.ipfs.tech"));
    }

    #[test]
    fn subdomain_labels_give_back_dnslink_names() {
        assert_eq!(uninline_dnslink("en-wikipedia--on--ipfs-org"), "en.wikipedia-on-ipfs.org");
        assert_eq!(uninline_dnslink("docs-ipfs-tech"), "docs.ipfs.tech");
    }

    #[test]
    fn base_conversions_round_trip_with_leading_zeros() {
        for bytes in [&[][..], &[0], &[0, 0, 1], &[1, 0], &[255; 40]] {
            for alphabet in [BASE58, BASE36] {
                let digits = encode_base(bytes, alphabet);
                if !bytes.is_empty() {
                    assert_eq!(decode_base(&digits, alphabet).as_deref(), Some(bytes), "{digits}");
                }
            }
        }
        assert_eq!(encode_base(&[0, 0, 1], BASE58), "112");
    }
}
