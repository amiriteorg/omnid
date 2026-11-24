use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use data_encoding::{BASE32_NOPAD, HEXLOWER, HEXLOWER_PERMISSIVE};
use serde::{Deserialize, Serialize};
use url::{Host, Position, Url};

use crate::ipfs::{self, Namespace};
use crate::nostr::{self, Nostr};

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
    /// IPFS content (`ipfs://<CID>/…`) and IPNS names (`ipns://<name>/…`),
    /// through an IPFS gateway.
    Ipfs,
    /// Nostr events, profiles and articles (`nostr:note1…`), from relays.
    Nostr,
    /// Gemini capsules (`gemini://`), on the clearnet, onions or I2P.
    Gemini,
    /// Gopher holes (`gopher://`), on the clearnet, onions or I2P.
    Gopher,
    Magnet,
}

impl Network {
    pub const ALL: [Network; 9] = [
        Network::Clearnet,
        Network::Onion,
        Network::I2p,
        Network::Hyphanet,
        Network::Ipfs,
        Network::Nostr,
        Network::Gemini,
        Network::Gopher,
        Network::Magnet,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Network::Clearnet => "clearnet",
            Network::Onion => "onion",
            Network::I2p => "i2p",
            Network::Hyphanet => "hyphanet",
            Network::Ipfs => "ipfs",
            Network::Nostr => "nostr",
            Network::Gemini => "gemini",
            Network::Gopher => "gopher",
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
            "ipfs" | "ipns" => Ok(Network::Ipfs),
            "nostr" => Ok(Network::Nostr),
            "gemini" => Ok(Network::Gemini),
            "gopher" => Ok(Network::Gopher),
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
    #[error("not an IPFS CID or IPNS name: `{0}`")]
    BadIpfs(String),
    #[error("not a Nostr note, profile or address: `{0}`")]
    BadNostr(String),
    #[error("not a Gemini or Gopher address: `{0}`")]
    BadSmallWeb(String),
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
    /// `ipfs://<CID>/<path>` or `ipns://<name>/<path>`, the CID or name in its
    /// canonical form: a CID as CIDv1 in base32, an IPNS key as CIDv1 in
    /// base36, a DNSLink name as its lowercase hostname.
    Ipfs(Url),
    Nostr(Nostr),
    /// `gemini://host[:port]/path[?query]`: the host lower case, the
    /// default port (1965) left out, an empty path as `/`.
    Gemini(Url),
    /// `gopher://host[:port]/<type><selector>`: the item type is the path's
    /// first character, `/` alone the server's root menu. The default port
    /// (70) is left out; a search (`%09…`) is not part of the address.
    Gopher(Url),
    Magnet(Magnet),
}

impl Source {
    /// Parses anything a user might paste: a URL, a bare host
    /// (`xyz.onion`), a `freenet:`/`hyphanet:` key, an IPFS address
    /// (`ipfs://`, `ipns://`, `/ipfs/…`, `dweb:/ipfs/…` or a bare CID), a
    /// Nostr entity (`nostr:npub1…`, or bare) or a magnet link.
    pub fn parse(input: &str) -> Result<Self, SourceError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(SourceError::Invalid(String::new()));
        }
        if let Some(entity) = nostr_entity(input) {
            return entity;
        }
        if strip_prefix_ignore_case(input, "magnet:").is_some() {
            return Magnet::parse(input).map(Source::Magnet);
        }
        if let Some(key) = hyphanet_key(input) {
            return Ok(Source::Hyphanet(key));
        }
        if let Some(source) = ipfs_path(input).or_else(|| bare_cid(input)) {
            return source;
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

    /// Classifies an http(s) URL by its host, and takes an `ipfs://` or
    /// `ipns://` one. An http(s) URL through a public IPFS gateway is the
    /// content it serves.
    pub fn from_url(mut url: Url) -> Result<Self, SourceError> {
        match url.scheme() {
            "http" | "https" => {}
            "nostr" => return Nostr::parse(url.as_str()).map(Source::Nostr).ok_or(SourceError::BadNostr(url.into())),
            "gemini" => return gemini_source(&url),
            "gopher" => return gopher_source(&url),
            scheme => match Namespace::from_name(scheme) {
                Some(namespace) => {
                    let id = url.host_str().ok_or_else(|| SourceError::BadIpfs(url.to_string()))?;
                    return ipfs_source(namespace, id, &url[Position::BeforePath..Position::AfterQuery]);
                }
                None => return Err(SourceError::UnsupportedScheme(scheme.to_owned())),
            },
        }
        url.set_fragment(None);
        drop_session(&mut url);
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
            _ => through_gateway(&url)
                .or_else(|| through_nostr_client(&url))
                .or_else(|| telegram_preview(&url))
                .unwrap_or(Source::Clearnet(url)),
        })
    }

    pub fn network(&self) -> Network {
        match self {
            Source::Clearnet(_) => Network::Clearnet,
            Source::Onion(_) => Network::Onion,
            Source::I2p(_) => Network::I2p,
            Source::Hyphanet(_) => Network::Hyphanet,
            Source::Ipfs(_) => Network::Ipfs,
            Source::Nostr(_) => Network::Nostr,
            Source::Gemini(_) => Network::Gemini,
            Source::Gopher(_) => Network::Gopher,
            Source::Magnet(_) => Network::Magnet,
        }
    }

    /// The network that carries the connection: where a Gemini capsule or a
    /// Gopher hole is, by its host (an onion through Tor, `.i2p` through
    /// I2P, anything else the clearnet); for every other address, its
    /// [`Source::network`]. What decides how it is reached, and whether a
    /// step to it leaves an anonymity network.
    pub fn transport(&self) -> Network {
        match self {
            Source::Gemini(url) | Source::Gopher(url) => match url.host_str() {
                Some(host) if host.ends_with(".onion") => Network::Onion,
                Some(host) if host.ends_with(".i2p") => Network::I2p,
                _ => Network::Clearnet,
            },
            source => source.network(),
        }
    }

    /// The http(s) URL, for the networks addressed by one.
    pub fn url(&self) -> Option<&Url> {
        match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => Some(url),
            Source::Hyphanet(_)
            | Source::Ipfs(_)
            | Source::Nostr(_)
            | Source::Gemini(_)
            | Source::Gopher(_)
            | Source::Magnet(_) => None,
        }
    }

    /// The `gemini://` or `gopher://` URL, for the small-web protocols.
    pub fn small_web_url(&self) -> Option<&Url> {
        match self {
            Source::Gemini(url) | Source::Gopher(url) => Some(url),
            _ => None,
        }
    }

    /// A Gopher address's item type and selector: `('1', "")` for the root
    /// menu. The selector is the bytes to send, percent-decoded.
    pub fn gopher_item(&self) -> Option<(char, Vec<u8>)> {
        let Source::Gopher(url) = self else { return None };
        Some(gopher_item(url))
    }

    /// The public Telegram channel this is the web preview of
    /// (`https://t.me/s/<name>`), its name lower case.
    pub fn telegram_channel(&self) -> Option<&str> {
        let url = match self {
            Source::Clearnet(url) if url.host_str() == Some("t.me") => url,
            _ => return None,
        };
        let mut segments = url.path_segments()?;
        (segments.next() == Some("s")).then(|| segments.next()).flatten().filter(|name| !name.is_empty())
    }

    /// Where a gateway serves an IPFS address: `ipfs/<CID>/<path>` or
    /// `ipns/<name>/<path>`, relative to the gateway's root.
    pub fn gateway_path(&self) -> Option<String> {
        let Source::Ipfs(url) = self else { return None };
        Some(format!("{}/{}{}", url.scheme(), url.host_str()?, &url[Position::BeforePath..Position::AfterQuery]))
    }

    /// Groups requests for politeness: a host (a Gemini capsule's or Gopher
    /// hole's too, so a denied host is denied on every protocol), a freesite,
    /// one IPFS root or IPNS name, one Nostr author (an event's own ID until
    /// its author is known), or one torrent.
    pub fn site_key(&self) -> String {
        match self {
            Source::Clearnet(url)
            | Source::Onion(url)
            | Source::I2p(url)
            | Source::Gemini(url)
            | Source::Gopher(url) => url.host_str().unwrap_or_default().to_owned(),
            Source::Hyphanet(key) => key.split('/').next().unwrap_or(key).to_owned(),
            Source::Ipfs(url) => format!("{}:{}", url.scheme(), url.host_str().unwrap_or_default()),
            Source::Nostr(entity) => match entity.author() {
                Some(author) => format!("nostr:{}", nostr::npub(&author)),
                None => entity.to_string(),
            },
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
        if strip_prefix_ignore_case(href, "nostr:").is_some() {
            return Nostr::parse(href).map(Source::Nostr);
        }
        let base = match self {
            Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) => url.clone(),
            Source::Hyphanet(key) => Url::parse(HYPHANET_BASE).ok()?.join(key).ok()?,
            Source::Ipfs(url) => {
                // Pages on IPFS link other content as gateway paths, and a
                // link without a scheme leaves for the web, as it does in a
                // browser on a gateway.
                if let Some(source) = ipfs_path(href) {
                    return source.ok();
                }
                if href.starts_with("//") {
                    return Source::from_url(Url::parse(&format!("https:{href}")).ok()?).ok();
                }
                url.clone()
            }
            // Nostr content links only by full address.
            Source::Nostr(_) => return Source::from_url(Url::parse(href).ok()?).ok(),
            // Gopher menus name each item in full; a Gemini page's links may
            // be relative.
            Source::Gemini(url) | Source::Gopher(url) => url.clone(),
            Source::Magnet(_) => return None,
        };
        let joined = base.join(href).ok()?;
        if matches!(self, Source::Hyphanet(_)) && joined.host_str() == Some(HYPHANET_HOST) {
            let path = &joined[url::Position::BeforePath..url::Position::AfterQuery];
            return hyphanet_key(path).map(Source::Hyphanet);
        }
        Source::from_url(joined).ok()
    }

    /// The site's `robots.txt`, for the networks that have one: a Gemini
    /// capsule's at `/robots.txt`, a Gopher hole's as the text item
    /// `robots.txt`, as Veronica-2 asks for it.
    pub fn robots_txt(&self) -> Option<Source> {
        let robots = match self {
            Source::Gemini(url) => url.join("/robots.txt").ok()?,
            Source::Gopher(url) => url.join("/0robots.txt").ok()?,
            _ => self.url()?.join("/robots.txt").ok()?,
        };
        Source::from_url(robots).ok()
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Clearnet(url)
            | Source::Onion(url)
            | Source::I2p(url)
            | Source::Ipfs(url)
            | Source::Gemini(url)
            | Source::Gopher(url) => f.write_str(url.as_str()),
            Source::Hyphanet(key) => write!(f, "freenet:{key}"),
            Source::Nostr(entity) => write!(f, "{entity}"),
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

/// Extracts a Hyphanet key from `freenet:KEY`, `hyphanet:KEY`, `/KEY`,
/// `/freenet:KEY` (FProxy's redirects) or a bare key, normalising the key
/// type to upper case.
fn hyphanet_key(input: &str) -> Option<String> {
    let input = input.trim_start_matches('/');
    let rest = strip_prefix_ignore_case(input, "freenet:")
        .or_else(|| strip_prefix_ignore_case(input, "hyphanet:"))
        .unwrap_or(input)
        .trim_start_matches('/');
    // Trimmed as the whole input was, so the canonical form parses back.
    let rest = rest.split('#').next().unwrap_or(rest).trim_end();
    let key_type = HYPHANET_KEY_TYPES
        .iter()
        .find(|t| rest.len() > t.len() && rest.get(..t.len()).is_some_and(|head| head.eq_ignore_ascii_case(t)))?;
    Some(format!("{key_type}{}", &rest[key_type.len()..]))
}

/// A `nostr:` URI, or a bare entity (`npub1…`, `note1…`, `nevent1…`,
/// `nprofile1…`, `naddr1…`). `None` when it is neither; an error when it
/// looks like one and does not decode.
fn nostr_entity(input: &str) -> Option<Result<Source, SourceError>> {
    let scheme = strip_prefix_ignore_case(input, "nostr:").is_some();
    let bare = ["npub1", "note1", "nevent1", "nprofile1", "naddr1"]
        .iter()
        .any(|hrp| strip_prefix_ignore_case(input, hrp).is_some() && !input.contains(['/', '.', ':']));
    (scheme || bare).then(|| Nostr::parse(input).map(Source::Nostr).ok_or_else(|| SourceError::BadNostr(input.into())))
}

/// The Nostr entity a known web client's page shows: njump.me/<entity>,
/// primal.net/e/<entity> and the like.
fn through_nostr_client(url: &Url) -> Option<Source> {
    let host = url.host_str()?.trim_start_matches("www.");
    if !nostr::KNOWN_CLIENTS.contains(&host) {
        return None;
    }
    url.path_segments()?.take(2).find_map(Nostr::parse).map(Source::Nostr)
}

/// The hosts of links to Telegram.
const TELEGRAM_HOSTS: [&str; 3] = ["t.me", "telegram.me", "telegram.dog"];
/// First path segments that are Telegram's own pages, not a channel's name.
const TELEGRAM_PAGES: [&str; 22] = [
    "s",
    "c",
    "joinchat",
    "addstickers",
    "addemoji",
    "addtheme",
    "addlist",
    "setlanguage",
    "share",
    "proxy",
    "socks",
    "iv",
    "login",
    "bg",
    "boost",
    "contact",
    "invoice",
    "giftcode",
    "confirmphone",
    "nft",
    "m",
    "k",
];

/// A link to a public Telegram channel or one of its posts, as the web
/// preview of the channel's latest posts (`https://t.me/s/<name>`); the
/// preview's `?before=<post>`, which pages back through older posts, is
/// kept. `t.me/<name>` and a post's own page show no posts to a reader
/// without the app. Bots (names ending in `bot`) have no preview.
fn telegram_preview(url: &Url) -> Option<Source> {
    let host = url.host_str()?.trim_start_matches("www.");
    if !TELEGRAM_HOSTS.contains(&host) {
        return None;
    }
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());
    let first = segments.next()?;
    let (name, preview) = if first == "s" { (segments.next()?, true) } else { (first, false) };
    let rest: Vec<&str> = segments.collect();
    let a_post = rest.len() == 1 && rest[0].bytes().all(|b| b.is_ascii_digit());
    if !(rest.is_empty() || a_post) {
        return None;
    }
    let name = name.to_ascii_lowercase();
    let valid = (5..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.ends_with("bot")
        && !TELEGRAM_PAGES.contains(&name.as_str());
    if !valid {
        return None;
    }
    let before = (preview && rest.is_empty())
        .then(|| url.query_pairs().find(|(key, _)| key == "before").map(|(_, value)| value.into_owned()))
        .flatten()
        .filter(|post| !post.is_empty() && post.len() <= 12 && post.bytes().all(|b| b.is_ascii_digit()));
    let preview = match before {
        Some(post) => format!("https://t.me/s/{name}?before={post}"),
        None => format!("https://t.me/s/{name}"),
    };
    Url::parse(&preview).ok().map(Source::Clearnet)
}

/// Drops what carries a visitor's session rather than a page: phpBB's `sid`,
/// vBulletin's `s`, PHP's and Java's session IDs. Forum software adds them to
/// every link for visitors without cookies, as a crawler is, so one thread
/// would have an address per visit. Only values shaped like a session token
/// go (`?s=search+words` stays); the rest of the query is kept as written.
fn drop_session(url: &mut Url) {
    let path = url.path();
    if let Some(at) = path.to_ascii_lowercase().find(";jsessionid=") {
        let kept = path[..at].to_owned();
        url.set_path(&kept);
    }
    let Some(query) = url.query() else { return };
    let pairs: Vec<&str> = query.split('&').collect();
    let kept: Vec<&str> = pairs.iter().copied().filter(|pair| !is_session(pair)).collect();
    if kept.len() < pairs.len() {
        let kept = kept.join("&");
        url.set_query(Some(&kept).filter(|q| !q.is_empty()).map(String::as_str));
    }
}

fn is_session(pair: &str) -> bool {
    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
    let hex32 = value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit());
    let token =
        (16..=128).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_,".contains(&b));
    match key.to_ascii_lowercase().as_str() {
        "sid" | "s" => hex32,
        "phpsessid" | "jsessionid" | "sessionid" | "oscsid" | "zenid" => token,
        _ => false,
    }
}

/// An IPFS address in its canonical form, from a CID or IPNS name and what
/// follows it: a path, a query, a fragment (dropped), or nothing.
fn ipfs_source(namespace: Namespace, id: &str, rest: &str) -> Result<Source, SourceError> {
    let bad = || SourceError::BadIpfs(id.to_owned());
    let id = namespace.canonical(id).ok_or_else(bad)?;
    let slash = if rest.starts_with('/') { "" } else { "/" };
    let mut url = Url::parse(&format!("{}://{id}{slash}{rest}", namespace.as_str())).map_err(|_| bad())?;
    url.set_fragment(None);
    Ok(Source::Ipfs(url))
}

/// A gateway path, `/ipfs/<CID>/…` or `/ipns/<name>/…`, or the same after
/// `dweb:`. `None` when it is not one; an error when its CID or name is bad.
fn ipfs_path(input: &str) -> Option<Result<Source, SourceError>> {
    let path = strip_prefix_ignore_case(input, "dweb:").unwrap_or(input).strip_prefix('/')?;
    let (namespace, rest) = path.split_once('/')?;
    let namespace = Namespace::from_name(namespace)?;
    let (id, rest) = split_id(rest);
    Some(ipfs_source(namespace, id, rest))
}

/// A CID on its own, or with a path after it.
fn bare_cid(input: &str) -> Option<Result<Source, SourceError>> {
    let (id, rest) = split_id(input);
    ipfs::canonical_cid(id).map(|_| ipfs_source(Namespace::Ipfs, id, rest))
}

fn split_id(s: &str) -> (&str, &str) {
    s.split_at(s.find(['/', '?', '#']).unwrap_or(s.len()))
}

/// The IPFS content an http(s) URL through a gateway serves: a subdomain
/// gateway's `<CID>.ipfs.<host>` or `<key>.ipns.<host>`, on any host, as the
/// label is a CID; a DNSLink name's label (`docs-ipfs-tech.ipns.<host>`) or
/// a path gateway's `/ipfs/<CID>`, only on a known public gateway.
fn through_gateway(url: &Url) -> Option<Source> {
    let host = url.host_str()?;
    let rest = &url[Position::BeforePath..Position::AfterQuery];
    let mut labels = host.splitn(3, '.');
    if let (Some(id), Some(namespace), Some(gateway)) = (labels.next(), labels.next(), labels.next()) {
        let known = ipfs::KNOWN_GATEWAYS.contains(&gateway) || gateway == "localhost";
        let id = match Namespace::from_name(namespace) {
            Some(Namespace::Ipfs) => Some((Namespace::Ipfs, id.to_owned())),
            Some(Namespace::Ipns) if ipfs::is_ipns_key(id) => Some((Namespace::Ipns, id.to_owned())),
            Some(Namespace::Ipns) if known => Some((Namespace::Ipns, ipfs::uninline_dnslink(id))),
            _ => None,
        };
        if let Some(Ok(source)) = id.map(|(namespace, id)| ipfs_source(namespace, &id, rest)) {
            return Some(source);
        }
    }
    if ipfs::KNOWN_GATEWAYS.contains(&host) {
        return ipfs_path(rest).and_then(Result::ok);
    }
    None
}

/// Gemini's and Gopher's default ports.
const GEMINI_PORT: u16 = 1965;
const GOPHER_PORT: u16 = 70;

/// A small-web URL's host in canonical form: an IDN as its ASCII form, lower
/// case, without a trailing dot; an onion checked as on the web. These are
/// not "special" schemes to the URL parser, which keeps their hosts as written.
fn small_web_host(url: &Url) -> Result<String, SourceError> {
    let bad = || SourceError::BadSmallWeb(url.to_string());
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad());
    }
    let raw = url.host_str().filter(|h| !h.is_empty()).ok_or_else(bad)?;
    let decoded = percent_decode(raw);
    let decoded = std::str::from_utf8(&decoded).map_err(|_| bad())?.trim_end_matches('.');
    let host = canonical_host(decoded).ok_or_else(bad)?;
    // The canonical form is a host as it stands: one that parsing would
    // change again (`4444444444..` became `4444444444`, an IPv4 address out
    // of range; found by fuzz/fuzz_targets/source_parse.rs and source_join.rs)
    // is no address.
    if canonical_host(&host).as_deref() != Some(host.as_str()) {
        return Err(bad());
    }
    if host.ends_with(".onion") {
        validate_onion(&host)?;
    }
    Ok(host)
}

/// A host as the URL parser reads a web host: an IDN as its ASCII form,
/// lower case, an IPv6 address in brackets.
fn canonical_host(host: &str) -> Option<String> {
    let host = match Host::parse(host).ok()? {
        Host::Domain(domain) => domain,
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{ip}]"),
    };
    (!host.is_empty()).then_some(host)
}

/// `host` or `host:port`, the port left out when it is the default.
fn authority(host: &str, port: Option<u16>, default: u16) -> String {
    match port.filter(|p| *p != default) {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

fn gemini_source(url: &Url) -> Result<Source, SourceError> {
    let host = small_web_host(url)?;
    let rest = &url[Position::BeforePath..Position::AfterQuery];
    let slash = if rest.starts_with('/') { "" } else { "/" };
    let canonical = format!("gemini://{}{slash}{rest}", authority(&host, url.port(), GEMINI_PORT));
    Url::parse(&canonical).map(Source::Gemini).map_err(|_| SourceError::BadSmallWeb(url.to_string()))
}

/// A Gopher address; a link to the web (an `h` item whose selector is
/// `URL:<address>`) is that address.
fn gopher_source(url: &Url) -> Result<Source, SourceError> {
    let bad = || SourceError::BadSmallWeb(url.to_string());
    let host = small_web_host(url)?;
    let (item_type, selector) = gopher_item(url);
    if item_type == 'h'
        && let Some(web) = selector.strip_prefix(b"URL:")
    {
        let web = std::str::from_utf8(web).map_err(|_| bad())?;
        return Source::parse(web).and_then(|source| match source {
            Source::Gopher(_) => Err(bad()),
            source => Ok(source),
        });
    }
    // Everything from a tab on asks a search server or a Gopher+ one for
    // more: not part of the item.
    let rest = &url[Position::BeforePath..Position::AfterQuery];
    let rest = rest.split("%09").next().unwrap_or(rest);
    let path = match rest {
        "" | "/" | "/1" => "/",
        path => path,
    };
    let slash = if path.starts_with('/') { "" } else { "/" };
    let canonical = format!("gopher://{}{slash}{path}", authority(&host, url.port(), GOPHER_PORT));
    Url::parse(&canonical).map(Source::Gopher).map_err(|_| bad())
}

/// A Gopher URL's item type and percent-decoded selector (RFC 4266): the
/// path's first character after `/`, then the rest, which may hold a `?`.
fn gopher_item(url: &Url) -> (char, Vec<u8>) {
    let rest = &url[Position::BeforePath..Position::AfterQuery];
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let rest = rest.split("%09").next().unwrap_or(rest);
    let mut chars = rest.chars();
    match chars.next() {
        None => ('1', Vec::new()),
        Some(item_type) => (item_type, percent_decode(chars.as_str())),
    }
}

/// Percent-decoding, leaving a `%` that starts no escape as it is.
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) =
                (bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
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
    fn telegram_links_are_their_channels_preview() {
        let canonical = |s: &str| Source::parse(s).unwrap().to_string();
        for link in [
            "https://t.me/TorProject",
            "https://t.me/torproject/63",
            "https://telegram.me/torproject",
            "http://www.t.me/s/torproject/41",
            "t.me/s/torproject?q=bridges",
            "https://t.me/s/torproject#latest",
        ] {
            assert_eq!(canonical(link), "https://t.me/s/torproject", "{link}");
        }
        assert_eq!(canonical("https://t.me/s/torproject?before=41"), "https://t.me/s/torproject?before=41");
        let channel = Source::parse("https://t.me/durov").unwrap();
        assert_eq!(channel.telegram_channel(), Some("durov"));
        assert_eq!(channel.site_key(), "t.me", "one site, for politeness");
        // Not channels: invites, Telegram's own pages, bots, a channel's deeper pages, other sites.
        for kept in [
            "https://t.me/+AbCdEf123",
            "https://t.me/joinchat/AbCdEf",
            "https://t.me/addstickers/pack",
            "https://t.me/GetBridgesBot",
            "https://t.me/abc",
            "https://t.me/torproject/63/extra",
            "https://telegram.org/blog",
        ] {
            let source = Source::parse(kept).unwrap();
            assert_eq!(source.telegram_channel(), None, "{kept}");
            assert!(!source.to_string().starts_with("https://t.me/s/"), "{kept}");
        }
        assert_eq!(canonical("https://t.me/s/torproject?before=12x"), "https://t.me/s/torproject", "a bad page number");
    }

    #[test]
    fn drops_session_ids_and_keeps_the_rest_of_the_query() {
        let canonical = |s: &str| Source::parse(s).unwrap().to_string();
        let sid = "0123456789abcdef0123456789ABCDEF";
        // phpBB, vBulletin, SMF, a Java forum; on an onion too.
        assert_eq!(
            canonical(&format!("http://f.example/viewtopic.php?f=2&t=9&sid={sid}")),
            "http://f.example/viewtopic.php?f=2&t=9"
        );
        assert_eq!(
            canonical(&format!("http://f.example/showthread.php?s={sid}&t=5")),
            "http://f.example/showthread.php?t=5"
        );
        assert_eq!(
            canonical("http://f.example/index.php?PHPSESSID=3k1m2n4b5v6c7x8z9a0s1d2f3g&topic=5.0"),
            "http://f.example/index.php?topic=5.0"
        );
        assert_eq!(
            canonical("http://f.example/thread.jsp;jsessionid=A1B2C3D4E5F6A7B8C9D0?id=4"),
            "http://f.example/thread.jsp?id=4"
        );
        assert_eq!(canonical(&format!("http://{ONION}/index.php?sid={sid}")), format!("http://{ONION}/index.php"));
        // Not sessions: a search, a short id, something else's hex, an encoded query kept as written.
        for kept in [
            "https://blog.example/?s=privacy+tools",
            "http://f.example/viewtopic.php?t=9&sid=12",
            &format!("https://example.com/file?hash={sid}"),
            "https://example.com/q?a=%2F%20b&b=2",
        ] {
            assert_eq!(canonical(kept), kept);
        }
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
    fn whitespace_before_a_fragment_is_not_part_of_a_key() {
        // Found by fuzz/fuzz_targets/source_parse.rs: the key kept the
        // whitespace before `#`, which parsing its canonical form trims,
        // leaving another key, or no address at all ("KSK@\t#").
        let key = "USK@abc,def,AQACAAE/site/3/";
        assert_eq!(Source::parse(&format!("{key} #top")).unwrap(), Source::Hyphanet(key.into()));
        assert!(Source::parse("KSK@\t#").is_err(), "a key type with no key is no key");
        for input in ["KSK@\t#", "USK@a #b", "freenet:SSK@x\t\t#y"] {
            if let Ok(source) = Source::parse(input) {
                assert_eq!(Source::parse(&source.to_string()), Ok(source), "{input:?}");
            }
        }
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
    fn follows_fproxy_edition_redirects() {
        // FProxy answers a stale USK edition with `Location: /freenet:USK@.../<newer>/...`.
        let base = Source::parse("freenet:USK@abc,def,AQACAAE/site/20/page.html").unwrap();
        assert_eq!(
            base.join("/freenet:USK@abc,def,AQACAAE/site/21/page.html"),
            Some(Source::Hyphanet("USK@abc,def,AQACAAE/site/21/page.html".into()))
        );
        assert_eq!(Source::parse("/hyphanet:SSK@xyz/other/").unwrap(), Source::Hyphanet("SSK@xyz/other/".into()));
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
        assert_eq!("ipns".parse::<Network>().unwrap(), Network::Ipfs);
        assert_eq!("Gopher".parse::<Network>().unwrap(), Network::Gopher);
        assert!("finger".parse::<Network>().is_err());
    }

    #[test]
    fn parses_gemini_addresses_canonically() {
        let canonical = |s: &str| Source::parse(s).unwrap().to_string();
        for input in [
            "gemini://geminiprotocol.net",
            "gemini://GeminiProtocol.NET/",
            "GEMINI://geminiprotocol.net:1965/",
            "gemini://geminiprotocol.net./#top",
        ] {
            assert_eq!(canonical(input), "gemini://geminiprotocol.net/", "{input}");
        }
        assert_eq!(canonical("gemini://a.example:1966/x/../docs/?q=1"), "gemini://a.example:1966/docs/?q=1");
        assert_eq!(canonical("gemini://bücher.example/"), "gemini://xn--bcher-kva.example/");
        let capsule = Source::parse("gemini://a.example/log/").unwrap();
        assert_eq!(capsule.network(), Network::Gemini);
        assert_eq!(capsule.transport(), Network::Clearnet);
        assert_eq!(capsule.site_key(), "a.example", "the host, as on the web");
        assert!(capsule.url().is_none(), "not an http(s) URL");
        assert_eq!(capsule.robots_txt().unwrap().to_string(), "gemini://a.example/robots.txt");
        let onion = Source::parse(&format!("gemini://{ONION}/")).unwrap();
        assert_eq!((onion.network(), onion.transport()), (Network::Gemini, Network::Onion));
        assert_eq!(Source::parse("gemini://x.i2p/").unwrap().transport(), Network::I2p);
        assert_eq!(canonical("gemini://a.example../x"), "gemini://a.example/x");
        for bad in [
            "gemini://user@a.example/",
            "gemini:///path",
            "gemini://short.onion/",
            "gemini:a.example",
            "gemini://44444444444../",
            "gopher://.../",
        ] {
            assert!(Source::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn joins_gemini_links() {
        let page = Source::parse("gemini://a.example/log/2024/post.gmi").unwrap();
        let at = |href: &str| page.join(href).map(|s| s.to_string());
        assert_eq!(at("next.gmi").unwrap(), "gemini://a.example/log/2024/next.gmi");
        assert_eq!(at("/").unwrap(), "gemini://a.example/");
        assert_eq!(at("//b.example/x").unwrap(), "gemini://b.example/x", "scheme-relative stays on Gemini");
        assert_eq!(at("gopher://c.example/1/").unwrap(), "gopher://c.example/1/");
        assert_eq!(at("https://example.com/").unwrap(), "https://example.com/");
        assert_eq!(at("#top"), None);
    }

    #[test]
    fn parses_gopher_addresses_canonically() {
        let canonical = |s: &str| Source::parse(s).unwrap().to_string();
        for root in ["gopher://Floodgap.com", "gopher://floodgap.com:70/", "gopher://floodgap.com/1"] {
            assert_eq!(canonical(root), "gopher://floodgap.com/", "{root}");
        }
        assert_eq!(canonical("gopher://floodgap.com/1/world"), "gopher://floodgap.com/1/world");
        assert_eq!(canonical("gopher://a.example:7070/0/notes.txt"), "gopher://a.example:7070/0/notes.txt");
        assert_eq!(
            canonical("gopher://a.example/7/v2/vs%09omnid"),
            "gopher://a.example/7/v2/vs",
            "a search's words are not the address"
        );
        let menu = Source::parse("gopher://a.example/1/phlog/").unwrap();
        assert_eq!(menu.network(), Network::Gopher);
        assert_eq!(menu.gopher_item(), Some(('1', b"/phlog/".to_vec())));
        assert_eq!(Source::parse("gopher://a.example/").unwrap().gopher_item(), Some(('1', vec![])));
        assert_eq!(
            Source::parse("gopher://a.example/0/a%20b?c").unwrap().gopher_item(),
            Some(('0', b"/a b?c".to_vec())),
            "a selector may hold a question mark"
        );
        assert_eq!(menu.robots_txt().unwrap().to_string(), "gopher://a.example/0robots.txt");
        assert_eq!(menu.site_key(), "a.example");
        assert_eq!(Source::parse(&format!("gopher://{ONION}/1/")).unwrap().transport(), Network::Onion);
    }

    #[test]
    fn a_gopher_link_to_the_web_is_the_web_address() {
        let canonical = |s: &str| Source::parse(s).unwrap().to_string();
        assert_eq!(canonical("gopher://a.example/hURL:https://example.com/page"), "https://example.com/page");
        assert_eq!(
            Source::parse(&format!("gopher://a.example/hURL:http://{ONION}/")).unwrap().network(),
            Network::Onion
        );
        assert!(Source::parse("gopher://a.example/hURL:gopher://b.example/").is_err(), "no loops");
        assert_eq!(canonical("gopher://a.example/h/page.html"), "gopher://a.example/h/page.html");
    }

    const NPUB: &str = "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg";
    const NPROFILE: &str = "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";

    #[test]
    fn parses_nostr_entities_and_their_web_pages() {
        for input in [
            format!("nostr:{NPUB}"),
            NPUB.to_owned(),
            NPUB.to_ascii_uppercase(),
            format!("https://njump.me/{NPUB}"),
            format!("https://primal.net/p/{NPUB}"),
        ] {
            let source = Source::parse(&input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(source.network(), Network::Nostr, "{input}");
            assert_eq!(source.to_string(), format!("nostr:{NPUB}"), "{input}");
            assert_eq!(source.site_key(), format!("nostr:{NPUB}"));
            assert!(source.url().is_none() && source.robots_txt().is_none());
        }
        let profile = Source::parse(&format!("nostr:{NPROFILE}")).unwrap();
        assert!(profile.to_string().starts_with("nostr:npub1"), "relay hints are not the address");
        assert!(matches!(Source::parse("nostr:npub1nothing"), Err(SourceError::BadNostr(_))));
        assert!(matches!(Source::parse("npub1qqqqqqqqqq"), Err(SourceError::BadNostr(_))));
        assert_eq!(Source::parse("https://njump.me/about").unwrap().network(), Network::Clearnet);
        assert_eq!(Source::parse("npub1.example.com").unwrap().network(), Network::Clearnet, "a host, not an entity");
    }

    #[test]
    fn joins_nostr_links() {
        let web = Source::parse("https://example.com/").unwrap();
        assert_eq!(web.join(&format!("nostr:{NPUB}")).unwrap().network(), Network::Nostr);
        let note = Source::parse(&format!("nostr:{NPUB}")).unwrap();
        assert_eq!(note.join("https://example.com/x").unwrap().to_string(), "https://example.com/x");
        assert_eq!(note.join(&format!("nostr:{NPROFILE}")).unwrap().network(), Network::Nostr);
        assert_eq!(note.join("relative/page"), None, "nothing is relative to a note");
        assert_eq!(note.join("#tag"), None);
    }

    const CID_V0: &str = "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn";
    const CID: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
    const KEY: &str = "k51qzi5uqu5dm49nrgwrw7omae4hd3dwbtd7s12zlq7vf2nphu4x6xv25dcl3c";

    #[test]
    fn parses_ipfs_addresses_in_every_form() {
        let want = format!("ipfs://{CID}/wiki/Page.html");
        for input in [
            format!("ipfs://{CID}/wiki/Page.html"),
            format!("ipfs://{CID_V0}/wiki/Page.html"),
            format!("/ipfs/{CID_V0}/wiki/Page.html"),
            format!("dweb:/ipfs/{CID}/wiki/Page.html#top"),
            format!("{CID_V0}/wiki/Page.html"),
            format!("https://ipfs.io/ipfs/{CID_V0}/wiki/Page.html"),
            format!("https://{CID}.ipfs.dweb.link/wiki/Page.html"),
            format!("http://{CID}.ipfs.localhost:8080/wiki/Page.html"),
        ] {
            let source = Source::parse(&input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(source.network(), Network::Ipfs, "{input}");
            assert_eq!(source.to_string(), want, "{input}");
        }
        let root = Source::parse(&format!("ipfs://{CID}")).unwrap();
        assert_eq!(root.to_string(), format!("ipfs://{CID}/"));
        assert_eq!(root.site_key(), format!("ipfs:{CID}"));
        assert_eq!(root.gateway_path().unwrap(), format!("ipfs/{CID}/"));
        assert!(root.url().is_none() && root.robots_txt().is_none());
        let query = Source::parse(&format!("/ipfs/{CID}/a b?filename=x.txt")).unwrap();
        assert_eq!(query.gateway_path().unwrap(), format!("ipfs/{CID}/a%20b?filename=x.txt"));
    }

    #[test]
    fn parses_ipns_keys_and_dnslink_names() {
        let docs = Source::parse("ipns://Docs.IPFS.tech/concepts/").unwrap();
        assert_eq!(docs.to_string(), "ipns://docs.ipfs.tech/concepts/");
        assert_eq!(docs.site_key(), "ipns:docs.ipfs.tech");
        for input in [
            "https://en-wikipedia--on--ipfs-org.ipns.dweb.link/wiki/",
            "https://dweb.link/ipns/en.wikipedia-on-ipfs.org/wiki/",
            "/ipns/en.wikipedia-on-ipfs.org/wiki/",
        ] {
            assert_eq!(Source::parse(input).unwrap().to_string(), "ipns://en.wikipedia-on-ipfs.org/wiki/", "{input}");
        }
        let key = format!("ipns://{KEY}/");
        assert_eq!(
            Source::parse("/ipns/12D3KooWRqzDGv6bWUyeQRA2GsiZJCGnw4eQGwcjV39aM87DL4hM").unwrap().to_string(),
            key
        );
        assert_eq!(Source::parse(&format!("https://{KEY}.ipns.gateway.example/")).unwrap().to_string(), key);
    }

    #[test]
    fn leaves_what_only_looks_like_a_gateway_on_the_web() {
        for input in [
            // A DNSLink label folds only on a known gateway: elsewhere it is a site.
            "https://my-site.ipns.example.com/",
            "https://docs.ipfs.tech/install/",
            "https://example.com/ipfs/bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354/",
            "https://ipfs.io/ipfs/not-a-cid/",
            "https://ipfs.io/",
        ] {
            assert_eq!(Source::parse(input).unwrap().network(), Network::Clearnet, "{input}");
        }
        for bad in ["ipfs://not-a-cid/", "/ipfs/Qm123/", "ipns://x.onion/", "ipns://localhost/", "ipfs:///x"] {
            assert!(Source::parse(bad).is_err(), "{bad}");
        }
        assert!(Source::parse("bafy").unwrap().network() == Network::Clearnet, "a bare name that is no CID");
    }

    #[test]
    fn joins_links_inside_ipfs_content() {
        let page = Source::parse(&format!("ipfs://{CID}/docs/intro.html")).unwrap();
        let at = |href: &str| page.join(href).map(|s| s.to_string());
        assert_eq!(at("next.html").unwrap(), format!("ipfs://{CID}/docs/next.html"));
        assert_eq!(at("../../../up.html").unwrap(), format!("ipfs://{CID}/up.html"));
        assert_eq!(at("/about/").unwrap(), format!("ipfs://{CID}/about/"), "the root is the CID's");
        assert_eq!(at(&format!("/ipfs/{CID_V0}/x")).unwrap(), format!("ipfs://{CID}/x"), "a gateway path");
        assert_eq!(at("/ipns/docs.ipfs.tech/").unwrap(), "ipns://docs.ipfs.tech/");
        assert_eq!(at(&format!("ipfs://{CID}/y")).unwrap(), format!("ipfs://{CID}/y"));
        assert_eq!(at("https://example.com/").unwrap(), "https://example.com/");
        assert_eq!(at("//example.com/x").unwrap(), "https://example.com/x", "scheme-relative goes to the web");
        assert_eq!(at(&format!("https://ipfs.io/ipfs/{CID}/z")).unwrap(), format!("ipfs://{CID}/z"));
        assert_eq!(at("#section"), None);
        assert_eq!(at("/ipfs/not-a-cid/"), None);
        // From the web into IPFS.
        let web = Source::parse("https://example.com/links.html").unwrap();
        assert_eq!(web.join(&format!("ipfs://{CID_V0}/")).unwrap().to_string(), format!("ipfs://{CID}/"));
        assert_eq!(web.join("ipns://docs.ipfs.tech").unwrap().network(), Network::Ipfs);
    }
}
