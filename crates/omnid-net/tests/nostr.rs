//! Nostr through relays, against local stand-ins: events found by ID and by
//! author across relays, forged or altered events refused, and what is said
//! when no relay answers.

#[path = "support/relay.rs"]
mod relay;

use omnid_core::Network;
use omnid_net::{FetchError, Gateway, GatewayConfig, NostrFilter, RouterState};
use serde_json::json;
use url::Url;

use relay::{hex, key, pubkey, signed};

fn gateway(relays: &[&str], clearnet_via_tor: bool) -> Gateway {
    Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        ipfs_gateways: vec![],
        nostr_relays: relays.iter().map(|r| Url::parse(r).unwrap()).collect(),
        // The stand-in relays are on 127.0.0.1.
        allow_private_addresses: true,
        clearnet_via_tor,
        ..GatewayConfig::default()
    })
    .unwrap()
}

fn id_of(event: &serde_json::Value) -> [u8; 32] {
    let hex = event["id"].as_str().unwrap();
    (0..32).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<_>>().try_into().unwrap()
}

#[tokio::test]
async fn finds_an_event_by_id_and_refuses_an_altered_copy() {
    let note = signed(&key(1), 1_790_000_000, 1, json!([["t", "nostr"]]), "hello from a relay");
    let mut altered = note.clone();
    altered["content"] = json!("words the author never wrote");
    // The first relay has only the altered copy; the second, the real one.
    let liar = relay::relay(vec![altered]).await;
    let honest = relay::relay(vec![note.clone()]).await;
    let gateway = gateway(&[&liar.url, &honest.url], false);
    let filter = NostrFilter { ids: vec![id_of(&note)], ..NostrFilter::default() };
    let events = gateway.nostr(&filter, &[]).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].content, "hello from a relay");
    assert_eq!(events[0].pubkey, pubkey(&key(1)));
    assert_eq!(*honest.asked.lock().unwrap(), [json!({"ids": [hex(&id_of(&note))], "limit": 100})]);

    // With only the liar, nothing: an altered event is never taken.
    let alone = gateway_for(&liar.url);
    assert!(alone.nostr(&filter, &[]).await.unwrap().is_empty());
}

fn gateway_for(relay: &str) -> Gateway {
    gateway(&[relay], false)
}

#[tokio::test]
async fn merges_relays_and_asks_a_links_relay_first() {
    let alice = key(2);
    let notes: Vec<_> = (0..3).map(|i| signed(&alice, 1_790_000_000 + i, 1, json!([]), &format!("note {i}"))).collect();
    let one = relay::relay(notes[..2].to_vec()).await;
    let other = relay::relay(notes[1..].to_vec()).await;
    let gateway = gateway(&[&one.url], false);
    let filter = NostrFilter { authors: vec![pubkey(&alice)], kinds: vec![1], ..NostrFilter::default() };
    // The other relay only as the link's hint.
    let mut events = gateway.nostr(&filter, std::slice::from_ref(&other.url)).await.unwrap();
    events.sort_by_key(|e| e.created_at);
    assert_eq!(events.iter().map(|e| e.content.as_str()).collect::<Vec<_>>(), ["note 0", "note 1", "note 2"]);
    assert_eq!(other.asked.lock().unwrap().len(), 1, "the hint was asked");
}

#[tokio::test]
async fn drops_events_a_relay_sends_that_were_not_asked_for() {
    let wanted = signed(&key(3), 1_790_000_000, 0, json!([]), r#"{"name":"carol"}"#);
    let other = signed(&key(4), 1_790_000_000, 0, json!([]), r#"{"name":"mallory"}"#);
    // A relay that ignores the filter and sends everything it has.
    let careless = relay::careless_relay(vec![other, wanted.clone()]).await;
    let gateway = gateway_for(&careless.url);
    let filter = NostrFilter { authors: vec![pubkey(&key(3))], kinds: vec![0], ..NostrFilter::default() };
    let events = gateway.nostr(&filter, &[]).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].content, r#"{"name":"carol"}"#);
}

#[tokio::test]
async fn says_plainly_when_no_relay_answers() {
    let filter = NostrFilter { ids: vec![[1; 32]], ..NostrFilter::default() };
    match gateway(&[], false).nostr(&filter, &[]).await {
        Err(FetchError::Unavailable { network: Network::Nostr, reason }) => assert!(reason.contains("no Nostr relay")),
        other => panic!("{other:?}"),
    }
    let error = gateway(&["ws://127.0.0.1:9/"], false).nostr(&filter, &[]).await.unwrap_err();
    assert!(error.to_string().contains("ws://127.0.0.1:9/"), "{error}");
    // Hints that are not relays are passed over.
    let error = gateway(&[], false).nostr(&filter, &["https://not.a.relay/".into()]).await.unwrap_err();
    assert!(error.to_string().contains("no Nostr relay"), "{error}");
}

#[tokio::test]
async fn reports_the_relays_without_probing_them() {
    let status = gateway(&["wss://relay.one.example/", "wss://relay.two.example/"], true).status().await;
    let nostr = status.iter().find(|s| s.network == Network::Nostr).unwrap();
    assert_eq!(nostr.state, RouterState::Ready);
    assert_eq!(nostr.detail, "relays through Tor: relay.one.example, relay.two.example");
    let none = gateway(&[], false).status().await;
    assert_eq!(none.iter().find(|s| s.network == Network::Nostr).unwrap().state, RouterState::Disabled);
}

#[test]
fn relays_are_reached_as_the_clearnet_is() {
    let direct = gateway(&[], false);
    assert!(direct.leaves_anonymity(Network::Onion, Network::Nostr), "an onion page could plant an event and watch");
    assert!(!direct.leaves_anonymity(Network::Clearnet, Network::Nostr));
    assert!(!direct.leaves_anonymity(Network::Nostr, Network::Clearnet));
    let via_tor = gateway(&[], true);
    assert!(!via_tor.leaves_anonymity(Network::Onion, Network::Nostr), "through Tor exits");
    assert!(via_tor.leaves_anonymity(Network::Nostr, Network::Ipfs));
}
