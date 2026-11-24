//! IRC addresses: a server (`irc://irc.libera.chat/`), or a channel on it
//! (`irc://irc.libera.chat/#rust`), as `irc://` and `ircs://` links and web
//! chats' pages write them (draft-butcher-irc-url, and the IANA entry).

use std::fmt;

use url::Url;

/// Plain IRC's and IRC over TLS's usual ports.
pub const PLAIN_PORT: u16 = 6667;
pub const TLS_PORT: u16 = 6697;
/// Longer than any network's CHANNELLEN (Libera's is 50).
const MAX_CHANNEL_BYTES: usize = 200;

/// An IRC server, or one of its channels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Irc {
    /// `ircs://`: TLS to the server.
    pub tls: bool,
    /// Lower case, as a web host's.
    pub host: String,
    /// `None` for the scheme's usual port.
    pub port: Option<u16>,
    /// `#name` (or `&`, `+`, `!` names), lower case.
    pub channel: Option<String>,
}

impl Irc {
    /// From an `irc://`, `ircs://` or `irc6://` URL whose host is already
    /// canonical. A channel is the path after `/`, with or without its `#`
    /// (`%23`), or the fragment (`/#rust`, as links usually write it);
    /// options after a comma (`,isnick`) and a key (`?key`) are not kept.
    /// A nick (`,isnick`) is no channel: `None` for it.
    pub fn from_url(url: &Url, host: String) -> Option<Self> {
        let tls = match url.scheme() {
            "irc" | "irc6" => false,
            "ircs" => true,
            _ => return None,
        };
        let default = if tls { TLS_PORT } else { PLAIN_PORT };
        let port = url.port().filter(|p| *p != default);
        // The channel is the path, or the fragment as links usually write
        // it (`/#rust`); a key (`?key`) is never kept.
        let path = url.path().trim_start_matches('/');
        let written = if path.is_empty() { url.fragment().map(|f| format!("#{f}")) } else { Some(path.to_owned()) };
        let written = written.map(|w| w.split('?').next().unwrap_or_default().to_owned()).filter(|w| w != "#");
        let channel = match written {
            Some(written) => {
                let (entity, options) = written.split_once(',').unwrap_or((&written, ""));
                if options.split(',').any(|o| o.eq_ignore_ascii_case("isnick") || o.eq_ignore_ascii_case("isuser")) {
                    return None;
                }
                Some(channel(&percent_decode(entity)?)?)
            }
            None => None,
        };
        Some(Irc { tls, host, port, channel })
    }

    /// The server alone, which is what is asked for a channel's list.
    pub fn server(&self) -> Irc {
        Irc { channel: None, ..self.clone() }
    }

    pub fn port(&self) -> u16 {
        self.port.unwrap_or(if self.tls { TLS_PORT } else { PLAIN_PORT })
    }
}

impl fmt::Display for Irc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", if self.tls { "ircs" } else { "irc" }, self.host)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        f.write_str("/")?;
        if let Some(channel) = &self.channel {
            // `#` starts the fragment, as links write a channel; the rest is
            // escaped where a URL would change it.
            let (prefix, name) = channel.split_at(1);
            f.write_str(prefix)?;
            for byte in name.bytes() {
                if byte.is_ascii_alphanumeric() || b"-._~!$'()*+;=:@&".contains(&byte) {
                    write!(f, "{}", byte as char)?;
                } else {
                    write!(f, "%{byte:02X}")?;
                }
            }
        }
        Ok(())
    }
}

/// A channel name in canonical form: its prefix (`#` when it has none, as
/// a bare name in a link is a channel), lower case. `None` for a name no
/// server takes: spaces, commas, control characters, or too long.
fn channel(name: &str) -> Option<String> {
    let name = if name.starts_with(['#', '&', '+', '!']) { name.to_owned() } else { format!("#{name}") };
    let valid = name.len() > 1
        && name.len() <= MAX_CHANNEL_BYTES
        && !name.chars().any(|c| c.is_whitespace() || c.is_control() || c == ',' || c == '\u{7}');
    valid.then(|| name.to_lowercase())
}

/// The channel a web chat's page opens, as an IRC address: Libera.Chat's
/// (`web.libera.chat/#rust`, `/gamja/#rust`), OFTC's
/// (`webchat.oftc.net/?channels=tor`) and Kiwi IRC's
/// (`kiwiirc.com/nextclient/#irc://irc.libera.chat:+6697/#rust`, or the
/// server in the path). `url` has its fragment still.
pub fn web_chat(url: &Url) -> Option<(Url, Option<String>)> {
    let host = url.host_str()?.trim_start_matches("www.");
    let named = |server: &str, channel: Option<String>| Some((Url::parse(&format!("irc://{server}/")).ok()?, channel));
    match host {
        "web.libera.chat" => named("irc.libera.chat", url.fragment().filter(|f| !f.is_empty()).map(str::to_owned)),
        "webchat.oftc.net" => {
            let channels = url.query_pairs().find(|(key, _)| key == "channels").map(|(_, v)| v.into_owned());
            named("irc.oftc.net", channels.and_then(|c| c.split(',').next().map(str::to_owned)))
        }
        "kiwiirc.com" => {
            let rest = url.path().strip_prefix("/nextclient/").unwrap_or_default();
            let fragment = url.fragment().unwrap_or_default();
            let (server, channel) =
                if let Some(link) = fragment.strip_prefix("irc://").or(fragment.strip_prefix("ircs://")) {
                    match link.split_once('/') {
                        Some((server, channel)) => (server.to_owned(), Some(channel.to_owned())),
                        None => (link.to_owned(), None),
                    }
                } else if !rest.is_empty() {
                    (rest.trim_end_matches('/').to_owned(), Some(fragment.to_owned()).filter(|f| !f.is_empty()))
                } else {
                    return None;
                };
            // Kiwi writes a TLS port with a `+`.
            let (server, tls) = match server.split_once(":+") {
                Some((host, port)) => (format!("{host}:{port}"), true),
                None => (server, false),
            };
            let url = Url::parse(&format!("{}://{server}/", if tls { "ircs" } else { "irc" })).ok()?;
            Some((url, channel.map(|c| c.trim_start_matches('#').to_owned()).filter(|c| !c.is_empty())))
        }
        _ => None,
    }
}

fn percent_decode(s: &str) -> Option<String> {
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
    String::from_utf8(out).ok()
}
