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
