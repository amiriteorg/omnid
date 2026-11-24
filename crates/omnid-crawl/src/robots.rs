//! robots.txt, fetched once per site through the same gateway as the page,
//! so an onion's robots.txt is read over Tor. A Gemini capsule's and a
//! Gopher hole's come from the capsule or hole, not from the website on the
//! same host.

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

/// The virtual user agent Gemini's robots.txt convention gives search
/// engines' crawlers, besides `*`.
const GEMINI_INDEXER: &str = "indexer";
/// Gopher's search engine, whose robots.txt groups holes write for indexers.
const GOPHER_INDEXER: &str = "veronica";

enum Rules {
    AllowAll,
    DisallowAll,
    /// Each agent's rules, all of which must allow.
    Parsed(Vec<Robot>),
}

impl Rules {
    /// What a robots.txt that came with a 2xx answer says to `agents`. One
    /// that cannot be read allows everything.
    fn parse(agents: &[&str], body: &[u8]) -> Self {
        match agents.iter().map(|agent| Robot::new(agent, body)).collect::<Result<Vec<_>, _>>() {
            Ok(robots) => Rules::Parsed(robots),
            Err(_) => Rules::AllowAll,
        }
    }

    fn allows(&self, url: &str) -> bool {
        match self {
            Rules::AllowAll => true,
            Rules::DisallowAll => false,
            Rules::Parsed(robots) => robots.iter().all(|robot| robot.allowed(url)),
        }
    }
}

/// What a robots.txt's rules are matched against: a URL's path and query,
/// and a Gopher item's selector, which is what its rules name.
fn rule_path(source: &Source) -> String {
    match source.gopher_item() {
        Some((_, selector)) => {
            let selector = String::from_utf8_lossy(&selector);
            if selector.starts_with('/') { selector.into_owned() } else { format!("/{selector}") }
        }
        None => source.to_string(),
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
        // The website and the capsule on one host are different sites here.
        let site = format!("{}:{}", source.network(), source.site_key());
        let url = rule_path(source);
        // An indexer obeys the groups written for indexers too, and the most
        // restrictive of them wins.
        let agents: &[&str] = match source {
            Source::Gemini(_) => &[&self.agent, GEMINI_INDEXER],
            Source::Gopher(_) => &[&self.agent, GOPHER_INDEXER],
            _ => &[&self.agent],
        };
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
            200..300 => (page.body.as_deref().map_or(Rules::AllowAll, |body| Rules::parse(agents, body)), TTL),
            500.. => (Rules::DisallowAll, SERVER_ERROR_TTL),
            _ => (Rules::AllowAll, TTL),
        };
        let allowed = rules.allows(&url);
        self.sites.lock().expect("robots lock poisoned").insert(site, (Instant::now() + ttl, rules));
        Ok(allowed)
    }
}

/// For the fuzz target in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    /// Whether a robots.txt `body` that came with a 200 lets `agent` fetch
    /// `url`.
    pub fn allows(agent: &str, body: &[u8], url: &str) -> bool {
        super::Rules::parse(&[agent, super::GEMINI_INDEXER], body).allows(url)
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

    #[test]
    fn every_agent_s_rules_must_allow() {
        let body = b"User-agent: *\nDisallow: /private\n\nUser-agent: indexer\nDisallow: /log/\n";
        let rules = Rules::parse(&["omnid", GEMINI_INDEXER], body);
        assert!(!rules.allows("gemini://a.example/log/1.gmi"), "Gemini's virtual agent for search engines");
        assert!(!rules.allows("gemini://a.example/private/x"), "and the * group for ours");
        assert!(rules.allows("gemini://a.example/"));
        assert!(Rules::parse(&["omnid"], body).allows("gemini://a.example/log/1.gmi"));
    }

    #[test]
    fn gopher_rules_name_selectors() {
        let menu = Source::parse("gopher://a.example/1/phlog/").unwrap();
        assert_eq!(rule_path(&menu), "/phlog/");
        assert_eq!(rule_path(&Source::parse("gopher://a.example/0notes.txt").unwrap()), "/notes.txt");
        let rules = Rules::parse(&["omnid"], b"User-agent: *\nDisallow: /phlog\n");
        assert!(!rules.allows(&rule_path(&menu)));
        let for_veronica = Rules::parse(&["omnid", GOPHER_INDEXER], b"User-agent: veronica\nDisallow: /\n");
        assert!(!for_veronica.allows(&rule_path(&menu)), "a hole that keeps search engines out");
        assert!(rules.allows(&rule_path(&Source::parse("gopher://a.example/").unwrap())));
    }

    #[tokio::test]
    async fn sources_without_robots_txt_are_allowed_without_a_fetch() {
        let cache = RobotsCache::new("omnid");
        let magnet = Source::parse("magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
        assert!(cache.allowed(&gateway(), &magnet).await.unwrap());
    }
}
