use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use data_encoding::{BASE32_NOPAD, HEXLOWER, HEXLOWER_PERMISSIVE};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// Origin used to resolve relative links inside a freesite. FProxy serves keys
/// at the root of its origin, so a key behaves like an absolute path there.
const HYPHANET_BASE: &str = "http://fproxy.invalid/";
const HYPHANET_HOST: &str = "fproxy.invalid";
const HYPHANET_KEY_TYPES: [&str; 4] = ["USK@", "SSK@", "CHK@", "KSK@"];

/// The network that carries a [`Source`]. Also the search filter facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Clearnet,
    Onion,
    I2p,
    /// Hyphanet, formerly Freenet: the Java node and its `USK@`/`SSK@`/`CHK@`
    /// keys. Not the newer, unrelated network that now uses the Freenet name.
    Hyphanet,
    Magnet,
}

impl Network {
    pub const ALL: [Network; 5] = [Network::Clearnet, Network::Onion, Network::I2p, Network::Hyphanet, Network::Magnet];

    pub fn as_str(self) -> &'static str {
        match self {
            Network::Clearnet => "clearnet",
            Network::Onion => "onion",
            Network::I2p => "i2p",
            Network::Hyphanet => "hyphanet",
            Network::Magnet => "magnet",
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Network {
    type Err = SourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "clearnet" | "web" => Ok(Network::Clearnet),
            "onion" | "tor" => Ok(Network::Onion),
            "i2p" => Ok(Network::I2p),
            "hyphanet" | "freenet" => Ok(Network::Hyphanet),
            "magnet" | "dht" | "torrent" => Ok(Network::Magnet),
            _ => Err(SourceError::UnknownNetwork(s.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("not a valid address: {0}")]
    Invalid(String),
    #[error("unsupported scheme `{0}`")]
    UnsupportedScheme(String),
    #[error("v2 onion addresses were retired in 2021; only v3 (56 characters) is reachable")]
    OnionV2,
    #[error("malformed onion address `{0}`")]
    BadOnion(String),
    #[error("magnet link has no BitTorrent v1 info hash (xt=urn:btih:...)")]
    NoInfoHash,
    #[error("unknown network `{0}`")]
    UnknownNetwork(String),
}

/// A canonical address on one of the supported networks.
///
/// Its `Display` form is the canonical URL that the index keys documents by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Clearnet(Url),
    Onion(Url),
    I2p(Url),
    /// A Hyphanet key with its path, e.g. `USK@…/site/4/index.html`.
    Hyphanet(String),
    Magnet(Magnet),
}

impl Source {
    /// Parses anything a user might paste: a URL, a bare host
    /// (`xyz.onion`), a `freenet:`/`hyphanet:` key or a magnet link.
    pub fn parse(input: &str) -> Result<Self, SourceError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(SourceError::Invalid(String::new()));
        }
        if strip_prefix_ignore_case(input, "magnet:").is_some() {
            return Magnet::parse(input).map(Source::Magnet);
        }
        if let Some(key) = hyphanet_key(input) {
            return Ok(Source::Hyphanet(key));
        }
        let with_scheme;
        let input = if input.contains("://") {
            input
        } else {
            with_scheme = format!("http://{input}");
            &with_scheme
        };
        let url = Url::parse(input).map_err(|e| SourceError::Invalid(format!("{input}: {e}")))?;
        Self::from_url(url)
    }

    /// Classifies an http(s) URL by its host.
    pub fn from_url(mut url: Url) -> Result<Self, SourceError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(SourceError::UnsupportedScheme(url.scheme().to_owned()));
        }
        url.set_fragment(None);
        let network = match url.host() {
            Some(Host::Domain(domain)) if domain.ends_with(".onion") => {
                validate_onion(domain)?;
                Network::Onion
            }
            Some(Host::Domain(domain)) if domain.ends_with(".i2p") => Network::I2p,
            Some(_) => Network::Clearnet,
            None => return Err(SourceError::Invalid(url.to_string())),
        };
        Ok(match network {
            Network::Onion => Source::Onion(url),
            Network::I2p => Source::I2p(url),
            _ => Source::Clearnet(url),
        })
    }

    pub fn network(&self) -> Network {
        match self {
            Source::Clearnet(_) => Network::Clearnet,
            Source::Onion(_) => Network::Onion,
            Source::I2p(_) => Network::I2p,
            Source::Hyphanet(_) => Network::Hyphanet,
            Source::Magnet(_) => Network::Magnet,
        }
    }

    /// The http(s) URL, for the networks addressed by one.
    pub fn url(&self) -> Option<&Url> {
        match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => Some(url),
            Source::Hyphanet(_) | Source::Magnet(_) => None,
        }
    }

    /// Groups requests for politeness: a host, a freesite, or one torrent.
    pub fn site_key(&self) -> String {
        match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => {
                url.host_str().unwrap_or_default().to_owned()
            }
            Source::Hyphanet(key) => key.split('/').next().unwrap_or(key).to_owned(),
            Source::Magnet(magnet) => format!("btih:{}", magnet.info_hash_hex()),
        }
    }

    /// Resolves a link found on this source's page. Links may cross networks:
    /// a clearnet directory page linking an onion yields a [`Source::Onion`].
    pub fn join(&self, href: &str) -> Option<Source> {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') {
            return None;
        }
        if strip_prefix_ignore_case(href, "magnet:").is_some() {
            return Magnet::parse(href).ok().map(Source::Magnet);
        }
        if strip_prefix_ignore_case(href, "freenet:").is_some() || strip_prefix_ignore_case(href, "hyphanet:").is_some()
        {
            return hyphanet_key(href).map(Source::Hyphanet);
        }
        let base = match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => url.clone(),
            Source::Hyphanet(key) => Url::parse(HYPHANET_BASE).ok()?.join(key).ok()?,
            Source::Magnet(_) => return None,
        };
        let joined = base.join(href).ok()?;
        if matches!(self, Source::Hyphanet(_)) && joined.host_str() == Some(HYPHANET_HOST) {
            let path = &joined[url::Position::BeforePath..url::Position::AfterQuery];
            return hyphanet_key(path).map(Source::Hyphanet);
        }
        Source::from_url(joined).ok()
    }

    /// The site's `robots.txt`, for the networks that have one.
    pub fn robots_txt(&self) -> Option<Source> {
        let robots = self.url()?.join("/robots.txt").ok()?;
        Source::from_url(robots).ok()
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => f.write_str(url.as_str()),
            Source::Hyphanet(key) => write!(f, "freenet:{key}"),
            Source::Magnet(magnet) => write!(f, "magnet:?xt=urn:btih:{}", magnet.info_hash_hex()),
        }
    }
}

impl FromStr for Source {
    type Err = SourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Source::parse(s)
    }
}

/// A BitTorrent v1 magnet link.
#[derive(Debug, Clone)]
pub struct Magnet {
    pub info_hash: [u8; 20],
    /// The `dn` display name. Not part of the identity: two magnets with the
    /// same info hash are the same torrent.
    pub name: Option<String>,
}

impl Magnet {
    pub fn parse(input: &str) -> Result<Self, SourceError> {
        let url = Url::parse(input.trim()).map_err(|e| SourceError::Invalid(format!("{input}: {e}")))?;
        if url.scheme() != "magnet" {
            return Err(SourceError::UnsupportedScheme(url.scheme().to_owned()));
        }
        let mut info_hash = None;
        let mut name = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "xt" => {
                    if let Some(hash) = strip_prefix_ignore_case(&value, "urn:btih:") {
                        info_hash = Some(parse_btih(hash)?);
                    }
                }
                "dn" => name = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        Ok(Magnet { info_hash: info_hash.ok_or(SourceError::NoInfoHash)?, name: name.filter(|n| !n.is_empty()) })
    }

    pub fn info_hash_hex(&self) -> String {
        HEXLOWER.encode(&self.info_hash)
    }
}

impl PartialEq for Magnet {
    fn eq(&self, other: &Self) -> bool {
        self.info_hash == other.info_hash
    }
}

impl Eq for Magnet {}

impl Hash for Magnet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.info_hash.hash(state);
    }
}

/// A btih is 40 hex characters, or 32 base32 characters in older clients.
fn parse_btih(hash: &str) -> Result<[u8; 20], SourceError> {
    let bad = || SourceError::Invalid(format!("info hash `{hash}`"));
    let bytes = match hash.len() {
        40 => HEXLOWER_PERMISSIVE.decode(hash.as_bytes()),
        32 => BASE32_NOPAD.decode(hash.to_ascii_uppercase().as_bytes()),
        _ => return Err(bad()),
    }
    .map_err(|_| bad())?;
    bytes.try_into().map_err(|_| bad())
}

/// A v3 address is base32(pubkey[32] || checksum[2] || version[1]) = 56
/// characters. Only the length and version byte are checked here; the
/// checksum is verified by arti when it connects.
fn validate_onion(domain: &str) -> Result<(), SourceError> {
    let bad = || SourceError::BadOnion(domain.to_owned());
    let label = domain.strip_suffix(".onion").and_then(|d| d.rsplit('.').next()).ok_or_else(bad)?;
    match label.len() {
        16 => Err(SourceError::OnionV2),
        56 => {
            let decoded = BASE32_NOPAD.decode(label.to_ascii_uppercase().as_bytes()).map_err(|_| bad())?;
            if decoded.len() == 35 && decoded[34] == 3 { Ok(()) } else { Err(bad()) }
        }
        _ => Err(bad()),
    }
}

/// Extracts a Hyphanet key from `freenet:KEY`, `hyphanet:KEY`, `/KEY` or a
/// bare key, normalising the key type to upper case.
fn hyphanet_key(input: &str) -> Option<String> {
    let rest = strip_prefix_ignore_case(input, "freenet:")
        .or_else(|| strip_prefix_ignore_case(input, "hyphanet:"))
        .unwrap_or(input)
        .trim_start_matches('/');
    let rest = rest.split('#').next().unwrap_or(rest);
    let key_type = HYPHANET_KEY_TYPES
        .iter()
        .find(|t| rest.len() > t.len() && rest.get(..t.len()).is_some_and(|head| head.eq_ignore_ascii_case(t)))?;
    Some(format!("{key_type}{}", &rest[key_type.len()..]))
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn bare_hosts_default_to_http() {
        let s = Source::parse("example.com").unwrap();
        assert_eq!(s.network(), Network::Clearnet);
        assert_eq!(s.to_string(), "http://example.com/");
    }

    #[test]
    fn classifies_by_host() {
        assert_eq!(Source::parse(ONION).unwrap().network(), Network::Onion);
        assert_eq!(Source::parse(&format!("https://www.{ONION}/x")).unwrap().network(), Network::Onion);
        assert_eq!(Source::parse("http://stats.i2p/").unwrap().network(), Network::I2p);
        assert_eq!(Source::parse("https://1.1.1.1/").unwrap().network(), Network::Clearnet);
    }

    #[test]
    fn rejects_bad_onions() {
        assert_eq!(Source::parse("abcdefghijklmnop.onion"), Err(SourceError::OnionV2));
        assert!(matches!(Source::parse("short.onion"), Err(SourceError::BadOnion(_))));
        // Right length, wrong version byte (last character must encode 3).
        let wrong_version = format!("{}a.onion", &ONION[..55]);
        assert!(matches!(Source::parse(&wrong_version), Err(SourceError::BadOnion(_))));
    }

    #[test]
    fn rejects_other_schemes() {
        assert_eq!(Source::parse("ftp://example.com/"), Err(SourceError::UnsupportedScheme("ftp".into())));
    }

    #[test]
    fn canonical_form_drops_fragment() {
        let s = Source::parse("https://example.com/a?b=1#frag").unwrap();
        assert_eq!(s.to_string(), "https://example.com/a?b=1");
    }

    #[test]
    fn parses_hyphanet_keys() {
        let key = "USK@abc,def,AQACAAE/site/3/";
        for input in [format!("freenet:{key}"), format!("hyphanet:{key}"), format!("/{key}"), key.to_string()] {
            assert_eq!(Source::parse(&input).unwrap(), Source::Hyphanet(key.into()), "{input}");
        }
        assert_eq!(Source::parse(key).unwrap().to_string(), format!("freenet:{key}"));
        assert_eq!(Source::parse(key).unwrap().site_key(), "USK@abc,def,AQACAAE");
    }

    #[test]
    fn joins_links_inside_freesites() {
        let base = Source::parse("freenet:USK@abc,def,AQACAAE/site/3/").unwrap();
        assert_eq!(base.join("page.html"), Some(Source::Hyphanet("USK@abc,def,AQACAAE/site/3/page.html".into())));
        assert_eq!(base.join("../"), Some(Source::Hyphanet("USK@abc,def,AQACAAE/site/".into())));
        assert_eq!(base.join("/SSK@xyz/other/"), Some(Source::Hyphanet("SSK@xyz/other/".into())));
        assert_eq!(base.join("/not-a-key"), None);
    }

    #[test]
    fn joins_across_networks() {
        let base = Source::parse("https://directory.example/list/").unwrap();
        assert_eq!(base.join(&format!("http://{ONION}/")).unwrap().network(), Network::Onion);
        assert_eq!(base.join("next.html").unwrap().to_string(), "https://directory.example/list/next.html");
        assert!(base.join("mailto:someone@example.com").is_none());
        assert!(base.join("javascript:void(0)").is_none());
        assert!(base.join("#top").is_none());
        let magnet = base.join("magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x").unwrap();
        assert_eq!(magnet.network(), Network::Magnet);
    }

    #[test]
    fn parses_magnets_in_hex_and_base32() {
        let hex = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        let bytes = HEXLOWER.decode(hex.as_bytes()).unwrap();
        let b32 = BASE32_NOPAD.encode(&bytes).to_ascii_lowercase();

        let a = Source::parse(&format!("magnet:?xt=urn:btih:{}&dn=Some+Name", hex.to_uppercase())).unwrap();
        let b = Source::parse(&format!("magnet:?xt=urn:btih:{b32}")).unwrap();
        assert_eq!(a, b, "same info hash is the same torrent regardless of dn");
        assert_eq!(a.to_string(), format!("magnet:?xt=urn:btih:{hex}"));
        let Source::Magnet(m) = a else { unreachable!() };
        assert_eq!(m.name.as_deref(), Some("Some Name"));
    }

    #[test]
    fn rejects_magnets_without_btih() {
        assert_eq!(Source::parse("magnet:?dn=nothing"), Err(SourceError::NoInfoHash));
        assert!(Source::parse("magnet:?xt=urn:btih:nothex").is_err());
    }

    #[test]
    fn robots_txt_lives_at_the_site_root() {
        let s = Source::parse(&format!("http://{ONION}/deep/page?q=1")).unwrap();
        assert_eq!(s.robots_txt().unwrap().to_string(), format!("http://{ONION}/robots.txt"));
        assert!(Source::parse("freenet:USK@a,b,c/s/1/").unwrap().robots_txt().is_none());
    }

    #[test]
    fn network_names_parse_with_aliases() {
        assert_eq!("tor".parse::<Network>().unwrap(), Network::Onion);
        assert_eq!("Freenet".parse::<Network>().unwrap(), Network::Hyphanet);
        assert!("gopher".parse::<Network>().is_err());
    }
}
