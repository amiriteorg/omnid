//! The crawler: store → frontier → safety → robots.txt → fetch → extract →
//! safety → index, with discovered links written back to the store.

pub mod extract;
mod forums;
mod frontier;
pub mod nostr;
mod robots;
pub mod safety;
mod store;

/// The robots.txt parser's entry point for the fuzz targets in `fuzz/`, built
/// only by cargo-fuzz (`cfg(fuzzing)`). Not part of the API.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzzing {
    pub use crate::nostr::fuzz::fields as nostr_fields;
    pub use crate::robots::fuzz::allows as robots;
}

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omnid_core::nostr::{Nostr, NostrTarget};
use omnid_core::{IndexedDoc, Magnet, Network, Source, now_secs};
use omnid_dht::{Dht, TorrentMeta};
use omnid_index::SearchIndex;
use omnid_net::{FetchError, FetchOptions, Gateway};
use serde::Serialize;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use frontier::{Job, Politeness};
pub use safety::{Safety, Verdict, is_indexable_type};
pub use store::{CrawlStore, Visit};

/// Trust given to the operator's seed list.
pub const SEED_TRUST: f32 = 0.8;
/// Trust given to links the crawler discovers and to user submissions.
pub const DISCOVERED_TRUST: f32 = 0.5;
/// Trust given to info hashes sampled from the DHT: nobody linked them.
pub const HARVESTED_TRUST: f32 = 0.3;
/// Peers at which a torrent counts as fully alive.
const HEALTHY_SWARM: f32 = 20.0;
/// File names kept in the index per torrent; the description keeps the count.
const MAX_INDEXED_FILES: usize = 1_000;
/// A page with a captcha or bot check in its markup and less visible text
/// than this is the check itself: MyBB's "Human Verification" was 300 bytes,
/// a forum thread is several thousand.
const CHALLENGE_TEXT: usize = 1_500;

#[derive(Debug, Clone)]
pub struct CrawlConfig {
    /// Sites are followed this many hops from a seed: a link to another site
    /// is a hop, links within a site are not.
    pub max_depth: u32,
    /// Links within one site are followed this many hops from the page the
    /// crawl entered it by.
    pub max_site_depth: u32,
    /// Addresses kept per site: past this a site's links are not added, so
    /// one big site cannot fill the crawl. Seeds and submissions are kept
    /// regardless.
    pub max_site_pages: usize,
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
    /// Jobs held in memory at once; the rest wait in the store.
    pub frontier_capacity: usize,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            max_depth: 5,
            max_site_depth: 4,
            max_site_pages: 500,
            max_pages: None,
            politeness: HashMap::new(),
            max_page_bytes: 2 << 20,
            clearnet_same_site_only: true,
            commit_interval: Duration::from_secs(10),
            robots_agent: "omnid".into(),
            frontier_capacity: 20_000,
        }
    }
}

/// Live counters, read through [`CrawlControl::stats`].
#[derive(Debug, Default)]
struct CrawlStats {
    fetched: AtomicU64,
    indexed: AtomicU64,
    skipped: AtomicU64,
    blocked: AtomicU64,
    failed: AtomicU64,
    queued: AtomicU64,
    inflight: AtomicU64,
    known: AtomicU64,
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
    /// Jobs in the in-memory working set.
    pub queued: u64,
    pub inflight: u64,
    /// Addresses in the crawl store.
    pub known: u64,
    pub paused: bool,
}

impl CrawlStats {
    fn snapshot(&self, paused: bool) -> StatsSnapshot {
        let get = |n: &AtomicU64| n.load(Ordering::Relaxed);
        StatsSnapshot {
            fetched: get(&self.fetched),
            indexed: get(&self.indexed),
            skipped: get(&self.skipped),
            blocked: get(&self.blocked),
            failed: get(&self.failed),
            queued: get(&self.queued),
            inflight: get(&self.inflight),
            known: get(&self.known),
            paused,
        }
    }

    fn bump(n: &AtomicU64) {
        n.fetch_add(1, Ordering::Relaxed);
    }
}

/// Start and stop a running crawl from outside it: the API, or the Android
/// shell when the phone leaves its charger. Pausing stops new fetches;
/// fetches in flight finish, and search keeps working.
#[derive(Debug, Default)]
pub struct CrawlControl {
    paused: AtomicBool,
    reason: Mutex<Option<String>>,
    changed: Notify,
    stats: CrawlStats,
}

impl CrawlControl {
    pub fn set_paused(&self, paused: bool) {
        if !paused {
            *self.reason.lock().expect("pause reason lock poisoned") = None;
        }
        if self.paused.swap(paused, Ordering::SeqCst) != paused {
            tracing::info!(paused, "crawl");
            self.changed.notify_one();
        }
    }

    /// Pauses with a reason to show the user, such as "waiting for Wi-Fi".
    pub fn pause_because(&self, reason: &str) {
        *self.reason.lock().expect("pause reason lock poisoned") = Some(reason.to_owned());
        self.set_paused(true);
    }

    pub fn pause_reason(&self) -> Option<String> {
        self.reason.lock().expect("pause reason lock poisoned").clone()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot(self.is_paused())
    }
}

enum Outcome {
    Indexed {
        doc: Box<IndexedDoc>,
        links: Vec<Source>,
    },
    NotIndexed {
        reason: String,
        links: Vec<Source>,
    },
    Redirect(Source),
    Blocked(Verdict),
    /// A fetch that did not get a usable answer; `visit` says how to treat it.
    Failed {
        visit: Visit,
        error: String,
    },
}

pub struct Crawler {
    ctx: Arc<Context>,
}

struct Context {
    gateway: Arc<Gateway>,
    index: Arc<SearchIndex>,
    safety: Arc<Safety>,
    store: Arc<CrawlStore>,
    robots: robots::RobotsCache,
    control: Arc<CrawlControl>,
    dht: Option<Dht>,
    cfg: CrawlConfig,
}

impl Crawler {
    pub fn new(
        gateway: Arc<Gateway>,
        index: Arc<SearchIndex>,
        safety: Arc<Safety>,
        store: Arc<CrawlStore>,
        cfg: CrawlConfig,
    ) -> Self {
        let robots = robots::RobotsCache::new(&cfg.robots_agent);
        let control = Arc::default();
        Self { ctx: Arc::new(Context { gateway, index, safety, store, robots, control, dht: None, cfg }) }
    }

    /// Resolves magnets' metadata through the DHT instead of indexing them
    /// from the link alone.
    pub fn with_dht(mut self, dht: Dht) -> Self {
        Arc::get_mut(&mut self.ctx).expect("the crawler has not started").dht = Some(dht);
        self
    }

    pub fn control(&self) -> Arc<CrawlControl> {
        self.ctx.control.clone()
    }

    /// Crawls until `shutdown` fires, or until `jobs` is closed and nothing
    /// in the store is due, or until `max_pages` jobs have run. Jobs sent on
    /// `jobs` are visited now even if they are known. Commits the index
    /// before returning.
    pub async fn run(self, mut jobs: mpsc::Receiver<Job>, shutdown: CancellationToken) -> StatsSnapshot {
        let ctx = &self.ctx;
        let stats = &ctx.control.stats;
        let politeness = Network::ALL
            .iter()
            .map(|n| (*n, ctx.cfg.politeness.get(n).copied().unwrap_or(Politeness::default_for(*n))));
        let mut frontier = frontier::Frontier::new(politeness.collect(), ctx.cfg.frontier_capacity);
        let mut tasks = JoinSet::new();
        let mut writes = JoinSet::new();
        let mut running = HashMap::new();
        let mut jobs_open = true;
        let mut dispatched = 0u64;
        let mut dirty = false;
        let mut refill = true;
        let mut nothing_due = false;
        let mut next_refill = Instant::now();
        let mut commit = tokio::time::interval(ctx.cfg.commit_interval);
        commit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        stats.known.store(ctx.store.known().unwrap_or_default(), Ordering::Relaxed);

        loop {
            let now = Instant::now();
            // Refill when told to, when running low (at most once a second),
            // and at once when there is nothing at all to do.
            let idle = frontier.queued() == 0 && tasks.is_empty();
            if refill || idle || (frontier.queued() < ctx.cfg.frontier_capacity / 2 && now >= next_refill) {
                match ctx.store.take_due(now_secs(), frontier.room()) {
                    Ok(due) => {
                        nothing_due = due.is_empty();
                        for job in due {
                            frontier.push(job);
                        }
                    }
                    Err(e) => tracing::error!("crawl store: {e}"),
                }
                refill = false;
                next_refill = now + Duration::from_secs(1);
            }
            while !ctx.control.is_paused() && ctx.cfg.max_pages.is_none_or(|max| dispatched < max) {
                let Some(job) = frontier.pop_ready(Instant::now()) else {
                    break;
                };
                dispatched += 1;
                let task_ctx = ctx.clone();
                let task_job = job.clone();
                let handle = tasks.spawn(async move { task_ctx.process(&task_job).await });
                running.insert(handle.id(), job);
            }
            stats.queued.store(frontier.queued() as u64, Ordering::Relaxed);
            stats.inflight.store(frontier.inflight() as u64, Ordering::Relaxed);

            let budget_spent = ctx.cfg.max_pages.is_some_and(|max| dispatched >= max);
            let drained = !jobs_open && frontier.queued() == 0 && nothing_due && !refill;
            if tasks.is_empty() && (budget_spent || drained) {
                break;
            }
            // Waiting on a site's cool-down needs a short nap, and so does
            // work the store may still hold past the last refill; an idle
            // daemon only has to notice revisits coming due.
            let nap = if frontier.queued() > 0 || !nothing_due {
                Duration::from_millis(250).min(next_refill.saturating_duration_since(now)).max(Duration::from_millis(1))
            } else {
                Duration::from_secs(5)
            };

            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(joined) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    let (id, outcome) = match joined {
                        Ok((id, outcome)) => (id, outcome),
                        Err(e) => (e.id(), Outcome::Failed { visit: Visit::Failed, error: format!("crawl task died: {e}") }),
                    };
                    if let Some(job) = running.remove(&id) {
                        frontier.finish(&job.source, Instant::now());
                        let (discovered, changed) = ctx.record(&job, outcome, &mut writes);
                        refill |= discovered;
                        dirty |= changed;
                    }
                }
                Some(_) = writes.join_next(), if !writes.is_empty() => {}
                job = jobs.recv(), if jobs_open => match job {
                    // Seeds and submissions go straight to the front of the
                    // frontier: a full working set must not make someone
                    // wait behind thousands of discovered links.
                    Some(job) => {
                        let now = now_secs();
                        let url = job.source.to_string();
                        let claimed = ctx.store.schedule(std::slice::from_ref(&job), now).and_then(|()| {
                            stats.known.store(ctx.store.known()?, Ordering::Relaxed);
                            ctx.store.claim(&url, now)
                        });
                        match claimed {
                            Ok(true) => frontier.push_urgent(job),
                            Ok(false) => {}
                            Err(e) => tracing::error!("crawl store: {e}"),
                        }
                    }
                    None => jobs_open = false,
                },
                _ = ctx.control.changed.notified() => {}
                _ = tokio::time::sleep(nap) => {}
                _ = commit.tick() => {
                    if dirty {
                        while writes.join_next().await.is_some() {}
                        ctx.commit().await;
                        dirty = false;
                    }
                }
            }
        }

        tasks.shutdown().await;
        while writes.join_next().await.is_some() {}
        stats.queued.store(frontier.queued() as u64, Ordering::Relaxed);
        stats.inflight.store(0, Ordering::Relaxed);
        ctx.commit().await;
        ctx.control.stats()
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
            return self.magnet(magnet, job).await;
        }
        if let Source::Nostr(entity) = source {
            return self.nostr(entity, job).await;
        }
        match self.robots.allowed(&self.gateway, source).await {
            Ok(true) => {}
            Ok(false) => return Outcome::NotIndexed { reason: "disallowed by robots.txt".into(), links: vec![] },
            Err(e) => return failed(&e, "robots.txt: "),
        }

        // Redirects come back unfollowed so the target goes through the
        // store and frontier: deduplicated, rate-limited, safety-checked.
        let opts = FetchOptions { max_bytes: self.cfg.max_page_bytes, accept: is_indexable_type, max_redirects: 0 };
        let page = match self.gateway.fetch(source, &opts).await {
            Ok(page) => page,
            Err(e) => return failed(&e, ""),
        };
        CrawlStats::bump(&self.control.stats.fetched);
        if let Some(target) = page.redirect {
            return Outcome::Redirect(target);
        }
        match page.status {
            200..300 => {}
            404 | 410 => return Outcome::Failed { visit: Visit::Gone, error: format!("HTTP {}", page.status) },
            status => return Outcome::Failed { visit: Visit::Failed, error: format!("HTTP {status}") },
        }
        let Some(body) = page.body else {
            let content_type = page.content_type.as_deref().unwrap_or("none");
            return Outcome::NotIndexed { reason: format!("content type {content_type}"), links: vec![] };
        };

        let page = extract::extract(page.content_type.as_deref(), &body);
        if page.challenge && page.text.len() < CHALLENGE_TEXT {
            // Asked again later, as a failure is: the check may let it through.
            return Outcome::Failed { visit: Visit::Failed, error: "a captcha or bot check".into() };
        }
        let url = source.to_string();
        let verdict = self.safety.check_content(&[&url, &page.title, &page.description, &page.text]);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        let links = if page.nofollow { vec![] } else { self.follow(source, &page.links) };
        if page.noindex {
            return Outcome::NotIndexed { reason: "noindex".into(), links };
        }
        if let Some(original) = page.canonical.as_deref().and_then(|c| copy_of(source, c)) {
            // The original goes through the store like any link, so it is
            // crawled once however many copies point at it.
            let mut links = links;
            links.extend(self.follow(source, &[original.to_string()]));
            return Outcome::NotIndexed { reason: format!("a copy of {original}"), links };
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
            lang: page.lang,
            url,
        };
        Outcome::Indexed { doc: Box::new(doc), links }
    }

    /// A magnet is indexed from its metadata when the DHT finds a peer that
    /// serves it (only the info dictionary is fetched, never content), and
    /// from the link alone otherwise.
    async fn magnet(&self, magnet: &Magnet, job: &Job) -> Outcome {
        let url = job.source.to_string();
        let info_hash = magnet.info_hash_hex();
        let meta = match &self.dht {
            Some(dht) => {
                dht.metadata(magnet.info_hash).await.inspect_err(|e| tracing::debug!(%url, "no metadata: {e}")).ok()
            }
            None => None,
        };
        let title = meta
            .as_ref()
            .map(|m| m.name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| magnet.name.clone())
            .unwrap_or_else(|| info_hash.clone());
        let all_files = meta.as_ref().map(|m| m.files.as_slice()).unwrap_or_default();
        // Every name is checked; only the first ones are stored.
        let file_list = all_files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join("\n");
        let files = all_files.iter().take(MAX_INDEXED_FILES).cloned().collect();
        let link_name = magnet.name.as_deref().unwrap_or_default();
        let verdict = self.safety.check_content(&[&url, &title, link_name, &file_list]);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        let now = now_secs();
        let doc = IndexedDoc {
            network: Network::Magnet,
            site: job.source.site_key(),
            description: meta.as_ref().map(describe).unwrap_or_default(),
            // The link's own name stays searchable when the torrent's differs.
            keywords: Some(link_name.to_owned()).filter(|n| !n.is_empty() && *n != title).into_iter().collect(),
            title,
            body: String::new(),
            content_hash: format!("btih:{info_hash}"),
            info_hash: Some(info_hash),
            files,
            first_seen: now,
            last_seen: now,
            // Unknown without the DHT; otherwise how many peers it found.
            liveness: meta.as_ref().map_or(0.0, |m| (m.peers as f32 / HEALTHY_SWARM).min(1.0)),
            trust: job.trust,
            lang: None,
            url,
        };
        Outcome::Indexed { doc: Box::new(doc), links: vec![] }
    }

    /// A Nostr event, article or profile, from the relays: each event the
    /// gateway hands over is checked, and its author must not be denied.
    async fn nostr(&self, entity: &Nostr, job: &Job) -> Outcome {
        let ask = |filter| async move { self.gateway.nostr(&filter, &entity.relays).await };
        let found = match ask(nostr::filter(&entity.target)).await {
            Ok(events) => events,
            Err(e) => return failed(&e, ""),
        };
        CrawlStats::bump(&self.control.stats.fetched);
        let Some(event) = nostr::newest(found) else {
            return Outcome::Failed { visit: Visit::Failed, error: "no relay asked has it".into() };
        };
        let author = Source::Nostr(Nostr { target: NostrTarget::Profile(event.pubkey), relays: vec![] });
        if self.safety.check_source(&author) != Verdict::Allow {
            return Outcome::Blocked(Verdict::DeniedSite);
        }
        // Whatever fails here leaves the event itself to index.
        let fields = match (&entity.target, event.kind) {
            (NostrTarget::Profile(pubkey), _) => {
                let events = ask(nostr::profile_events(*pubkey)).await.unwrap_or_default();
                nostr::profile(&event, &events)
            }
            (_, nostr::CHANNEL) => {
                let messages = ask(nostr::channel_messages(event.id)).await.unwrap_or_default();
                nostr::event(&event, &messages)
            }
            _ => nostr::event(&event, &[]),
        };
        let url = job.source.to_string();
        let verdict = self.safety.check_content(&[&url, &fields.title, &fields.description, &fields.body]);
        if verdict != Verdict::Allow {
            return Outcome::Blocked(verdict);
        }
        let links = self.follow(&job.source, &fields.links);
        let now = now_secs();
        let doc = IndexedDoc {
            network: Network::Nostr,
            site: author.site_key(),
            title: fields.title,
            description: fields.description,
            keywords: fields.keywords,
            content_hash: content_hash(&fields.body, &event.id),
            body: fields.body,
            info_hash: None,
            files: vec![],
            first_seen: now,
            last_seen: now,
            liveness: 1.0,
            trust: job.trust,
            lang: None,
            url,
        };
        Outcome::Indexed { doc: Box::new(doc), links }
    }

    fn follow(&self, from: &Source, hrefs: &[String]) -> Vec<Source> {
        let mut seen = HashSet::new();
        hrefs
            .iter()
            .filter_map(|href| from.join(href))
            .filter(|link| self.may_follow(from, link))
            .filter(|link| seen.insert(link.to_string()))
            .collect()
    }

    /// The link policy, for links and redirects alike. A page reached
    /// anonymously never leads the crawler to a network it reaches from its
    /// own address ([`Gateway::leaves_anonymity`]).
    fn may_follow(&self, from: &Source, link: &Source) -> bool {
        clearnet_link_ok(from, link, self.cfg.clearnet_same_site_only)
            && !self.gateway.leaves_anonymity(from.network(), link.network())
            && !link.url().is_some_and(forums::is_chaff)
            && !names_a_file(link)
            && self.safety.check_source(link) == Verdict::Allow
    }

    /// Updates the store, the index and the counters for a finished job.
    /// Returns whether new addresses were discovered and whether the index
    /// changed.
    fn record(&self, job: &Job, outcome: Outcome, writes: &mut JoinSet<()>) -> (bool, bool) {
        let stats = &self.control.stats;
        let url = job.source.to_string();
        let network = job.source.network();
        let now = now_secs();
        let finish = |visit| match self.store.finish(&url, network, visit, now) {
            Ok(liveness) => liveness,
            Err(e) => {
                tracing::error!("crawl store: {e}");
                0.0
            }
        };
        // From this page, a link within its site goes a hop deeper there and a
        // link to another site is a hop between sites, starting at that
        // site's top. A redirect is the same page elsewhere, not a hop.
        let discover = |links: Vec<Source>, redirect: bool| {
            let site = job.source.site_key();
            let jobs: Vec<_> = links
                .into_iter()
                .filter_map(|source| {
                    let (depth, site_depth) = match (source.site_key() == site, redirect) {
                        (true, false) => (job.depth, job.site_depth + 1),
                        (true, true) => (job.depth, job.site_depth),
                        (false, false) => (job.depth + 1, 0),
                        (false, true) => (job.depth, 0),
                    };
                    (depth <= self.cfg.max_depth && site_depth <= self.cfg.max_site_depth).then_some(Job {
                        source,
                        depth,
                        site_depth,
                        trust: DISCOVERED_TRUST,
                    })
                })
                .collect();
            if jobs.is_empty() {
                return false;
            }
            match self.store.discover_within(&jobs, now, self.cfg.max_site_pages) {
                Ok(added) => {
                    stats.known.fetch_add(added as u64, Ordering::Relaxed);
                    added > 0
                }
                Err(e) => {
                    tracing::error!("crawl store: {e}");
                    false
                }
            }
        };
        let index = self.index.clone();

        match outcome {
            Outcome::Indexed { mut doc, links } => {
                CrawlStats::bump(&stats.indexed);
                tracing::debug!(%url, links = links.len(), "indexed");
                // Magnets have no liveness signal until the DHT checks them.
                if network != Network::Magnet {
                    doc.liveness = finish(Visit::Answered);
                } else {
                    finish(Visit::Answered);
                }
                spawn_write(writes, move || index.upsert(&doc));
                (discover(links, false), true)
            }
            Outcome::NotIndexed { reason, links } => {
                CrawlStats::bump(&stats.skipped);
                tracing::debug!(%url, %reason, "not indexed");
                finish(Visit::Answered);
                (discover(links, false), false)
            }
            Outcome::Redirect(target) => {
                CrawlStats::bump(&stats.skipped);
                tracing::debug!(%url, to = %target, "redirect");
                finish(Visit::Answered);
                let followed = self.may_follow(&job.source, &target);
                (followed && discover(vec![target], true), false)
            }
            Outcome::Blocked(verdict) => {
                CrawlStats::bump(&stats.blocked);
                tracing::info!(%url, ?verdict, "blocked by the safety gate");
                finish(Visit::Blocked);
                // It may have been indexed before the lists changed.
                spawn_write(writes, move || index.delete(&url));
                (false, true)
            }
            Outcome::Failed { visit, error } => {
                CrawlStats::bump(&stats.failed);
                tracing::debug!(%url, %error, ?visit, "failed");
                let liveness = finish(visit);
                match visit {
                    Visit::Gone => spawn_write(writes, move || index.delete(&url)),
                    Visit::Failed => spawn_write(writes, move || index.set_liveness(&url, liveness, None).map(drop)),
                    _ => return (false, false),
                }
                (false, true)
            }
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

/// Index writes run off the async threads; the crawl loop waits for them
/// before each commit.
fn spawn_write(writes: &mut JoinSet<()>, write: impl FnOnce() -> omnid_index::Result<()> + Send + 'static) {
    writes.spawn_blocking(move || {
        if let Err(e) = write() {
            tracing::error!("index write failed: {e}");
        }
    });
}

/// "11 files · 123.3 MB · 322 peers"
fn describe(meta: &TorrentMeta) -> String {
    let files = match meta.files.len() {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    };
    format!("{files} · {} · {} peers", human_size(meta.total_size), meta.peers)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

/// `www.example.com` and `example.com` are one site.
/// Endings of file names the crawler would not index: archives, programs, disk
/// images, media, documents, fonts, and checksums and signatures (text, but no
/// page).
const FILE_ENDINGS: [&str; 64] = [
    "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "exe", "msi", "dmg", "pkg", "deb", "rpm", "apk", "ipa",
    "iso", "img", "bin", "jar", "whl", "crx", "xpi", "mp3", "mp4", "m4a", "m4v", "mkv", "avi", "mov", "webm", "ogg",
    "ogv", "opus", "flac", "wav", "jpg", "jpeg", "png", "gif", "webp", "avif", "ico", "svg", "pdf", "epub", "doc",
    "docx", "xls", "xlsx", "ppt", "pptx", "odt", "woff", "woff2", "ttf", "otf", "sha256", "sha512", "md5", "asc",
    "sig", "cid",
];

/// Whether a link names, by its file name, a file the crawler would not index.
/// Such a link is not fetched at all: the content type would refuse its body,
/// but asking costs a fetch over Tor or I2P, and makes an IPFS node fetch the
/// file's first blocks. A gateway's `?filename=` is its name too: crawling
/// dist.ipfs.tech had visited 2,507 release files from one seed.
fn names_a_file(link: &Source) -> bool {
    let (path, filename) = match link {
        Source::Clearnet(url) | Source::Onion(url) | Source::I2p(url) | Source::Ipfs(url) => {
            let filename = url.query_pairs().find(|(key, _)| key == "filename").map(|(_, name)| name.into_owned());
            (url.path().to_owned(), filename)
        }
        Source::Hyphanet(key) => (key.clone(), None),
        Source::Nostr(_) | Source::Magnet(_) => return false,
    };
    let name = filename.unwrap_or_else(|| path.rsplit('/').next().unwrap_or_default().to_owned());
    let ending = name.rsplit_once('.').map(|(_, ending)| ending.to_ascii_lowercase());
    ending.is_some_and(|ending| FILE_ENDINGS.contains(&ending.as_str()))
}

/// Whether a link to the clearnet may be followed from `from`, before
/// [`Gateway::leaves_anonymity`] has its say: from a clearnet page, and with
/// `same_site_only` only within its site; and a public Telegram channel's
/// preview from any page, as a channel is its own site wherever it is linked.
/// Telegram's other pages (bots, invites, the app's download page) show a
/// reader without the app nothing, and are not followed at all.
fn clearnet_link_ok(from: &Source, link: &Source, same_site_only: bool) -> bool {
    let host = link.url().and_then(|u| u.host_str()).map(|h| h.trim_start_matches("www."));
    if matches!(host, Some("t.me" | "telegram.me" | "telegram.dog")) {
        return link.telegram_channel().is_some();
    }
    link.network() != Network::Clearnet
        || (from.network() == Network::Clearnet && (!same_site_only || same_site(&from.site_key(), &link.site_key())))
}

/// The page `canonical` names, when `source` is a copy of it on the same site:
/// the same path with another query (`/post/1?scrollToComments=true` of
/// `/post/1`, a phpBB topic with other parameters), or a page below it
/// (`/t/topic/123/4`, a post's place in its topic, of `/t/topic/123`). A
/// canonical address anywhere else, the site's front page above all, is
/// taken for a mistake and left alone.
fn copy_of(source: &Source, canonical: &str) -> Option<Source> {
    // A Telegram channel's preview names the page before its next post, an
    // address that moves with every post: the preview is its own page.
    if source.telegram_channel().is_some() {
        return None;
    }
    let original = source.join(canonical)?;
    let (from, to) = (source.url()?, original.url()?);
    if to == from || to.host_str() != from.host_str() || to.port() != from.port() {
        return None;
    }
    let below = from.path().strip_prefix(to.path().trim_end_matches('/')).is_some_and(|rest| rest.starts_with('/'));
    let same_path = to.path() == from.path();
    (same_path || (below && to.path() != "/")).then_some(original)
}

fn same_site(a: &str, b: &str) -> bool {
    a.strip_prefix("www.").unwrap_or(a) == b.strip_prefix("www.").unwrap_or(b)
}

/// A missing router says nothing about the site, so it only defers.
fn failed(error: &FetchError, context: &str) -> Outcome {
    let visit = match error {
        FetchError::Unavailable { .. } => Visit::Deferred,
        _ => Visit::Failed,
    };
    Outcome::Failed { visit, error: format!("{context}{error}") }
}

/// Hashes the normalised visible text, so mirrors that differ only in markup
/// or link targets still share a hash. Exact match only: near-duplicate
/// detection would need a similarity hash.
fn content_hash(text: &str, raw: &[u8]) -> String {
    let hash = if text.is_empty() { blake3::hash(raw) } else { blake3::hash(text.to_lowercase().as_bytes()) };
    format!("b3:{}", hash.to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_torrents() {
        let meta = TorrentMeta {
            name: "Sintel".into(),
            files: vec![omnid_core::FileMeta { path: "Sintel.mp4".into(), size: 129_241_752 }],
            total_size: 129_302_391,
            piece_length: 131_072,
            private: false,
            peers: 322,
        };
        assert_eq!(describe(&meta), "1 file · 123.3 MB · 322 peers");
        assert_eq!(human_size(512), "512 B");
    }

    #[test]
    fn files_the_crawler_would_not_index_are_not_followed() {
        let file = |s: &str| names_a_file(&Source::parse(s).unwrap());
        for link in [
            "https://example.com/release/omnid-0.1.6.tar.gz",
            "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/img/Logo.PNG",
            "http://stats.i2p/files/i2pinstall.exe",
            "ipfs://bafkreia35gizrkdomv4v4oc6uin6kzjpn5hqlnk7wan3qcuvwrzjx7rubu/?filename=ctl_v1.1.6_linux-amd64.tar.gz.cid",
            "ipns://dist.ipfs.tech/kubo/v0.43.1/kubo_v0.43.1_darwin-arm64.tar.gz.sha512",
            "freenet:CHK@abc,def,AAMC--8/song.ogg",
        ] {
            assert!(file(link), "{link}");
        }
        for page in [
            "https://example.com/",
            "https://example.com/docs/v1.2",
            "https://example.com/index.php?file=x.zip",
            "https://example.com/page.html",
            "https://example.com/notes.txt",
            "ipns://docs.ipfs.tech/concepts/",
            "freenet:USK@abc,def,AQACAAE/site/3/",
            "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x.iso",
        ] {
            assert!(!file(page), "{page}");
        }
    }

    #[test]
    fn telegram_channels_are_followed_from_any_page_other_sites_only_by_choice() {
        let parse = |s: &str| Source::parse(s).unwrap();
        let blog = parse("https://blog.example/post");
        let onion = parse("http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/");
        let channel = parse("https://t.me/torproject");
        let elsewhere = parse("https://other.example/");
        assert!(clearnet_link_ok(&blog, &channel, true));
        assert!(clearnet_link_ok(&onion, &channel, true), "then leaves_anonymity decides");
        assert!(!clearnet_link_ok(&blog, &elsewhere, true));
        assert!(clearnet_link_ok(&blog, &elsewhere, false));
        assert!(!clearnet_link_ok(&onion, &elsewhere, false), "the clearnet only from the clearnet");
        assert!(clearnet_link_ok(&blog, &parse("https://blog.example/next"), true));
        assert!(clearnet_link_ok(&blog, &onion, true));
        // A channel's older posts are the same site as its preview; its bots
        // and Telegram's own pages are not followed.
        assert!(clearnet_link_ok(&channel, &parse("https://t.me/s/torproject?before=41"), true));
        for page in
            ["https://t.me/GetBridgesBot", "https://t.me/", "https://t.me/+invite", "https://telegram.me/addstickers/x"]
        {
            assert!(!clearnet_link_ok(&channel, &parse(page), false), "{page}");
        }
    }

    #[test]
    fn a_copy_names_its_original_on_the_same_site_only() {
        let copy =
            |page: &str, canonical: &str| copy_of(&Source::parse(page).unwrap(), canonical).map(|s| s.to_string());
        // Lemmy, NodeBB, phpBB, Discourse, as they say it.
        assert_eq!(
            copy("https://lemmy.ml/post/5?scrollToComments=true", "https://lemmy.ml/post/5").as_deref(),
            Some("https://lemmy.ml/post/5")
        );
        assert_eq!(
            copy("https://community.nodebb.org/topic/18883/push/4", "https://community.nodebb.org/topic/18883/push")
                .as_deref(),
            Some("https://community.nodebb.org/topic/18883/push")
        );
        assert_eq!(
            copy("http://f.example/viewtopic.php?f=2&t=9", "viewtopic.php?t=9").as_deref(),
            Some("http://f.example/viewtopic.php?t=9")
        );
        assert_eq!(copy("https://d.example/t/x/7/3", "/t/x/7/").as_deref(), Some("https://d.example/t/x/7/"));
        assert_eq!(
            copy("http://d.example/a?x=1", "https://d.example/a").as_deref(),
            Some("https://d.example/a"),
            "to https"
        );
        // Not copies: itself, elsewhere, the front page, a sibling, a page below.
        assert_eq!(copy("https://lemmy.ml/post/5", "https://lemmy.ml/post/5"), None);
        assert_eq!(copy("https://lemmy.ml/post/5", "https://other.example/post/5"), None);
        assert_eq!(copy("https://lemmy.ml/post/5", "/"), None);
        assert_eq!(copy("https://lemmy.ml/post/5", "/post/6"), None);
        assert_eq!(copy("https://lemmy.ml/post", "/post/5"), None);
        assert_eq!(copy("https://lemmy.ml/postal/5", "/post"), None, "a path prefix is not a parent");
        assert_eq!(copy("freenet:USK@a,b,c/s/1/", "/USK@a,b,c/s/2/"), None);
        assert_eq!(copy("https://t.me/s/torproject", "/s/torproject?before=63"), None, "Telegram's moving pointer");
    }

    #[test]
    fn www_is_the_same_site() {
        assert!(same_site("www.example.com", "example.com"));
        assert!(same_site("example.com", "example.com"));
        assert!(!same_site("cdn.example.com", "example.com"));
        assert!(!same_site("example.org", "example.com"));
    }
}
