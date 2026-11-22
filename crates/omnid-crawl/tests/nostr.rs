//! Crawls Nostr from one profile on a stand-in relay: its notes, article and
//! chat channel, the notes and people they mention, and nothing by a denied
//! author.

#[path = "../../omnid-net/tests/support/relay.rs"]
mod relay;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use omnid_core::nostr::{Nostr, NostrTarget, npub};
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, CrawlStore, Crawler, Job, Politeness, SEED_TRUST, Safety};
use omnid_index::{SearchIndex, SearchQuery};
use omnid_net::{Gateway, GatewayConfig};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use relay::{hex, key, pubkey, signed};

#[tokio::test]
async fn crawls_a_profile_its_events_and_what_they_mention() {
    let (alice, bob, dave) = (key(1), key(2), key(3));
    let t = 1_790_000_000;
    let bob_note = signed(&bob, t, 1, json!([]), "Bob writes about walnuts");
    let dave_note = signed(&dave, t, 1, json!([]), "Dave writes about forbidden things");
    let channel = signed(&alice, t, 40, json!([]), r#"{"name":"Nut lovers","about":"A chat about nuts"}"#);
    let channel_id = channel["id"].as_str().unwrap().to_owned();
    let events = vec![
        signed(&alice, t, 0, json!([]), r#"{"name":"alice","about":"I write about pecans"}"#),
        // An older profile: the newest is the one indexed.
        signed(&alice, t - 100, 0, json!([]), r#"{"name":"old alice","about":"stale"}"#),
        signed(
            &alice,
            t + 1,
            1,
            json!([["e", bob_note["id"]], ["p", hex(&pubkey(&bob))], ["t", "nuts"]]),
            "Replying to Bob about hazelnuts",
        ),
        signed(&alice, t + 2, 1, json!([["e", dave_note["id"]]]), "Quoting Dave"),
        signed(&alice, t + 3, 30023, json!([["d", "almonds"], ["title", "All about almonds"]]), "Long text on almonds"),
        channel,
        signed(&bob, t + 4, 42, json!([["e", channel_id, "", "root"]]), "cashews are the best"),
        signed(&bob, t, 0, json!([]), r#"{"name":"bob","about":"Bob's profile"}"#),
        bob_note,
        dave_note,
    ];
    let relay = relay::relay(events).await;

    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        ipfs_gateways: vec![],
        nostr_relays: vec![relay.url.parse().unwrap()],
        allow_private_addresses: true,
        ..GatewayConfig::default()
    })
    .unwrap();
    let dave_npub = format!("nostr:{}", npub(&pubkey(&dave)));
    let safety = Safety::from_lists(&dave_npub, "");
    let no_delay = Politeness { concurrency: 4, site_delay: Duration::ZERO };
    let cfg = CrawlConfig {
        politeness: HashMap::from([(Network::Nostr, no_delay)]),
        commit_interval: Duration::from_millis(100),
        ..CrawlConfig::default()
    };
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let crawler = Crawler::new(Arc::new(gateway), index.clone(), Arc::new(safety), store, cfg);
    let alice_entity = Nostr { target: NostrTarget::Profile(pubkey(&alice)), relays: vec![] };
    let (jobs, rx) = tokio::sync::mpsc::channel(4);
    jobs.send(Job { source: Source::Nostr(alice_entity), depth: 0, site_depth: 0, trust: SEED_TRUST }).await.unwrap();
    drop(jobs);
    let stats = tokio::time::timeout(Duration::from_secs(30), crawler.run(rx, CancellationToken::new())).await.unwrap();

    let hits = |text: &str| {
        index
            .search(&SearchQuery {
                text: text.into(),
                networks: vec![Network::Nostr],
                limit: 10,
                ..SearchQuery::default()
            })
            .unwrap()
            .hits
    };
    // Alice's profile, her two notes, article and channel; Bob's note and
    // profile. Dave's note is linked, but its author is denied.
    assert_eq!(stats.indexed, 7, "{stats:?}");
    assert_eq!(stats.blocked, 1, "{stats:?}");
    let profile = &hits("pecans")[0];
    assert_eq!(profile.title, "alice", "the newest profile");
    assert!(hits("stale").is_empty());
    assert_eq!(hits("almonds")[0].title, "All about almonds");
    assert!(hits("almonds")[0].routes[0].url.starts_with("nostr:naddr1"));
    let channel = hits("cashews");
    assert_eq!(channel.len(), 1, "the message is in its channel");
    assert_eq!(channel[0].title, "Nut lovers");
    assert!(hits("walnuts")[0].routes[0].url.starts_with("nostr:note1"), "Bob's note, linked by its ID");
    assert_eq!(hits("Bob's profile").len(), 1);
    assert!(hits("forbidden").is_empty());
}
