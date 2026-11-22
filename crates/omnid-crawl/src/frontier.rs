//! The crawl frontier: the working set of jobs pulled from the crawl store,
//! in per-network queues with politeness.
//!
//! Each network has a concurrency limit, each site gets one request at a
//! time, and a site cools down for `site_delay` after each request finishes.
//! Deduplication happens in the store, which hands each address out once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use omnid_core::{Network, Source};

/// How many queued jobs `pop_ready` looks at before rotating the queue.
const SCAN_WINDOW: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Politeness {
    /// Fetches in flight at once on this network.
    pub concurrency: usize,
    /// Pause between requests to one site, counted from the end of the last.
    pub site_delay: Duration,
}

impl Politeness {
    pub fn default_for(network: Network) -> Self {
        // Concurrency is across sites; each site still gets one request at a
        // time with `site_delay` between them.
        let (concurrency, delay_secs) = match network {
            Network::Clearnet => (32, 1),
            // Most onion services are one small machine behind a slow
            // circuit, so the per-site delay is long; parallelism across
            // sites is what makes an onion crawl fast.
            Network::Onion => (16, 5),
            Network::I2p => (4, 5),
            // FProxy fetches through the local node; more parallelism only
            // queues up inside it.
            Network::Hyphanet => (2, 2),
            // A gateway finds content through the IPFS DHT, which can take
            // a while; it is the node that does the work, not a site.
            Network::Ipfs => (8, 1),
            // Magnets are indexed from the link itself, without a fetch.
            Network::Magnet => (16, 0),
        };
        Self { concurrency, site_delay: Duration::from_secs(delay_secs) }
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub source: Source,
    /// Sites from a seed: a link to another site is a hop, a link within one
    /// is not.
    pub depth: u32,
    /// Hops within the site from the page the crawl entered it by.
    pub site_depth: u32,
    /// Provenance, copied into the indexed document.
    pub trust: f32,
}

pub(crate) struct Frontier {
    queues: HashMap<Network, VecDeque<Job>>,
    politeness: HashMap<Network, Politeness>,
    inflight: HashMap<Network, usize>,
    busy_sites: HashSet<String>,
    next_allowed: HashMap<String, Instant>,
    /// Round-robin start, so one busy network does not starve the others.
    cursor: usize,
    capacity: usize,
}

impl Frontier {
    pub(crate) fn new(politeness: HashMap<Network, Politeness>, capacity: usize) -> Self {
        Self {
            queues: HashMap::new(),
            politeness,
            inflight: HashMap::new(),
            busy_sites: HashSet::new(),
            next_allowed: HashMap::new(),
            cursor: 0,
            capacity,
        }
    }

    /// Queues a job unless the frontier is full. Returns whether it was
    /// queued.
    pub(crate) fn push(&mut self, job: Job) -> bool {
        if self.queued() >= self.capacity {
            return false;
        }
        self.queues.entry(job.source.network()).or_default().push_back(job);
        true
    }

    /// Queues a job ahead of everything else on its network, even when the
    /// frontier is full: for addresses a person asked for.
    pub(crate) fn push_urgent(&mut self, job: Job) {
        self.queues.entry(job.source.network()).or_default().push_front(job);
    }

    /// The next job whose network has a free slot and whose site is idle and
    /// out of its cool-down.
    pub(crate) fn pop_ready(&mut self, now: Instant) -> Option<Job> {
        let networks = Network::ALL.len();
        for offset in 0..networks {
            let network = Network::ALL[(self.cursor + offset) % networks];
            if self.inflight.get(&network).copied().unwrap_or(0) >= self.politeness(network).concurrency {
                continue;
            }
            let Some(queue) = self.queues.get_mut(&network) else {
                continue;
            };
            let ready = queue.iter().take(SCAN_WINDOW).position(|job| {
                let site = job.source.site_key();
                !self.busy_sites.contains(&site) && self.next_allowed.get(&site).is_none_or(|at| *at <= now)
            });
            let Some(position) = ready else {
                // Everything in the window is waiting on a site; look further
                // down the queue next time.
                let window = queue.len().min(SCAN_WINDOW);
                queue.rotate_left(window);
                continue;
            };
            let job = queue.remove(position).expect("position is within the queue");
            self.busy_sites.insert(job.source.site_key());
            *self.inflight.entry(network).or_default() += 1;
            self.cursor = (self.cursor + offset + 1) % networks;
            return Some(job);
        }
        None
    }

    /// Marks a popped job finished and starts its site's cool-down.
    pub(crate) fn finish(&mut self, source: &Source, now: Instant) {
        let network = source.network();
        if let Some(n) = self.inflight.get_mut(&network) {
            *n = n.saturating_sub(1);
        }
        let site = source.site_key();
        self.busy_sites.remove(&site);
        self.next_allowed.insert(site, now + self.politeness(network).site_delay);
    }

    pub(crate) fn queued(&self) -> usize {
        self.queues.values().map(VecDeque::len).sum()
    }

    pub(crate) fn room(&self) -> usize {
        self.capacity.saturating_sub(self.queued())
    }

    pub(crate) fn inflight(&self) -> usize {
        self.inflight.values().sum()
    }

    fn politeness(&self, network: Network) -> Politeness {
        self.politeness.get(&network).copied().unwrap_or_else(|| Politeness::default_for(network))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(url: &str) -> Job {
        Job { source: Source::parse(url).unwrap(), depth: 0, site_depth: 0, trust: 0.5 }
    }

    fn frontier(concurrency: usize, delay: Duration) -> Frontier {
        let politeness = Network::ALL.iter().map(|n| (*n, Politeness { concurrency, site_delay: delay })).collect();
        Frontier::new(politeness, 100)
    }

    #[test]
    fn one_request_per_site_at_a_time() {
        let mut f = frontier(4, Duration::ZERO);
        f.push(job("http://a.example/1"));
        f.push(job("http://a.example/2"));
        f.push(job("http://b.example/1"));
        let now = Instant::now();

        let first = f.pop_ready(now).unwrap();
        let second = f.pop_ready(now).unwrap();
        assert_ne!(first.source.site_key(), second.source.site_key());
        assert!(f.pop_ready(now).is_none(), "a.example is busy");

        f.finish(&first.source, now);
        f.finish(&second.source, now);
        assert!(f.pop_ready(now).is_some());
    }

    #[test]
    fn sites_cool_down_after_a_request() {
        let delay = Duration::from_secs(5);
        let mut f = frontier(4, delay);
        f.push(job("http://a.example/1"));
        f.push(job("http://a.example/2"));
        let now = Instant::now();

        let first = f.pop_ready(now).unwrap();
        f.finish(&first.source, now);
        assert!(f.pop_ready(now + Duration::from_secs(1)).is_none());
        assert!(f.pop_ready(now + delay).is_some());
    }

    #[test]
    fn caps_concurrency_per_network() {
        let mut f = frontier(1, Duration::ZERO);
        f.push(job("http://a.example/"));
        f.push(job("http://b.example/"));
        f.push(job("http://stats.i2p/"));
        let now = Instant::now();

        let networks: Vec<_> = std::iter::from_fn(|| f.pop_ready(now)).map(|j| j.source.network()).collect();
        assert_eq!(networks.len(), 2, "one clearnet and one i2p slot");
        assert!(networks.contains(&Network::Clearnet) && networks.contains(&Network::I2p));
        assert_eq!(f.inflight(), 2);
    }

    #[test]
    fn looks_past_a_blocked_window() {
        let mut f = frontier(8, Duration::ZERO);
        for i in 0..SCAN_WINDOW + 1 {
            f.push(job(&format!("http://busy.example/{i}")));
        }
        f.push(job("http://other.example/"));
        let now = Instant::now();

        let busy = f.pop_ready(now).unwrap();
        assert_eq!(busy.source.site_key(), "busy.example");
        let next = (0..3).find_map(|_| f.pop_ready(now)).expect("other.example is reachable");
        assert_eq!(next.source.site_key(), "other.example");
    }

    #[test]
    fn respects_capacity() {
        let mut f = Frontier::new(HashMap::new(), 1);
        assert!(f.push(job("http://a.example/")));
        assert!(!f.push(job("http://b.example/")));
    }

    #[test]
    fn urgent_jobs_skip_the_line_and_the_cap() {
        let mut f = Frontier::new(HashMap::new(), 1);
        assert!(f.push(job("http://queued.example/")));
        f.push_urgent(job("http://asked-for.example/"));
        assert_eq!(f.queued(), 2);
        assert_eq!(f.pop_ready(Instant::now()).unwrap().source.site_key(), "asked-for.example");
    }
}
