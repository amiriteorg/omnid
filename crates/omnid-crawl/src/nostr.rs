//! Nostr events as documents: a note, a long-form article, a public chat
//! channel with its latest messages (NIP-28), a profile; and the links in
//! them, to other events, people, articles and the web. The gateway checks
//! every event's ID and signature before it gets here.

use omnid_core::nostr::{Nostr, NostrTarget, npub};
use omnid_net::{NostrEvent, NostrFilter};

use crate::extract::{collapse, truncate};

pub(crate) const PROFILE: u32 = 0;
pub(crate) const NOTE: u32 = 1;
pub(crate) const CHANNEL: u32 = 40;
pub(crate) const CHANNEL_MESSAGE: u32 = 42;
pub(crate) const ARTICLE: u32 = 30023;
/// A profile's latest notes, articles and channels, followed as links.
pub(crate) const PROFILE_EVENTS: usize = 20;
/// A channel's latest messages, indexed with it.
pub(crate) const CHANNEL_MESSAGES: usize = 50;
const MAX_TITLE: usize = 120;
const MAX_DESCRIPTION: usize = 280;
const MAX_KEYWORDS: usize = 32;
/// Links taken from one event's tags and text.
const MAX_LINKS: usize = 200;

/// What the index gets from an event.
pub(crate) struct Fields {
    pub title: String,
    pub description: String,
    pub body: String,
    pub keywords: Vec<String>,
    /// As written: `nostr:` entities and web addresses.
    pub links: Vec<String>,
}

/// The filter that finds what `target` names.
pub fn filter(target: &NostrTarget) -> NostrFilter {
    match target {
        NostrTarget::Event(id) => NostrFilter { ids: vec![*id], ..NostrFilter::default() },
        NostrTarget::Profile(pubkey) => {
            NostrFilter { authors: vec![*pubkey], kinds: vec![PROFILE], limit: Some(1), ..NostrFilter::default() }
        }
        NostrTarget::Address { kind, author, identifier } => NostrFilter {
            authors: vec![*author],
            kinds: vec![*kind],
            d: vec![identifier.clone()],
            limit: Some(1),
            ..NostrFilter::default()
        },
    }
}

/// A profile's latest notes, articles and channels.
pub(crate) fn profile_events(pubkey: [u8; 32]) -> NostrFilter {
    NostrFilter {
        authors: vec![pubkey],
        kinds: vec![NOTE, ARTICLE, CHANNEL],
        limit: Some(PROFILE_EVENTS),
        ..NostrFilter::default()
    }
}

/// A channel's latest messages.
pub(crate) fn channel_messages(channel: [u8; 32]) -> NostrFilter {
    NostrFilter {
        kinds: vec![CHANNEL_MESSAGE],
        e: vec![channel],
        limit: Some(CHANNEL_MESSAGES),
        ..NostrFilter::default()
    }
}

/// The newest of `events`: a replaceable event's current version.
pub(crate) fn newest(events: Vec<NostrEvent>) -> Option<NostrEvent> {
    events.into_iter().max_by_key(|e| (e.created_at, e.id))
}

/// The address an event of a profile's is linked by: an article by its
/// kind, author and `d` tag, so its newest version is the one crawled.
pub(crate) fn address_of(event: &NostrEvent) -> String {
    let address = match (event.kind, event.tag_values("d").next()) {
        (30000..40000, Some(d)) => NostrTarget::address(event.kind, event.pubkey, d),
        _ => None,
    };
    // An identifier NIP-19 cannot write: by the event's ID instead.
    let target = address.unwrap_or(NostrTarget::Event(event.id));
    Nostr { target, relays: vec![] }.to_string()
}

/// A profile (kind 0): its name and what it says about itself. The links are
/// its website and the events given.
pub(crate) fn profile(metadata: &NostrEvent, events: &[NostrEvent]) -> Fields {
    let json: serde_json::Value = serde_json::from_str(&metadata.content).unwrap_or_default();
    let field = |name: &str| json.get(name).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let name =
        field("display_name").or_else(|| field("name")).map(str::to_owned).unwrap_or_else(|| short(&metadata.pubkey));
    let about = field("about").unwrap_or_default();
    let mut links: Vec<String> = field("website").into_iter().map(str::to_owned).collect();
    links.extend(events.iter().map(address_of));
    let nip05 = field("nip05").map(|n| format!(" {n}")).unwrap_or_default();
    Fields {
        title: collapse(&name, MAX_TITLE),
        description: collapse(about, MAX_DESCRIPTION),
        body: collapse(&format!("{name}{nip05} {about}"), crate::extract::MAX_TEXT_BYTES),
        keywords: vec![npub(&metadata.pubkey)],
        links,
    }
}

/// Any other event: a note, an article, a channel with `messages`.
pub(crate) fn event(event: &NostrEvent, messages: &[NostrEvent]) -> Fields {
    let (mut title, mut description, mut text) = match event.kind {
        ARTICLE => (
            event.tag_values("title").next().unwrap_or_default().to_owned(),
            event.tag_values("summary").next().unwrap_or_default().to_owned(),
            event.content.clone(),
        ),
        CHANNEL => {
            let json: serde_json::Value = serde_json::from_str(&event.content).unwrap_or_default();
            let field = |name: &str| json.get(name).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            (field("name"), field("about"), field("about"))
        }
        _ => (String::new(), String::new(), event.content.clone()),
    };
    for message in messages {
        text.push('\n');
        text.push_str(&message.content);
    }
    let first_line = event.content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if title.trim().is_empty() {
        title = if first_line.is_empty() || event.kind == CHANNEL {
            format!("{} by {}", kind_name(event.kind), short(&event.pubkey))
        } else {
            first_line.to_owned()
        };
    }
    if description.trim().is_empty() {
        description = text.clone();
    }
    let mut links = tag_links(event);
    links.extend(text_links(&event.content));
    for message in messages {
        links.extend(text_links(&message.content));
    }
    links.truncate(MAX_LINKS);
    Fields {
        title: collapse(&title, MAX_TITLE),
        description: collapse(&description, MAX_DESCRIPTION),
        body: collapse(&text, crate::extract::MAX_TEXT_BYTES),
        keywords: event.tag_values("t").take(MAX_KEYWORDS).map(str::to_owned).collect(),
        links,
    }
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        NOTE => "Note",
        CHANNEL => "Channel",
        CHANNEL_MESSAGE => "Message",
        ARTICLE => "Article",
        _ => "Event",
    }
}

/// `npub1abcdefgh…` for someone with no name.
fn short(pubkey: &[u8; 32]) -> String {
    let npub = npub(pubkey);
    format!("{}…", truncate(&npub, 16))
}

/// The events (`e`, `q`), people (`p`) and articles (`a`) an event's tags
/// name, as `nostr:` entities, with a tag's relay hint where it has one.
fn tag_links(event: &NostrEvent) -> Vec<String> {
    let mut links = Vec::new();
    for tag in &event.tags {
        let (Some(name), Some(value)) = (tag.first(), tag.get(1)) else { continue };
        let relays: Vec<String> =
            tag.get(2).filter(|r| r.starts_with("wss://") || r.starts_with("ws://")).cloned().into_iter().collect();
        let key = |hex: &str| -> Option<[u8; 32]> {
            let bytes: Option<Vec<u8>> =
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect();
            bytes?.try_into().ok()
        };
        let target = match name.as_str() {
            "e" | "q" => key(value).map(NostrTarget::Event),
            "p" => key(value).map(NostrTarget::Profile),
            "a" => {
                let mut parts = value.splitn(3, ':');
                match (parts.next().and_then(|k| k.parse().ok()), parts.next().and_then(key), parts.next()) {
                    (Some(kind), Some(author), Some(identifier)) => NostrTarget::address(kind, author, identifier),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(target) = target {
            links.push(Nostr { target, relays }.to_string());
        }
    }
    links
}

/// `nostr:` entities and web addresses written in an event's text.
fn text_links(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || "()<>[]\"'".contains(c))
        .map(|word| word.trim_end_matches(['.', ',', ';', ':', '!', '?']))
        .filter(|word| {
            let lower = word.get(..8).unwrap_or(word).to_ascii_lowercase();
            lower.starts_with("nostr:") || lower.starts_with("https://") || lower.starts_with("http://")
        })
        .map(str::to_owned)
        .collect()
}

/// For the fuzz target in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    use super::*;

    /// An event as the crawler indexes it, with a channel's `messages`.
    /// What the index gets stays within the caps, and each address the
    /// crawler writes reads back as itself and names what it meant.
    pub fn fields(event: &NostrEvent, messages: &[NostrEvent]) {
        let fields = if event.kind == PROFILE { profile(event, messages) } else { super::event(event, messages) };
        assert!(fields.title.len() <= MAX_TITLE && fields.description.len() <= MAX_DESCRIPTION);
        assert!(fields.body.len() <= crate::extract::MAX_TEXT_BYTES && fields.keywords.len() <= MAX_KEYWORDS);
        assert!(event.kind == PROFILE || fields.links.len() <= MAX_LINKS);
        for link in tag_links(event).into_iter().chain([address_of(event)]) {
            let parsed = Nostr::parse(&link).unwrap_or_else(|| panic!("{link} does not parse"));
            assert_eq!(parsed.to_string(), link);
        }
        match Nostr::parse(&address_of(event)).map(|n| n.target) {
            Some(NostrTarget::Event(id)) => assert_eq!(id, event.id),
            Some(NostrTarget::Address { kind, author, identifier }) => {
                assert_eq!((kind, author), (event.kind, event.pubkey));
                assert_eq!(event.tag_values("d").next(), Some(identifier.as_str()));
            }
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u32, tags: Vec<Vec<&str>>, content: &str) -> NostrEvent {
        NostrEvent {
            id: [1; 32],
            pubkey: [2; 32],
            created_at: 1,
            kind,
            tags: tags.into_iter().map(|t| t.into_iter().map(str::to_owned).collect()).collect(),
            content: content.into(),
        }
    }

    #[test]
    fn a_note_is_its_first_line_and_its_links() {
        let hex_id = "03".repeat(32);
        let note = event(
            NOTE,
            vec![vec!["e", &hex_id, "wss://relay.example"], vec!["p", &"04".repeat(32)], vec!["t", "privacy"]],
            "Tor and Nostr together\nSee https://example.com/x, and nostr:npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg.",
        );
        let fields = super::event(&note, &[]);
        assert_eq!(fields.title, "Tor and Nostr together");
        assert_eq!(fields.keywords, ["privacy"]);
        assert!(fields.body.contains("See https://example.com/x"));
        assert_eq!(fields.links.len(), 4, "{:?}", fields.links);
        assert!(fields.links[0].starts_with("nostr:note1"), "the e tag: {:?}", fields.links);
        assert!(fields.links[1].starts_with("nostr:npub1"));
        assert_eq!(fields.links[2], "https://example.com/x");
        assert_eq!(fields.links[3], "nostr:npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg");
    }

    #[test]
    fn an_article_has_its_title_and_summary() {
        let a = format!("30023:{}:other-article", "05".repeat(32));
        let article = event(
            ARTICLE,
            vec![vec!["title", "On relays"], vec!["summary", "Why they matter"], vec!["a", &a]],
            "# On relays\nLong text",
        );
        let fields = super::event(&article, &[]);
        assert_eq!((fields.title.as_str(), fields.description.as_str()), ("On relays", "Why they matter"));
        assert!(fields.links[0].starts_with("nostr:naddr1"));
        let mut versioned = article.clone();
        versioned.tags.push(vec!["d".into(), "on-relays".into()]);
        assert!(address_of(&versioned).starts_with("nostr:naddr1"), "an article is linked by its address");
        assert!(address_of(&event(NOTE, vec![], "x")).starts_with("nostr:note1"));
    }

    #[test]
    fn an_identifier_nip19_cannot_write_is_linked_by_the_events_id() {
        let linked = |e: &NostrEvent| Nostr::parse(&address_of(e)).map(|n| n.target);
        // A TLV value has one length byte: 255 bytes is the longest identifier.
        let longest = "d".repeat(255);
        let article = event(ARTICLE, vec![vec!["d", &longest]], "x");
        let address = NostrTarget::Address { kind: ARTICLE, author: article.pubkey, identifier: longest };
        assert_eq!(linked(&article), Some(address));
        assert_eq!(linked(&event(ARTICLE, vec![vec!["d", &"d".repeat(256)]], "x")), Some(NostrTarget::Event([1; 32])));
        // 300 bytes would be written with length 44 (300 as u8), and the rest
        // read as records: an author ([9; 32]) and a kind, read before the
        // real ones, then two records of an unknown type to skip.
        let mut forged = "x".repeat(44);
        forged.push_str("\u{2} ");
        forged.push_str(&"\t".repeat(32));
        forged.push_str("\u{3}\u{4}\0\0uG");
        for _ in 0..2 {
            forged.push_str("\u{5}j");
            forged.push_str(&"x".repeat(106));
        }
        assert_eq!(forged.len(), 300);
        let article = event(ARTICLE, vec![vec!["d", &forged]], "x");
        assert_eq!(linked(&article), Some(NostrTarget::Event([1; 32])), "linked by its ID, not another's address");
    }

    #[test]
    fn an_a_tag_nip19_cannot_write_is_skipped() {
        let a = |identifier: &str| format!("30023:{}:{identifier}", "05".repeat(32));
        let tags = vec![vec!["a".to_owned(), a(&"i".repeat(255))], vec!["a".to_owned(), a(&"i".repeat(256))]];
        let mut note = event(NOTE, vec![], "x");
        note.tags = tags;
        let links = tag_links(&note);
        assert_eq!(links.len(), 1, "{links:?}");
        let Some(NostrTarget::Address { identifier, .. }) = Nostr::parse(&links[0]).map(|n| n.target) else {
            panic!("{links:?}")
        };
        assert_eq!(identifier.len(), 255);
    }

    #[test]
    fn a_channel_holds_its_messages() {
        let channel = event(CHANNEL, vec![], r#"{"name":"Tor users","about":"Help with Tor"}"#);
        let messages = [
            event(CHANNEL_MESSAGE, vec![], "how do bridges work?"),
            event(CHANNEL_MESSAGE, vec![], "see https://bridges.torproject.org"),
        ];
        let fields = super::event(&channel, &messages);
        assert_eq!(fields.title, "Tor users");
        assert_eq!(fields.description, "Help with Tor");
        assert!(fields.body.contains("how do bridges work?"));
        assert_eq!(fields.links, ["https://bridges.torproject.org"]);
        let unnamed = super::event(&event(CHANNEL, vec![], "not json"), &[]);
        assert!(unnamed.title.starts_with("Channel by npub1"), "{}", unnamed.title);
    }

    #[test]
    fn a_profile_is_its_name_about_and_events() {
        let metadata = event(
            PROFILE,
            vec![],
            r#"{"name":"alice","display_name":"Alice","about":"I run relays","website":"https://alice.example","nip05":"alice@alice.example"}"#,
        );
        let fields = profile(&metadata, &[event(NOTE, vec![], "hi")]);
        assert_eq!(fields.title, "Alice");
        assert_eq!(fields.description, "I run relays");
        assert!(fields.body.contains("alice@alice.example"));
        assert_eq!(fields.links[0], "https://alice.example");
        assert!(fields.links[1].starts_with("nostr:note1"));
        let nameless = profile(&event(PROFILE, vec![], "{}"), &[]);
        assert!(nameless.title.starts_with("npub1") && nameless.title.ends_with('…'));
    }

    #[test]
    fn bad_tags_are_skipped_and_links_capped() {
        let short = "0".repeat(63);
        let tags = vec![vec!["e", "nothex"], vec!["p"], vec!["a", "notakind:xx:y"], vec!["e", &short]];
        let fields = super::event(&event(NOTE, tags, "x"), &[]);
        assert!(fields.links.is_empty(), "{:?}", fields.links);
        let many = (0..300).map(|i| format!("https://example.com/{i}")).collect::<Vec<_>>().join(" ");
        assert_eq!(super::event(&event(NOTE, vec![], &many), &[]).links.len(), MAX_LINKS);
    }
}
