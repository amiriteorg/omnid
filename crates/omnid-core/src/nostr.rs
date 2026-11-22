//! Nostr addresses (NIP-19, NIP-21): an event such as a note (`note1…`,
//! `nevent1…`), a profile (`npub1…`, `nprofile1…`), or an addressable event
//! such as a long-form article (`naddr1…`), written as `nostr:` URIs.
//!
//! The canonical form drops relay hints, as a magnet's is its info hash
//! without its name: an event is `nostr:note1…`, a profile `nostr:npub1…`,
//! an addressable event `nostr:naddr1…` with its kind, author and identifier.
//! A private key (`nsec1…`) is never taken.

use std::fmt;
use std::hash::{Hash, Hasher};

/// Longer than any entity with an identifier, author, kind and the relay
/// hints kept, and the bound on what is decoded.
const MAX_ENTITY: usize = 1_024;
/// Relay hints kept from one entity, and their length.
const MAX_RELAYS: usize = 4;
const MAX_RELAY_LEN: usize = 256;
/// An addressable event's identifier (its `d` tag), in bytes.
const MAX_IDENTIFIER: usize = 256;
const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// Web clients and gateways whose pages are a Nostr entity named in the
/// path: `https://njump.me/<entity>`, `https://primal.net/e/<entity>`.
pub(crate) const KNOWN_CLIENTS: [&str; 6] =
    ["njump.me", "primal.net", "snort.social", "iris.to", "nostr.band", "coracle.social"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NostrTarget {
    /// An event by its ID: a note, a reply, a chat message, a channel.
    Event([u8; 32]),
    /// A person, by public key: their profile and recent notes.
    Profile([u8; 32]),
    /// A replaceable event by kind, author and `d` tag, such as a long-form
    /// article (kind 30023): the newest version is the one meant.
    Address { kind: u32, author: [u8; 32], identifier: String },
}

/// A Nostr entity and the relays its link suggested asking. Two with the
/// same target are the same address, whatever relays they name.
#[derive(Debug, Clone)]
pub struct Nostr {
    pub target: NostrTarget,
    /// `wss://` or `ws://` relays from the link, at most four.
    pub relays: Vec<String>,
}

impl PartialEq for Nostr {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
    }
}

impl Eq for Nostr {}

impl Hash for Nostr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.target.hash(state);
    }
}

impl Nostr {
    /// An entity, with or without its `nostr:` scheme.
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        let entity = match input.get(..6) {
            Some(scheme) if scheme.eq_ignore_ascii_case("nostr:") => &input[6..],
            _ => input,
        };
        let (hrp, bytes) = bech32_decode(entity)?;
        let key = |bytes: &[u8]| <[u8; 32]>::try_from(bytes).ok();
        let (target, relays) = match hrp.as_str() {
            "note" => (NostrTarget::Event(key(&bytes)?), vec![]),
            "npub" => (NostrTarget::Profile(key(&bytes)?), vec![]),
            "nevent" | "nprofile" | "naddr" => {
                let tlv = Tlv::read(&bytes)?;
                let target = match hrp.as_str() {
                    "nevent" => NostrTarget::Event(key(tlv.special?)?),
                    "nprofile" => NostrTarget::Profile(key(tlv.special?)?),
                    _ => {
                        let identifier = std::str::from_utf8(tlv.special.unwrap_or_default()).ok()?;
                        if identifier.len() > MAX_IDENTIFIER {
                            return None;
                        }
                        NostrTarget::Address {
                            kind: tlv.kind?,
                            author: key(tlv.author?)?,
                            identifier: identifier.to_owned(),
                        }
                    }
                };
                (target, tlv.relays)
            }
            // nsec is a private key: never an address.
            _ => return None,
        };
        Some(Nostr { target, relays })
    }

    /// The canonical entity, without its `nostr:` scheme or relay hints.
    pub fn entity(&self) -> String {
        match &self.target {
            NostrTarget::Event(id) => bech32_encode("note", id),
            NostrTarget::Profile(pubkey) => bech32_encode("npub", pubkey),
            NostrTarget::Address { kind, author, identifier } => {
                let mut tlv = vec![0, identifier.len() as u8];
                tlv.extend(identifier.as_bytes());
                tlv.extend([2, 32]);
                tlv.extend(author);
                tlv.extend([3, 4]);
                tlv.extend(kind.to_be_bytes());
                bech32_encode("naddr", &tlv)
            }
        }
    }

    /// Whose it is, where the address says: a profile's or an addressable
    /// event's author. An event's author is known once it is fetched.
    pub fn author(&self) -> Option<[u8; 32]> {
        match &self.target {
            NostrTarget::Profile(pubkey) | NostrTarget::Address { author: pubkey, .. } => Some(*pubkey),
            NostrTarget::Event(_) => None,
        }
    }
}

impl fmt::Display for Nostr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "nostr:{}", self.entity())
    }
}

/// `npub1…` for a public key, as Nostr shows people.
pub fn npub(pubkey: &[u8; 32]) -> String {
    bech32_encode("npub", pubkey)
}

/// A NIP-19 TLV payload's fields; unknown types are skipped, as NIP-19 says.
#[derive(Default)]
struct Tlv<'a> {
    special: Option<&'a [u8]>,
    relays: Vec<String>,
    author: Option<&'a [u8]>,
    kind: Option<u32>,
}

impl<'a> Tlv<'a> {
    fn read(mut bytes: &'a [u8]) -> Option<Self> {
        let mut tlv = Tlv::default();
        while !bytes.is_empty() {
            let (&kind, rest) = bytes.split_first()?;
            let (&len, rest) = rest.split_first()?;
            let value = rest.get(..usize::from(len))?;
            bytes = &rest[usize::from(len)..];
            match kind {
                0 if tlv.special.is_none() => tlv.special = Some(value),
                1 if tlv.relays.len() < MAX_RELAYS && value.len() <= MAX_RELAY_LEN => {
                    let relay = std::str::from_utf8(value).ok().map(str::trim).unwrap_or_default();
                    let lower = relay.to_ascii_lowercase();
                    if lower.starts_with("wss://") || lower.starts_with("ws://") {
                        tlv.relays.push(relay.to_owned());
                    }
                }
                2 if tlv.author.is_none() => tlv.author = Some(value),
                3 if tlv.kind.is_none() => tlv.kind = Some(u32::from_be_bytes(value.try_into().ok()?)),
                _ => {}
            }
        }
        Some(tlv)
    }
}

/// BIP-173 bech32, without its 90-character limit (Nostr's entities pass
/// it), up to [`MAX_ENTITY`].
fn bech32_decode(s: &str) -> Option<(String, Vec<u8>)> {
    if s.len() < 8 || s.len() > MAX_ENTITY || !s.is_ascii() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if s != lower && s != s.to_ascii_uppercase() {
        return None;
    }
    let at = lower.rfind('1')?;
    let (hrp, data) = (&lower[..at], &lower[at + 1..]);
    if hrp.is_empty() || data.len() < 6 {
        return None;
    }
    let values: Vec<u8> =
        data.bytes().map(|c| CHARSET.iter().position(|&x| x == c).map(|p| p as u8)).collect::<Option<_>>()?;
    if polymod(&[hrp_expand(hrp), values.clone()].concat()) != 1 {
        return None;
    }
    let bytes = convert_bits(&values[..values.len() - 6], 5, 8, false)?;
    Some((hrp.to_owned(), bytes))
}

fn bech32_encode(hrp: &str, bytes: &[u8]) -> String {
    let values = convert_bits(bytes, 8, 5, true).expect("8-bit input converts");
    let checksum = polymod(&[hrp_expand(hrp), values.clone(), vec![0; 6]].concat()) ^ 1;
    let checksum = (0..6).map(|i| ((checksum >> (5 * (5 - i))) & 31) as u8);
    let data: String = values.into_iter().chain(checksum).map(|v| CHARSET[usize::from(v)] as char).collect();
    format!("{hrp}1{data}")
}

fn hrp_expand(hrp: &str) -> Vec<u8> {
    hrp.bytes().map(|b| b >> 5).chain([0]).chain(hrp.bytes().map(|b| b & 31)).collect()
}

fn polymod(values: &[u8]) -> u32 {
    const GENERATOR: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
    let mut checksum = 1u32;
    for &value in values {
        let top = checksum >> 25;
        checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(value);
        for (i, generator) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                checksum ^= generator;
            }
        }
    }
    checksum
}

/// Regroups bits, as BIP-173's reference does: from 5 to 8 without padding
/// (leftover bits must be zero), from 8 to 5 with it.
fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let max = (1u32 << to) - 1;
    let max_acc = (1u32 << (from + to - 1)) - 1;
    let mut out = Vec::with_capacity(data.len() * from as usize / to as usize + 1);
    for &value in data {
        if u32::from(value) >> from != 0 {
            return None;
        }
        acc = ((acc << from) | u32::from(value)) & max_acc;
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & max) as u8);
        }
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & max) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & max) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NIP-19's own examples.
    const NPUB: &str = "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg";
    const PUBKEY: &str = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";
    const NPROFILE: &str = "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";
    const PROFILE_KEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn decodes_nip19s_examples() {
        let npub = Nostr::parse(NPUB).unwrap();
        assert_eq!(npub.target, NostrTarget::Profile(<[u8; 32]>::try_from(&unhex(PUBKEY)[..]).unwrap()));
        assert_eq!(npub.entity(), NPUB);
        let profile = Nostr::parse(&format!("nostr:{NPROFILE}")).unwrap();
        let NostrTarget::Profile(key) = profile.target else { panic!() };
        assert_eq!(hex(&key), PROFILE_KEY);
        assert_eq!(profile.relays, ["wss://r.x.com", "wss://djbas.sadkb.com"]);
        assert_eq!(profile.entity(), npub_of(PROFILE_KEY), "the canonical form drops the hints");
        assert_eq!(Nostr::parse(&profile.entity()).unwrap(), profile, "the same address either way");
    }

    #[test]
    fn events_and_addresses_round_trip() {
        let id = [7u8; 32];
        let note = Nostr { target: NostrTarget::Event(id), relays: vec![] };
        assert!(note.entity().starts_with("note1"));
        assert_eq!(Nostr::parse(&note.to_string()).unwrap().target, note.target);
        let article = Nostr {
            target: NostrTarget::Address { kind: 30023, author: [9; 32], identifier: "my-article".into() },
            relays: vec![],
        };
        let entity = article.entity();
        assert!(entity.starts_with("naddr1"));
        assert_eq!(Nostr::parse(&entity).unwrap().target, article.target);
        assert_eq!(article.author(), Some([9; 32]));
        assert_eq!(note.author(), None);
    }

    #[test]
    fn reads_an_nevent_with_hints_in_any_order() {
        // type 1 (relay) and 2 (author) before 0, an unknown type 9, kind 1.
        let mut tlv = vec![1, 15];
        tlv.extend(b"wss://relay.one");
        tlv.extend([9, 2, 0xaa, 0xbb, 2, 32]);
        tlv.extend([5; 32]);
        tlv.extend([0, 32]);
        tlv.extend([6; 32]);
        tlv.extend([3, 4, 0, 0, 0, 1]);
        let event = Nostr::parse(&bech32_encode("nevent", &tlv)).unwrap();
        assert_eq!(event.target, NostrTarget::Event([6; 32]));
        assert_eq!(event.relays, ["wss://relay.one"]);
        assert_eq!(event.to_string(), format!("nostr:{}", bech32_encode("note", &[6; 32])));
    }

    #[test]
    fn refuses_keys_bad_checksums_and_bad_payloads() {
        let nsec = bech32_encode("nsec", &[1; 32]);
        assert!(Nostr::parse(&nsec).is_none(), "a private key is never an address");
        let mut bad = NPUB.to_owned();
        bad.replace_range(10..11, if &NPUB[10..11] == "q" { "p" } else { "q" });
        assert!(Nostr::parse(&bad).is_none(), "checksum");
        assert!(Nostr::parse(&NPUB.to_ascii_uppercase()).is_some(), "one case throughout is fine");
        assert!(Nostr::parse(&format!("{}{}", NPUB[..10].to_ascii_uppercase(), &NPUB[10..])).is_none(), "mixed case");
        assert!(Nostr::parse(&bech32_encode("npub", &[1; 31])).is_none(), "a key is 32 bytes");
        assert!(Nostr::parse(&bech32_encode("nevent", &[0, 40, 1])).is_none(), "TLV longer than its payload");
        assert!(Nostr::parse(&bech32_encode("naddr", &[0, 1, b'x'])).is_none(), "no author or kind");
        assert!(Nostr::parse(&"q".repeat(MAX_ENTITY + 1)).is_none());
        assert!(Nostr::parse("npub1").is_none() && Nostr::parse("").is_none());
    }

    #[test]
    fn caps_relay_hints() {
        let mut tlv = vec![0, 32];
        tlv.extend([1; 32]);
        for _ in 0..6 {
            tlv.extend([1, 13]);
            tlv.extend(b"wss://a.relay");
        }
        tlv.extend([1, 18]);
        tlv.extend(b"http://not.a.relay");
        let profile = Nostr::parse(&bech32_encode("nprofile", &tlv)).unwrap();
        assert_eq!(profile.relays.len(), MAX_RELAYS);
        assert!(profile.relays.iter().all(|r| r == "wss://a.relay"));
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn npub_of(hex_key: &str) -> String {
        bech32_encode("npub", &unhex(hex_key))
    }
}
