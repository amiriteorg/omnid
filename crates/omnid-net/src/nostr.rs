//! Reading Nostr relays (NIP-01): one subscription for the events a filter
//! matches, read until the relay says it has sent what it stores (`EOSE`).
//!
//! No event is believed before it is checked: its ID must be the hash of
//! what it says, and its signature the author's (BIP-340). A relay can
//! withhold events, but not forge or alter one. Relays are reached the way
//! the clearnet is, so through Tor exits with `clearnet_via_tor`, and an
//! onion relay over Tor.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use k256::schnorr::{Signature, VerifyingKey};
use omnid_core::Network;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use url::Url;

use crate::tor::Circuits;
use crate::{BoxError, FetchError, Gateway, ws};

/// A relay message, at most: an event with a long article in it fits.
const MAX_MESSAGE: usize = 512 << 10;
/// Events taken from one subscription.
pub const MAX_EVENTS: usize = 100;
/// Tags on one event, and strings in one tag; an event with more is refused.
const MAX_TAGS: usize = 2_000;
const MAX_TAG_LEN: usize = 16;
/// How long a relay has to send what it stores.
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
/// Relays asked at once for one query.
const RELAYS_PER_QUERY: usize = 4;
/// Messages read from one relay for one query, events and notices alike.
const MAX_MESSAGES: usize = 2 * MAX_EVENTS;

/// An event, checked: its ID is its content's hash, and the author signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: [u8; 32],
    pub pubkey: [u8; 32],
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Event {
    /// The first value of each tag named `name`: `e`, `p`, `a`, `d`, `title`.
    pub fn tag_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.tags
            .iter()
            .filter(move |t| t.first().is_some_and(|n| n == name))
            .filter_map(|t| t.get(1))
            .map(String::as_str)
    }
}

/// What a subscription asks for (NIP-01's filter), the fields omnid uses.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub ids: Vec<[u8; 32]>,
    pub authors: Vec<[u8; 32]>,
    pub kinds: Vec<u32>,
    /// `#d`: an addressable event's identifier.
    pub d: Vec<String>,
    /// `#e`: events that refer to these, such as a channel's messages.
    pub e: Vec<[u8; 32]>,
    /// At most 100.
    pub limit: Option<usize>,
}

impl Filter {
    fn to_json(&self) -> Value {
        let hexes = |keys: &[[u8; 32]]| keys.iter().map(|k| hex(k)).collect::<Vec<_>>();
        let mut filter = serde_json::Map::new();
        if !self.ids.is_empty() {
            filter.insert("ids".into(), json!(hexes(&self.ids)));
        }
        if !self.authors.is_empty() {
            filter.insert("authors".into(), json!(hexes(&self.authors)));
        }
        if !self.kinds.is_empty() {
            filter.insert("kinds".into(), json!(self.kinds));
        }
        if !self.d.is_empty() {
            filter.insert("#d".into(), json!(self.d));
        }
        if !self.e.is_empty() {
            filter.insert("#e".into(), json!(hexes(&self.e)));
        }
        filter.insert("limit".into(), json!(self.limit.unwrap_or(MAX_EVENTS).min(MAX_EVENTS)));
        Value::Object(filter)
    }

    /// Whether `event` is one this filter asked for: a relay's other events
    /// are dropped.
    fn matches(&self, event: &Event) -> bool {
        (self.ids.is_empty() || self.ids.contains(&event.id))
            && (self.authors.is_empty() || self.authors.contains(&event.pubkey))
            && (self.kinds.is_empty() || self.kinds.contains(&event.kind))
            && (self.d.is_empty() || event.tag_values("d").any(|d| self.d.iter().any(|w| w == d)))
            && (self.e.is_empty() || event.tag_values("e").any(|e| self.e.iter().any(|w| hex(w) == e)))
    }
}

impl Gateway {
    /// The events `filter` matches on the relays `hints` names and the
    /// configured ones, four of them at once: each event checked, and each
    /// once. An error only when no relay answered.
    pub async fn nostr(&self, filter: &Filter, hints: &[String]) -> Result<Vec<Event>, FetchError> {
        let network = Network::Nostr;
        let mut relays: Vec<Url> = Vec::new();
        for relay in hints.iter().filter_map(|h| Url::parse(h).ok()).chain(self.cfg.nostr_relays.iter().cloned()) {
            if matches!(relay.scheme(), "ws" | "wss") && !relays.contains(&relay) {
                relays.push(relay);
            }
        }
        if relays.is_empty() {
            return Err(FetchError::Unavailable { network, reason: "no Nostr relay configured".into() });
        }
        let request = json!(["REQ", "omnid", filter.to_json()]).to_string();
        let mut running: Vec<Asking<'_>> = relays
            .into_iter()
            .take(RELAYS_PER_QUERY)
            .map(|relay| {
                let request = request.as_str();
                Box::pin(async move {
                    let answer = tokio::time::timeout(RELAY_TIMEOUT, self.ask_relay(&relay, request, filter)).await;
                    (relay, answer.unwrap_or_else(|_| Err("no answer in time".into())))
                }) as Asking<'_>
            })
            .collect();
        let mut events: Vec<Event> = Vec::new();
        let (mut answered, mut errors) = (false, Vec::new());
        while let Some((relay, answer)) = next_done(&mut running).await {
            match answer {
                Ok(found) => {
                    answered = true;
                    for event in found {
                        if events.len() < MAX_EVENTS && !events.iter().any(|e| e.id == event.id) {
                            events.push(event);
                        }
                    }
                    // Events asked for by ID: one relay having them is enough.
                    if !filter.ids.is_empty() && filter.ids.iter().all(|id| events.iter().any(|e| e.id == *id)) {
                        break;
                    }
                }
                Err(error) => errors.push(format!("{relay}: {error}")),
            }
        }
        if !answered {
            return Err(FetchError::Transport { network, error: errors.join("; ").into() });
        }
        Ok(events)
    }

    /// One relay's stored events for `request`, checked.
    async fn ask_relay(&self, relay: &Url, request: &str, filter: &Filter) -> Result<Vec<Event>, BoxError> {
        let host = relay.host_str().ok_or("a relay without a host")?.to_owned();
        let port = relay.port_or_known_default().ok_or("a relay without a port")?;
        let path = &relay[url::Position::BeforePath..url::Position::AfterQuery];
        let path = if path.is_empty() { "/" } else { path };
        let stream = self.connect_on(&host, port, Circuits::Shared).await.map_err(|e| -> BoxError { e.into() })?;
        let host_header = match relay.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        if relay.scheme() == "wss" {
            let name = ServerName::try_from(host.trim_start_matches('[').trim_end_matches(']').to_owned())?;
            let tls = TlsConnector::from(Arc::new(crate::tls_config()?)).connect(name, stream).await?;
            read_events(ws::connect(tls, &host_header, path, MAX_MESSAGE).await?, request, filter).await
        } else {
            read_events(ws::connect(stream, &host_header, path, MAX_MESSAGE).await?, request, filter).await
        }
    }
}

/// Sends the subscription and reads until `EOSE`, the subscription's end or
/// the connection's: the events that check out and match, at most
/// [`MAX_EVENTS`].
async fn read_events<S>(mut ws: ws::WebSocket<S>, request: &str, filter: &Filter) -> Result<Vec<Event>, BoxError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    ws.send_text(request).await?;
    let mut events = Vec::new();
    for _ in 0..MAX_MESSAGES {
        let Some(message) = ws.recv_text().await? else { break };
        let Ok(Value::Array(message)) = serde_json::from_str::<Value>(&message) else { continue };
        match message.first().and_then(Value::as_str) {
            Some("EVENT") if message.get(1).and_then(Value::as_str) == Some("omnid") => {
                if let Some(event) = message.get(2).and_then(checked_event).filter(|e| filter.matches(e)) {
                    events.push(event);
                }
                if events.len() >= MAX_EVENTS {
                    break;
                }
            }
            Some("EOSE") => break,
            Some("CLOSED") => {
                let why = message.get(2).and_then(Value::as_str).unwrap_or_default();
                if events.is_empty() {
                    return Err(format!("the relay closed the subscription: {why}").into());
                }
                break;
            }
            _ => {}
        }
    }
    let _ = ws.send_text(&json!(["CLOSE", "omnid"]).to_string()).await;
    Ok(events)
}

/// An event from a relay, if its ID is the hash of its content and its
/// signature its author's.
fn checked_event(value: &Value) -> Option<Event> {
    let key = |name: &str| -> Option<[u8; 32]> { unhex(value.get(name)?.as_str()?)?.try_into().ok() };
    let (id, pubkey) = (key("id")?, key("pubkey")?);
    let sig: [u8; 64] = unhex(value.get("sig")?.as_str()?)?.try_into().ok()?;
    let created_at = value.get("created_at")?.as_u64()?;
    let kind = u32::try_from(value.get("kind")?.as_u64()?).ok()?;
    let content = value.get("content")?.as_str()?.to_owned();
    let tags = value.get("tags")?.as_array()?;
    if tags.len() > MAX_TAGS {
        return None;
    }
    let tags: Vec<Vec<String>> = tags
        .iter()
        .map(|tag| {
            let tag = tag.as_array().filter(|t| t.len() <= MAX_TAG_LEN)?;
            tag.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()
        })
        .collect::<Option<_>>()?;
    let event = Event { id, pubkey, created_at, kind, tags, content };
    (event_id(&event) == id && verify(&event, &sig)).then_some(event)
}

/// NIP-01's ID: SHA-256 of `[0,pubkey,created_at,kind,tags,content]` as
/// compact JSON, with only `"`, `\`, and the line break, carriage return,
/// tab, backspace and form feed escaped.
pub(crate) fn event_id(event: &Event) -> [u8; 32] {
    let mut out = format!("[0,\"{}\",{},{},[", hex(&event.pubkey), event.created_at, event.kind);
    for (i, tag) in event.tags.iter().enumerate() {
        out.push_str(if i == 0 { "[" } else { ",[" });
        for (j, value) in tag.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            push_string(&mut out, value);
        }
        out.push(']');
    }
    out.push_str("],");
    push_string(&mut out, &event.content);
    out.push(']');
    Sha256::digest(out.as_bytes()).into()
}

fn push_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn verify(event: &Event, sig: &[u8; 64]) -> bool {
    let (Ok(key), Ok(sig)) = (VerifyingKey::from_slice(&event.pubkey), Signature::from_slice(sig)) else {
        return false;
    };
    key.verify_raw(&event.id, &sig).is_ok()
}

/// A relay being asked, and what it answers.
type Asking<'a> = Pin<Box<dyn Future<Output = (Url, Result<Vec<Event>, BoxError>)> + Send + 'a>>;

/// The next of `running` to finish, taken out of it; `None` when none is left.
async fn next_done<T>(running: &mut Vec<Pin<Box<dyn Future<Output = T> + Send + '_>>>) -> Option<T> {
    if running.is_empty() {
        return None;
    }
    std::future::poll_fn(|cx| {
        for i in 0..running.len() {
            if let Poll::Ready(value) = running[i].as_mut().poll(cx) {
                drop(running.swap_remove(i));
                return Poll::Ready(Some(value));
            }
        }
        Poll::Pending
    })
    .await
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.len() > 256 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use k256::schnorr::SigningKey;

    use super::*;

    /// An event signed by `key`, as a relay sends it.
    pub(crate) fn signed(key: &SigningKey, kind: u32, tags: Vec<Vec<String>>, content: &str) -> (Event, Value) {
        let pubkey: [u8; 32] = key.verifying_key().to_bytes().into();
        let mut event = Event { id: [0; 32], pubkey, created_at: 1_790_000_000, kind, tags, content: content.into() };
        event.id = event_id(&event);
        let sig = key.sign_raw(&event.id, &[0; 32]).unwrap();
        let json = json!({
            "id": hex(&event.id), "pubkey": hex(&event.pubkey), "created_at": event.created_at, "kind": kind,
            "tags": event.tags, "content": event.content, "sig": hex(&sig.to_bytes()),
        });
        (event, json)
    }

    pub(crate) fn key(seed: u8) -> SigningKey {
        SigningKey::from_slice(&[seed; 32]).unwrap()
    }

    #[test]
    fn takes_an_event_whose_id_and_signature_check_out() {
        let tags = vec![vec!["e".into(), "ab".into()], vec!["t".into(), "nostr".into()]];
        let (event, json) = signed(&key(1), 1, tags, "hello \"world\"\n\ttab \\ é \u{1}");
        assert_eq!(checked_event(&json), Some(event));
    }

    #[test]
    fn refuses_altered_and_forged_events() {
        let (_, json) = signed(&key(1), 1, vec![], "original");
        let mut altered = json.clone();
        altered["content"] = json!("altered");
        assert_eq!(checked_event(&altered), None, "the ID no longer matches");
        // Another author's name on it, with the ID made to match: the
        // signature still is not theirs.
        let (other, _) = signed(&key(2), 1, vec![], "original");
        let mut forged = json.clone();
        forged["pubkey"] = json!(hex(&other.pubkey));
        let mut event = checked_fields(&forged);
        event.id = event_id(&event);
        forged["id"] = json!(hex(&event.id));
        assert_eq!(checked_event(&forged), None, "signed by someone else");
        for field in ["id", "pubkey", "sig", "created_at", "kind", "tags", "content"] {
            let mut missing = json.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert_eq!(checked_event(&missing), None, "{field}");
        }
        let mut long = json.clone();
        long["tags"] = json!(vec![vec!["t"]; MAX_TAGS + 1]);
        assert_eq!(checked_event(&long), None, "too many tags");
    }

    fn checked_fields(json: &Value) -> Event {
        Event {
            id: [0; 32],
            pubkey: unhex(json["pubkey"].as_str().unwrap()).unwrap().try_into().unwrap(),
            created_at: json["created_at"].as_u64().unwrap(),
            kind: json["kind"].as_u64().unwrap() as u32,
            tags: vec![],
            content: json["content"].as_str().unwrap().into(),
        }
    }

    #[test]
    fn nip01s_serialization_escapes_only_what_it_names() {
        let event = Event {
            id: [0; 32],
            pubkey: [0xab; 32],
            created_at: 1,
            kind: 1,
            tags: vec![vec!["p".into(), "x".into()]],
            content: "a\"b\\c\nd\u{1}e/é".into(),
        };
        let mut expected = format!("[0,\"{}\",1,1,[[\"p\",\"x\"]],", "ab".repeat(32));
        expected.push_str("\"a\\\"b\\\\c\\nd\u{1}e/é\"]");
        assert_eq!(event_id(&event), <[u8; 32]>::from(Sha256::digest(expected.as_bytes())));
    }

    #[test]
    fn filters_ask_for_what_omnid_uses_and_cap_the_limit() {
        let filter = Filter { authors: vec![[1; 32]], kinds: vec![0], limit: Some(5000), ..Filter::default() };
        assert_eq!(filter.to_json(), json!({"authors": [hex(&[1; 32])], "kinds": [0], "limit": MAX_EVENTS}));
        let (event, _) = signed(&key(3), 1, vec![vec!["e".into(), hex(&[9; 32])]], "x");
        assert!(Filter { e: vec![[9; 32]], ..Filter::default() }.matches(&event));
        assert!(!Filter { kinds: vec![0], ..Filter::default() }.matches(&event));
        assert!(!Filter { authors: vec![[1; 32]], ..Filter::default() }.matches(&event));
    }
}
