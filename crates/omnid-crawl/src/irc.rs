//! IRC networks and their channels, from each network's channel list. One
//! list serves every channel on the network: it is asked for once and kept
//! for a while, so crawling a network's channels is one connection to it,
//! not one per channel. A list that could not be had is remembered too, so
//! its channels wait instead of each asking again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omnid_core::irc::Irc;
use omnid_core::{IndexedDoc, Network, Source, now_secs};
use omnid_net::{FetchError, Gateway, IrcListing};

/// How long a network's list is used before it is asked for again.
const LIST_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// How long a network that gave no list is left alone.
const FAILURE_TTL: Duration = Duration::from_secs(30 * 60);
/// Addresses in a topic, looked for as words.
const LINK_SCHEMES: [&str; 6] = ["https://", "http://", "irc://", "ircs://", "gemini://", "gopher://"];

type Answer = Result<Arc<IrcListing>, String>;

#[derive(Default)]
pub(crate) struct IrcLists {
    servers: Mutex<HashMap<String, (Instant, Answer)>>,
}

impl IrcLists {
    /// The list of the network `server` is on: kept, or asked for now.
    pub(crate) async fn get(&self, gateway: &Gateway, server: &Irc) -> Result<Arc<IrcListing>, ListError> {
        let key = server.server().to_string();
        if let Some((expires, answer)) = self.servers.lock().expect("IRC lists lock poisoned").get(&key)
            && *expires > Instant::now()
        {
            return answer.clone().map_err(ListError::Remembered);
        }
        let answer = gateway.irc_list(&Source::Irc(server.server())).await.map(Arc::new);
        let (ttl, kept) = match &answer {
            Ok(listing) => (LIST_TTL, Ok(listing.clone())),
            // A missing router says nothing of the network: not remembered.
            Err(FetchError::Unavailable { .. }) => return answer.map_err(ListError::Fetch),
            Err(e) => (FAILURE_TTL, Err(e.to_string())),
        };
        self.servers.lock().expect("IRC lists lock poisoned").insert(key, (Instant::now() + ttl, kept));
        answer.map_err(ListError::Fetch)
    }
}

pub(crate) enum ListError {
    Fetch(FetchError),
    /// The network gave no list a short while ago.
    Remembered(String),
}

/// What is indexed for a network or one of its channels, and the addresses
/// it leads to: from a network, its channels, the largest first; from a
/// channel, what its topic links.
pub(crate) struct Found {
    pub title: String,
    pub description: String,
    pub body: String,
    pub keywords: Vec<String>,
    pub links: Vec<String>,
}

/// The network's name, as it gives it, or its server's.
fn network_name(listing: &IrcListing, server: &Irc) -> String {
    listing.network.clone().unwrap_or_else(|| server.host.clone())
}

/// A network: its name and its largest channels. `None` for a channel the
/// list does not have (it is gone, secret, or has too few people).
pub(crate) fn found(listing: &IrcListing, address: &Irc) -> Option<Found> {
    let network = network_name(listing, address);
    let Some(wanted) = &address.channel else {
        let largest: Vec<&str> = listing.channels.iter().take(5).map(|c| c.name.as_str()).collect();
        let links = listing
            .channels
            .iter()
            .filter_map(|c| Source::parse(&format!("{}{}", address.server(), c.name)).ok())
            .map(|s| s.to_string())
            .collect();
        return Some(Found {
            title: network.clone(),
            description: format!(
                "An IRC network: {} channels with {} or more people, the largest {}.",
                listing.listed,
                omnid_net::IRC_MIN_USERS,
                largest.join(", ")
            ),
            body: format!("{network} IRC network {}", address.host),
            keywords: vec![network, "IRC".into()],
            links,
        });
    };
    let channel = listing.channels.iter().find(|c| c.name.to_lowercase() == *wanted)?;
    let topic = if channel.topic.is_empty() { "No topic.".to_owned() } else { channel.topic.clone() };
    Some(Found {
        title: channel.name.clone(),
        description: format!("{} people on {network}. {topic}", channel.users),
        body: format!("{} {network} {topic}", channel.name),
        keywords: vec![network, "IRC".into()],
        links: topic_links(&channel.topic),
    })
}

/// The addresses a topic names, as words: web, IRC, Gemini and Gopher.
fn topic_links(topic: &str) -> Vec<String> {
    topic
        .split_whitespace()
        .filter(|word| LINK_SCHEMES.iter().any(|scheme| word.to_ascii_lowercase().starts_with(scheme)))
        .map(|word| word.trim_end_matches(['.', ',', ';', ')', '!', '?', '"', '\'']).to_owned())
        .take(20)
        .collect()
}

/// The document for a network or a channel.
pub(crate) fn doc(address: &Source, found: Found, trust: f32) -> IndexedDoc {
    let url = address.to_string();
    let now = now_secs();
    IndexedDoc {
        network: Network::Irc,
        site: address.site_key(),
        title: found.title,
        description: found.description,
        keywords: found.keywords,
        // Each channel is its own: two with one topic are not mirrors.
        content_hash: format!("irc:{url}"),
        body: found.body,
        info_hash: None,
        files: vec![],
        first_seen: now,
        last_seen: now,
        liveness: 1.0,
        trust,
        lang: None,
        url,
    }
}

#[cfg(test)]
mod tests {
    use omnid_net::IrcChannel;

    use super::*;

    fn listing() -> IrcListing {
        let channel = |name: &str, users, topic: &str| IrcChannel { name: name.into(), users, topic: topic.into() };
        IrcListing {
            network: Some("Libera.Chat".into()),
            channels: vec![
                channel(
                    "#libera",
                    1500,
                    "Libera.Chat help: https://libera.chat/guides/. Also ircs://irc.libera.chat:6697/#libera-offtopic",
                ),
                channel("#Rust", 900, ""),
            ],
            listed: 2,
        }
    }

    fn irc(address: &str) -> Irc {
        match Source::parse(address).unwrap() {
            Source::Irc(irc) => irc,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_network_leads_to_its_channels() {
        let found = found(&listing(), &irc("ircs://irc.libera.chat/")).unwrap();
        assert_eq!(found.title, "Libera.Chat");
        assert_eq!(found.links, ["ircs://irc.libera.chat/#libera", "ircs://irc.libera.chat/#rust"]);
        assert!(found.description.contains("2 channels with 10 or more people, the largest #libera, #Rust"));
    }

    #[test]
    fn a_channel_has_its_topic_and_its_links() {
        let found = found(&listing(), &irc("ircs://irc.libera.chat/#libera")).unwrap();
        assert_eq!(found.title, "#libera");
        assert!(found.description.starts_with("1500 people on Libera.Chat. Libera.Chat help"));
        assert_eq!(found.links, ["https://libera.chat/guides/", "ircs://irc.libera.chat:6697/#libera-offtopic"]);
        let quiet = super::found(&listing(), &irc("ircs://irc.libera.chat/#rust")).unwrap();
        assert!(quiet.description.ends_with("No topic."), "names compare without case");
        assert!(super::found(&listing(), &irc("ircs://irc.libera.chat/#gone")).is_none());
    }

    #[test]
    fn each_channel_is_its_own_document() {
        let a = doc(&Source::parse("irc://h/#a").unwrap(), found(&listing(), &irc("irc://h/#libera")).unwrap(), 0.5);
        let b = doc(&Source::parse("irc://h/#b").unwrap(), found(&listing(), &irc("irc://h/#libera")).unwrap(), 0.5);
        assert_ne!(a.content_hash, b.content_hash);
        assert_eq!((a.network, a.site.as_str()), (Network::Irc, "h"));
    }
}
