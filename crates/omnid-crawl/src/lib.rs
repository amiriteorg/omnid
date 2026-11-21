//! The crawler: frontier → safety → robots.txt → fetch → extract → safety →
//! index, with discovered links fed back into the frontier.

pub mod extract;
mod frontier;
mod robots;
pub mod safety;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use omnid_core::{IndexedDoc, Magnet, Network, Source, now_secs};
use omnid_index::SearchIndex;
use omnid_net::{FetchOptions, Gateway};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use frontier::{Job, Politeness};
pub use safety::{Safety, Verdict, is_indexable_type};

/// Trust given to the operator's seed list.
pub const SEED_TRUST: f32 = 0.8;
/// Trust given to links the crawler discovers and to user submissions.
pub const DISCOVERED_TRUST: f32 = 0.5;

#[derive(Debug, Clone)]
pub struct CrawlConfig {
    /// Links are followed this many hops from a seed.
    pub max_depth: u32,
    /// Stop dispatching after this many jobs. `None` is unbounded.
    pub max_pages: Option<u64>,
    /// Overrides [`Politeness::default_for`] per network.
    pub politeness: HashMap<Network, Politeness>,
    pub max_page_bytes: usize,
    /// Clearnet links are only followed from clearnet pages, and with this
    /// set only within the same site. Overlay and magnet links are always
    /// harvested. This keeps a seed list of directory pages from turning into
    /// a crawl of the whole web.
    pub clearnet_same_site_only: bool,
    pub commit_interval: Duration,
    /// The product token matched against robots.txt `User-agent` lines.
    pub robots_agent: String,
    /// Queued jobs beyond this are dropped.
    pub frontier_capacity: usize,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_pages: None,
            politeness: HashMap::new(),
            max_page_bytes: 2 << 20,
            clearnet_same_site_only: true,
            commit_interval: Duration::from_secs(10),
            robots_agent: "omnid".into(),
            frontier_capacity: 1_000_000,
        }
    }
}

/// Live counters, shared with the API while the crawl runs.
#[derive(Debug, Default)]
pub struct CrawlStats {
    fetched: AtomicU64,
    indexed: AtomicU64,
    skipped: AtomicU64,
    blocked: AtomicU64,
    failed: AtomicU64,
    queued: AtomicU64,
    inflight: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StatsSnapshot {
    /// Responses received, any status.
    pub fetched: u64,
    pub indexed: u64,
    /// Fetched or considered but not indexed: robots.txt, noindex, a
    /// redirect, a refused content type.
    pub skipped: u64,
    /// Refused by the safety gate.
    pub blocked: u64,
    pub failed: u64,
    pub queued: u64,
    pub inflight: u64,
}

impl CrawlStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let get = |n: &AtomicU64| n.load(Ordering::Relaxed);
        StatsSnapshot {
            fetched: get(&self.fetched),
            indexed: get(&self.indexed),
            skipped: get(&self.skipped),
            blocked: get(&self.blocked),
            failed: get(&self.failed),
            queued: get(&self.queued),
            inflight: get(&self.inflight),
        }
    }

    fn bump(n: &AtomicU64) {
        n.fetch_add(1, Ordering::Relaxed);
    }
}

enum Outcome {
    Indexed { links: Vec<Source> },
    NotIndexed { reason: String, links: Vec<Source> },
    Redirect(Source),
    Blocked(Verdict),
    Failed(String),
}

pub struct Crawler {
    ctx: Arc<Context>,
}

struct Context {
    gateway: Arc<Gateway>,
    index: Arc<SearchIndex>,
    safety: Arc<Safety>,
    robots: robots::RobotsCache,
    stats: Arc<CrawlStats>,
    cfg: CrawlConfig,
}

impl Crawler {
    pub fn new(gateway: Arc<Gateway>, index: Arc<SearchIndex>, safety: Arc<Safety>, cfg: CrawlConfig) -> Self {
        let robots = robots::RobotsCache::new(&cfg.robots_agent);
        let stats = Arc::default();
        Self { ctx: Arc::new(Context { gateway, index, safety, robots, stats, cfg }) }
    }

    pub fn stats(&self) -> Arc<CrawlStats> {
        self.ctx.stats.clone()
    }

    /// Crawls until `shutdown` fires, or until `jobs` is closed and every
    /// queued job is done, or until `max_pages` jobs have run. Commits the
    /// index before returning.
    pub async fn run(self, mut jobs: mpsc::Receiver<Job>, shutdown: CancellationToken) -> StatsSnapshot {
        let ctx = &self.ctx;
        let politeness = Network::ALL
            .iter()
            .map(|n| (*n, ctx.cfg.politeness.get(n).copied().unwrap_or(Politeness::default_for(*n))));
        let mut frontier = frontier::Frontier::new(politeness.collect(), ctx.cfg.frontier_capacity);
        let mut tasks = JoinSet::new();
        let mut running = HashMap::new();
        let mut jobs_open = true;
        let mut dispatched = 0u64;
        let mut dirty = false;
        let mut commit = tokio::time::interval(ctx.cfg.commit_interval);
        commit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            while ctx.cfg.max_pages.is_none_or(|max| dispatched < max) {
                let Some(job) = frontier.pop_ready(Instant::now()) else {
                    break;
                };
                dispatched += 1;
                let task_ctx = ctx.clone();
                let task_job = job.clone();
                let handle = tasks.spawn(async move { task_ctx.process(&task_job).await });
                running.insert(handle.id(), job);
            }
            ctx.stats.queued.store(frontier.queued() as u64, Ordering::Relaxed);
            ctx.stats.inflight.store(frontier.inflight() as u64, Ordering::Relaxed);

            let budget_spent = ctx.cfg.max_pages.is_some_and(|max| dispatched >= max);
            if tasks.is_empty() && (budget_spent || (!jobs_open && frontier.queued() == 0)) {
                break;
            }

            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(joined) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    let (id, outcome) = match joined {
                        Ok((id, outcome)) => (id, outcome),
                        Err(e) => (e.id(), Outcome::Failed(format!("crawl task died: {e}"))),
                    };
                    if let Some(job) = running.remove(&id) {
                        frontier.finish(&job.source, Instant::now());
                        dirty |= ctx.record(&mut frontier, &job, outcome);
                    }
                }
                job = jobs.recv(), if jobs_open => match job {
                    Some(job) => { frontier.push(job); }
                    None => jobs_open = false,
                },
                _ = tokio::time::sleep(Duration::from_millis(250)), if frontier.queued() > 0 => {}
                _ = commit.tick() => {
                    if dirty {
                        ctx.commit().await;
                        dirty = false;
                    }
                }
            }
        }

        tasks.shutdown().await;
        ctx.stats.queued.store(frontier.queued() as u64, Ordering::Relaxed);
        ctx.stats.inflight.store(0, Ordering::Relaxed);
        ctx.commit().await;
        ctx.stats.snapshot()
    }
}

impl Context {
    async fn process(&self, job: &Job) -> Outcome {
        let source = &job.source;
        let verdict = self.safety.check_source(source);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        if let Source::Magnet(magnet) = source {
            return self.index_magnet(magnet, job).await;
        }
        match self.robots.allowed(&self.gateway, source).await {
            Ok(true) => {}
            Ok(false) => return Outcome::NotIndexed { reason: "disallowed by robots.txt".into(), links: vec![] },
            Err(e) => return Outcome::Failed(format!("robots.txt: {e}")),
        }

        // Redirects come back unfollowed so the target goes through the
        // frontier: deduplicated, rate-limited and safety-checked.
        let opts = FetchOptions { max_bytes: self.cfg.max_page_bytes, accept: is_indexable_type, max_redirects: 0 };
        let page = match self.gateway.fetch(source, &opts).await {
            Ok(page) => page,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        CrawlStats::bump(&self.stats.fetched);
        if let Some(target) = page.redirect {
            return Outcome::Redirect(target);
        }
        if !(200..300).contains(&page.status) {
            return Outcome::Failed(format!("HTTP {}", page.status));
        }
        let Some(body) = page.body else {
            let content_type = page.content_type.as_deref().unwrap_or("none");
            return Outcome::NotIndexed { reason: format!("content type {content_type}"), links: vec![] };
        };

        let page = extract::extract(page.content_type.as_deref(), &body);
        let url = source.to_string();
        let verdict = self.safety.check_content(&[&url, &page.title, &page.description, &page.text]);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        let links = if page.nofollow { vec![] } else { self.follow(source, &page.links) };
        if page.noindex {
            return Outcome::NotIndexed { reason: "noindex".into(), links };
        }

        let now = now_secs();
        let doc = IndexedDoc {
            network: source.network(),
            site: source.site_key(),
            title: if page.title.is_empty() { url.clone() } else { page.title },
            description: page.description,
            keywords: page.keywords,
            content_hash: content_hash(&page.text, &body),
            body: page.text,
            info_hash: None,
            files: vec![],
            first_seen: now,
            last_seen: now,
            liveness: 1.0,
            trust: job.trust,
            url,
        };
        match self.upsert(doc).await {
            Ok(()) => Outcome::Indexed { links },
            Err(e) => Outcome::Failed(e),
        }
    }

    /// A magnet link is indexed from the link alone. Its file list and
    /// swarm health arrive with the DHT module.
    async fn index_magnet(&self, magnet: &Magnet, job: &Job) -> Outcome {
        let url = job.source.to_string();
        let info_hash = magnet.info_hash_hex();
        let title = magnet.name.clone().unwrap_or_else(|| info_hash.clone());
        let verdict = self.safety.check_content(&[&url, &title]);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        let now = now_secs();
        let doc = IndexedDoc {
            network: Network::Magnet,
            site: job.source.site_key(),
            title,
            description: String::new(),
            keywords: vec![],
            body: String::new(),
            content_hash: format!("btih:{info_hash}"),
            info_hash: Some(info_hash),
            files: vec![],
            first_seen: now,
            last_seen: now,
            // Unknown until something checks the swarm.
            liveness: 0.0,
            trust: job.trust,
            url,
        };
        match self.upsert(doc).await {
            Ok(()) => Outcome::Indexed { links: vec![] },
            Err(e) => Outcome::Failed(e),
        }
    }

    fn follow(&self, from: &Source, hrefs: &[String]) -> Vec<Source> {
        let mut seen = HashSet::new();
        hrefs
            .iter()
            .filter_map(|href| from.join(href))
            .filter(|link| {
                link.network() != Network::Clearnet
                    || (from.network() == Network::Clearnet
                        && (!self.cfg.clearnet_same_site_only || link.site_key() == from.site_key()))
            })
            .filter(|link| self.safety.check_source(link) == Verdict::Allow)
            .filter(|link| seen.insert(link.to_string()))
            .collect()
    }

    /// Updates counters and queues follow-up jobs. Returns whether the index
    /// changed.
    fn record(&self, frontier: &mut frontier::Frontier, job: &Job, outcome: Outcome) -> bool {
        let follow = |frontier: &mut frontier::Frontier, links: Vec<Source>| {
            if job.depth < self.cfg.max_depth {
                for source in links {
                    frontier.push(Job { source, depth: job.depth + 1, trust: DISCOVERED_TRUST });
                }
            }
        };
        match outcome {
            Outcome::Indexed { links } => {
                CrawlStats::bump(&self.stats.indexed);
                tracing::debug!(url = %job.source, links = links.len(), "indexed");
                follow(frontier, links);
                true
            }
            Outcome::NotIndexed { reason, links } => {
                CrawlStats::bump(&self.stats.skipped);
                tracing::debug!(url = %job.source, %reason, "not indexed");
                follow(frontier, links);
                false
            }
            Outcome::Redirect(target) => {
                CrawlStats::bump(&self.stats.skipped);
                tracing::debug!(url = %job.source, to = %target, "redirect");
                frontier.push(Job { source: target, ..job.clone() });
                false
            }
            Outcome::Blocked(verdict) => {
                CrawlStats::bump(&self.stats.blocked);
                tracing::info!(url = %job.source, ?verdict, "blocked by the safety gate");
                false
            }
            Outcome::Failed(error) => {
                CrawlStats::bump(&self.stats.failed);
                tracing::debug!(url = %job.source, %error, "failed");
                false
            }
        }
    }

    async fn upsert(&self, doc: IndexedDoc) -> Result<(), String> {
        let index = self.index.clone();
        match tokio::task::spawn_blocking(move || index.upsert(&doc)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("index: {e}")),
            Err(e) => Err(format!("index task died: {e}")),
        }
    }

    async fn commit(&self) {
        let index = self.index.clone();
        match tokio::task::spawn_blocking(move || index.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("index commit failed: {e}"),
            Err(e) => tracing::error!("index commit task died: {e}"),
        }
    }
}

/// Hashes the normalised visible text, so mirrors that differ only in markup
/// or link targets still share a hash. Exact match only: near-duplicate
/// detection would need a similarity hash.
fn content_hash(text: &str, raw: &[u8]) -> String {
    let hash = if text.is_empty() { blake3::hash(raw) } else { blake3::hash(text.to_lowercase().as_bytes()) };
    format!("b3:{}", hash.to_hex())
}
