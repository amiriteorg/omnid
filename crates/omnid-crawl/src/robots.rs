//! robots.txt, fetched once per site through the same gateway as the page,
//! so an onion's robots.txt is read over Tor.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use omnid_core::Source;
use omnid_net::{FetchError, FetchOptions, Gateway};
use texting_robots::Robot;

const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// A server error means "try later", not "crawl freely" (RFC 9309 §2.3.1.4).
const SERVER_ERROR_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_ROBOTS_BYTES: usize = 512 << 10;

enum Rules {
    AllowAll,
    DisallowAll,
    Parsed(Box<Robot>),
}

impl Rules {
    fn allows(&self, url: &str) -> bool {
        match self {
            Rules::AllowAll => true,
            Rules::DisallowAll => false,
            Rules::Parsed(robot) => robot.allowed(url),
        }
    }
}

pub(crate) struct RobotsCache {
    agent: String,
    sites: Mutex<HashMap<String, (Instant, Rules)>>,
}

impl RobotsCache {
    pub(crate) fn new(agent: &str) -> Self {
        Self { agent: agent.to_owned(), sites: Mutex::new(HashMap::new()) }
    }

    /// Whether `source` may be fetched. A network error fetching robots.txt
    /// is returned rather than cached: the site is probably down and the page
    /// fetch would fail too.
    pub(crate) async fn allowed(&self, gateway: &Gateway, source: &Source) -> Result<bool, FetchError> {
        let Some(robots) = source.robots_txt() else {
            return Ok(true);
        };
        let site = source.site_key();
        let url = source.to_string();
        if let Some((expires, rules)) = self.sites.lock().expect("robots lock poisoned").get(&site)
            && *expires > Instant::now()
        {
            return Ok(rules.allows(&url));
        }

        let opts = FetchOptions {
            max_bytes: MAX_ROBOTS_BYTES,
            accept: |ct| ct.is_none_or(|ct| ct.trim_start().to_ascii_lowercase().starts_with("text/")),
            max_redirects: 5,
        };
        let page = gateway.fetch(&robots, &opts).await?;
        let (rules, ttl) = match page.status {
            200..300 => match page.body.as_deref().map(|body| Robot::new(&self.agent, body)) {
                Some(Ok(robot)) => (Rules::Parsed(Box::new(robot)), TTL),
                _ => (Rules::AllowAll, TTL),
            },
            500.. => (Rules::DisallowAll, SERVER_ERROR_TTL),
            _ => (Rules::AllowAll, TTL),
        };
        let allowed = rules.allows(&url);
        self.sites.lock().expect("robots lock poisoned").insert(site, (Instant::now() + ttl, rules));
        Ok(allowed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::StatusCode;
    use axum::routing::get;
    use omnid_net::GatewayConfig;

    use super::*;

    /// A site whose robots.txt answers with `status` and `body`, counting
    /// how often it is asked.
    async fn site(status: u16, body: &'static str) -> (String, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let app = axum::Router::new().route(
            "/robots.txt",
            get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async move { (StatusCode::from_u16(status).unwrap(), body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, asked)
    }

    fn gateway() -> Gateway {
        Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
            .unwrap()
    }

    async fn allowed(cache: &RobotsCache, gateway: &Gateway, url: &str) -> bool {
        cache.allowed(gateway, &Source::parse(url).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn applies_rules_for_our_agent_and_caches_them() {
        let (base, asked) =
            site(200, "User-agent: *\nDisallow: /private\n\nUser-agent: omnid\nDisallow: /no-omnid\n").await;
        let (cache, gateway) = (RobotsCache::new("omnid"), gateway());
        assert!(!allowed(&cache, &gateway, &format!("{base}/no-omnid/page")).await, "our own group applies");
        assert!(allowed(&cache, &gateway, &format!("{base}/private/x")).await, "not the * group, once ours exists");
        assert!(allowed(&cache, &gateway, &format!("{base}/")).await);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "fetched once per site");
    }

    #[tokio::test]
    async fn a_missing_robots_txt_allows_everything() {
        let (base, _) = site(404, "").await;
        assert!(allowed(&RobotsCache::new("omnid"), &gateway(), &format!("{base}/anything")).await);
    }

    #[tokio::test]
    async fn a_server_error_disallows_everything_for_now() {
        let (base, asked) = site(503, "").await;
        let (cache, gateway) = (RobotsCache::new("omnid"), gateway());
        assert!(!allowed(&cache, &gateway, &format!("{base}/")).await);
        assert!(!allowed(&cache, &gateway, &format!("{base}/other")).await);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the refusal is cached too");
    }

    #[tokio::test]
    async fn an_unreachable_site_is_an_error_not_a_verdict() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cache = RobotsCache::new("omnid");
        let source = Source::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        assert!(cache.allowed(&gateway(), &source).await.is_err());
        assert!(cache.sites.lock().unwrap().is_empty(), "nothing cached for a site that did not answer");
    }

    #[tokio::test]
    async fn sources_without_robots_txt_are_allowed_without_a_fetch() {
        let cache = RobotsCache::new("omnid");
        let magnet = Source::parse("magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
        assert!(cache.allowed(&gateway(), &magnet).await.unwrap());
    }
}
