//! The HTTP API, driven in-process: request checks, every endpoint's
//! contract, and what reaches the crawler.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use omnid::api::{self, AppState};
use omnid_core::{IndexedDoc, Network};
use omnid_crawl::{CrawlControl, Job, Safety};
use omnid_index::SearchIndex;
use omnid_net::{Gateway, GatewayConfig};
use serde_json::Value;
use tokio::sync::mpsc;
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:7171";

struct Api {
    router: axum::Router,
    jobs: mpsc::Receiver<Job>,
    control: Arc<CrawlControl>,
}

fn api(token: Option<&str>) -> Api {
    api_with(token, None)
}

/// With `node`, a Hyphanet node's FProxy.
fn api_with(token: Option<&str>, node: Option<&str>) -> Api {
    let index = SearchIndex::in_memory().unwrap();
    for (url, network, title, lang) in [
        ("http://forum.example/", Network::Clearnet, "Privacy forum", "en"),
        ("http://forum.i2p/", Network::I2p, "Privacy forum mirror", "de"),
    ] {
        index
            .upsert(&IndexedDoc {
                url: url.into(),
                network,
                site: "site".into(),
                title: title.into(),
                description: "talk about privacy".into(),
                keywords: vec![],
                body: "privacy tools and guides".into(),
                content_hash: url.into(),
                info_hash: None,
                files: vec![],
                first_seen: 1,
                last_seen: 1,
                liveness: 1.0,
                trust: 0.5,
                lang: Some(lang.into()),
            })
            .unwrap();
    }
    index.commit().unwrap();
    let (jobs, rx) = mpsc::channel(4);
    let control = Arc::new(CrawlControl::default());
    let state = AppState {
        gateway: Arc::new(
            Gateway::new(GatewayConfig {
                tor_dir: None,
                i2p_proxy: None,
                fproxy: node.map(|n| n.parse().unwrap()),
                ..GatewayConfig::default()
            })
            .unwrap(),
        ),
        index: Arc::new(index),
        control: control.clone(),
        jobs,
        safety: Arc::new(Safety::from_lists("denied.example\nCHK@denied,x,y", "")),
        dht: None,
    };
    let listen: SocketAddr = LISTEN.parse().unwrap();
    Api { router: api::router(state, listen, token.map(str::to_owned)), jobs: rx, control }
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::get(path).header(header::HOST, LISTEN)
}

fn post_json(path: &str, body: &str) -> Request<Body> {
    Request::post(path)
        .header(header::HOST, LISTEN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn rejects_requests_not_addressed_to_localhost() {
    let api = api(None);
    for host in ["evil.example:7171", "127.0.0.1:9999", "localhost"] {
        let request = Request::get("/api/status").header(header::HOST, host).body(Body::empty()).unwrap();
        assert_eq!(send(&api.router, request).await.0, StatusCode::FORBIDDEN, "{host}");
    }
    for host in ["127.0.0.1:7171", "localhost:7171", "[::1]:7171"] {
        let request = Request::get("/api/status").header(header::HOST, host).body(Body::empty()).unwrap();
        assert_eq!(send(&api.router, request).await.0, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn requires_the_token_when_one_is_set() {
    let api = api(Some("s3cret"));
    let status = |request| async { send(&api.router, request).await.0 };
    assert_eq!(status(get("/api/status").body(Body::empty()).unwrap()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(get("/").body(Body::empty()).unwrap()).await, StatusCode::UNAUTHORIZED, "the page too");
    let bearer = get("/api/status").header(header::AUTHORIZATION, "Bearer s3cret").body(Body::empty()).unwrap();
    assert_eq!(status(bearer).await, StatusCode::OK);
    let cookie = get("/api/status").header(header::COOKIE, "a=b; omnid_token=s3cret").body(Body::empty()).unwrap();
    assert_eq!(status(cookie).await, StatusCode::OK);
    let wrong = get("/api/status").header(header::AUTHORIZATION, "Bearer s3cres").body(Body::empty()).unwrap();
    assert_eq!(status(wrong).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn searches_with_counts_snippets_and_filters() {
    let api = api(None);
    let (status, body) = send(&api.router, get("/api/search?q=privacy").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["network_counts"]["clearnet"], 1);
    assert_eq!(body["network_counts"]["i2p"], 1);
    assert_eq!(body["relaxed"], false);
    assert!(body["took_ms"].is_u64());
    let hit = &body["hits"][0];
    assert!(hit["title_spans"].as_array().unwrap().iter().any(|s| s["mark"] == true));
    assert!(hit["routes"][0]["url"].is_string());

    let (_, filtered) = send(&api.router, get("/api/search?q=privacy&network=i2p").body(Body::empty()).unwrap()).await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["hits"][0]["routes"][0]["network"], "i2p");

    let (status, body) = send(&api.router, get("/api/search?network=gopher").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("gopher"));
}

#[tokio::test]
async fn ranks_the_readers_languages_first() {
    let api = api(None);
    let top = |accept_language: &str, query: &str| {
        let request = get(&format!("/api/search?q=privacy{query}"))
            .header(header::ACCEPT_LANGUAGE, accept_language)
            .body(Body::empty())
            .unwrap();
        async { send(&api.router, request).await.1["hits"][0].clone() }
    };
    assert_eq!(top("en-US,en;q=0.9", "").await["routes"][0]["url"], "http://forum.example/");
    let german = top("de-DE", "").await;
    assert_eq!(german["routes"][0]["url"], "http://forum.i2p/");
    assert_eq!(german["lang"], "de");
    assert_eq!(top("en-US", "&lang=de").await["routes"][0]["url"], "http://forum.i2p/", "lang= overrides the header");
}

#[tokio::test]
async fn queues_submissions_and_refuses_bad_ones() {
    let mut api = api(None);
    let form = Request::post("/api/submit")
        .header(header::HOST, LISTEN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("url=http://example.com/"))
        .unwrap();
    assert_eq!(send(&api.router, form).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE, "a cross-site form post");
    assert_eq!(
        send(&api.router, post_json("/api/submit", r#"{"url":"abcdefghijklmnop.onion"}"#)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&api.router, post_json("/api/submit", r#"{"url":"https://denied.example/x"}"#)).await.0,
        StatusCode::FORBIDDEN
    );
    assert!(api.jobs.try_recv().is_err(), "nothing refused reaches the crawler");

    let (status, body) = send(&api.router, post_json("/api/submit", r#"{"url":"stats.i2p"}"#)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], "http://stats.i2p/");
    assert_eq!(body["network"], "i2p");
    let job = api.jobs.try_recv().unwrap();
    assert_eq!((job.source.to_string().as_str(), job.depth), ("http://stats.i2p/", 0));
}

#[tokio::test]
async fn pauses_and_resumes_the_crawl() {
    let api = api(None);
    let (status, body) = send(&api.router, post_json("/api/crawl", r#"{"paused":true}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["crawl"]["paused"], true);
    assert!(api.control.is_paused());
    send(&api.router, post_json("/api/crawl", r#"{"paused":false}"#)).await;
    assert!(!api.control.is_paused());
}

#[tokio::test]
async fn reports_status_for_every_network() {
    let api = api(None);
    api.control.pause_because("waiting for a charger");
    let (_, body) = send(&api.router, get("/api/status").body(Body::empty()).unwrap()).await;
    let networks: Vec<_> = body["routers"].as_array().unwrap().iter().map(|r| r["network"].as_str().unwrap()).collect();
    assert_eq!(networks, ["clearnet", "onion", "i2p", "hyphanet", "magnet"]);
    assert_eq!(body["routers"][1]["state"], "disabled", "Tor is off in this test");
    assert_eq!(body["index"]["documents"], 2);
    assert_eq!(body["pause_reason"], "waiting for a charger");
    assert_eq!(body["crawl"]["paused"], true);
}

#[tokio::test]
async fn serves_the_page_with_a_strict_policy() {
    let api = api(None);
    let pages =
        [("/", "text/html"), ("/app.js", "text/javascript"), ("/app.css", "text/css"), ("/icon.png", "image/png")];
    for (path, content_type) in pages {
        let response = api.router.clone().oneshot(get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let headers = response.headers();
        assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with(content_type), "{path}");
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"), "{csp}");
        assert!(csp.contains("img-src 'self';"), "the page's own icon, and no other image: {csp}");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    }
}

#[tokio::test]
async fn sends_hyphanet_keys_to_the_node() {
    // Outside the app, a Hyphanet result links here and the node's own
    // FProxy serves the page.
    let node = api_with(None, Some("http://127.0.0.1:8888/"));
    let key = "USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/";
    let response =
        node.router.clone().oneshot(get(&format!("/hyphanet/{key}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()[header::LOCATION], format!("http://127.0.0.1:8888/{key}").as_str());
    // Keys only: nothing else is sent on, to the node or anywhere.
    for path in ["/hyphanet/http:%2F%2Fevil.example/", "/hyphanet//evil.example/x", "/hyphanet/not-a-key"] {
        let (status, _) = send(&node.router, get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let (status, _) = send(&node.router, get("/hyphanet/CHK@denied,x,y/f").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the denylist holds");
    let (status, _) = send(&api(None).router, get(&format!("/hyphanet/{key}")).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no node configured");
}
