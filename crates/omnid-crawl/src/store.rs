//! What the crawler knows across restarts: every address it has seen, when
//! to visit it next, and how reliably it answers. The in-memory frontier is
//! only a working set pulled from here, so memory stays flat however many
//! addresses are known.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use omnid_core::{Network, Source};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Job;

const IDLE: i64 = 0;
const QUEUED: i64 = 1;
/// Refused by the safety gate: never visited again.
const BLOCKED: i64 = 2;

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;
const MAX_BACKOFF: i64 = 30 * DAY;
/// When a router is down the site is not at fault; look again soon.
const DEFERRED: i64 = 10 * 60;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pages (
    url        TEXT PRIMARY KEY,
    depth      INTEGER NOT NULL,
    trust      REAL    NOT NULL,
    state      INTEGER NOT NULL DEFAULT 0,
    next_visit INTEGER NOT NULL,
    visits     INTEGER NOT NULL DEFAULT 0,
    failures   INTEGER NOT NULL DEFAULT 0,
    liveness   REAL    NOT NULL DEFAULT 0,
    last_visit INTEGER,
    -- A magnet's display name, which its canonical address leaves out.
    name       TEXT,
    -- Source::site_key: the host, or a Hyphanet key without its path.
    site       TEXT,
    -- Hops within the site from the page the crawl entered it by.
    site_depth INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS pages_due ON pages (state, depth, next_visit);
";

/// How a visit went, for scheduling the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visit {
    /// The site answered: indexed, skipped, redirected, robots.txt said no.
    Answered,
    /// Network error, timeout or server error.
    Failed,
    /// 404 or 410.
    Gone,
    /// The network's router is not running; nothing is known about the site.
    Deferred,
    /// The safety gate refused it. Never visited again.
    Blocked,
}

pub struct CrawlStore {
    db: Mutex<Connection>,
}

impl CrawlStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut db: Connection) -> rusqlite::Result<Self> {
        // WAL lets readers and the single writer proceed together; NORMAL
        // sync is durable across crashes of this process, which is the case
        // that matters.
        db.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.execute_batch(SCHEMA)?;
        add_sites(&mut db)?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS pages_site ON pages (site)")?;
        // Jobs that were queued when the process stopped never finished.
        db.execute("UPDATE pages SET state = ?1 WHERE state = ?2", params![IDLE, QUEUED])?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn known(&self) -> rusqlite::Result<u64> {
        self.db().query_row("SELECT COUNT(*) FROM pages", [], |r| r.get::<_, i64>(0)).map(|n| n as u64)
    }

    /// Adds addresses not seen before, due now. Known ones are left alone.
    /// Returns how many were new.
    pub fn discover(&self, jobs: &[Job], now: i64) -> rusqlite::Result<usize> {
        self.discover_within(jobs, now, usize::MAX)
    }

    /// [`discover`](Self::discover), keeping at most `per_site` addresses
    /// for any one site, so one big site cannot fill the crawl.
    pub fn discover_within(&self, jobs: &[Job], now: i64, per_site: usize) -> rusqlite::Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut added = 0;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO pages (url, depth, site_depth, site, trust, next_visit, name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut count = tx.prepare_cached("SELECT COUNT(*) FROM pages WHERE site = ?1")?;
            let mut known: HashMap<String, usize> = HashMap::new();
            for job in jobs {
                let site = job.source.site_key();
                if per_site != usize::MAX {
                    let n = match known.get(&site) {
                        Some(n) => *n,
                        None => count.query_row([&site], |r| r.get::<_, i64>(0))? as usize,
                    };
                    if n >= per_site {
                        known.insert(site, n);
                        continue;
                    }
                    known.insert(site.clone(), n);
                }
                let new = insert.execute(params![
                    job.source.to_string(),
                    job.depth,
                    job.site_depth,
                    site,
                    job.trust,
                    now,
                    name(job)
                ])?;
                if let Some(n) = known.get_mut(&job.source.site_key()) {
                    *n += new;
                }
                added += new;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Makes addresses due now even if known: seeds and user submissions.
    /// The higher trust and the shallower depth win. Blocked addresses stay
    /// blocked.
    pub fn schedule(&self, jobs: &[Job], now: i64) -> rusqlite::Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO pages (url, depth, site_depth, site, trust, next_visit, name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (url) DO UPDATE SET
                     next_visit = MIN(next_visit, excluded.next_visit),
                     depth = MIN(depth, excluded.depth),
                     site_depth = MIN(site_depth, excluded.site_depth),
                     site = COALESCE(site, excluded.site),
                     trust = MAX(trust, excluded.trust),
                     name = COALESCE(name, excluded.name)
                 WHERE state != 2",
            )?;
            for job in jobs {
                upsert.execute(params![
                    job.source.to_string(),
                    job.depth,
                    job.site_depth,
                    job.source.site_key(),
                    job.trust,
                    now,
                    name(job)
                ])?;
            }
        }
        tx.commit()
    }

    /// Marks one due, idle address queued, for handing straight to the
    /// frontier. Returns false when it is already queued or not due.
    pub fn claim(&self, url: &str, now: i64) -> rusqlite::Result<bool> {
        let claimed = self.db().execute(
            "UPDATE pages SET state = 1 WHERE url = ?1 AND state = 0 AND next_visit <= ?2",
            params![url, now],
        )?;
        Ok(claimed == 1)
    }

    /// Up to `limit` addresses due by `now`, marked queued so they are not
    /// handed out twice. Sites take turns: each site's shallowest page, then
    /// each one's next, so one big site cannot fill the working set.
    pub fn take_due(&self, now: i64, limit: usize) -> rusqlite::Result<Vec<Job>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let mut db = self.db();
        let tx = db.transaction()?;
        type Row = (String, u32, u32, f32, Option<String>);
        let rows: Vec<Row> = {
            let mut due = tx.prepare_cached(
                "SELECT url, depth, site_depth, trust, name FROM (
                     SELECT url, depth, site_depth, trust, name, next_visit,
                            ROW_NUMBER() OVER (PARTITION BY site ORDER BY site_depth, depth, next_visit) AS turn
                     FROM pages WHERE state = 0 AND next_visit <= ?1)
                 ORDER BY turn, depth, site_depth, next_visit LIMIT ?2",
            )?;
            due.query_map(params![now, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut jobs = Vec::with_capacity(rows.len());
        {
            let mut mark = tx.prepare_cached("UPDATE pages SET state = 1 WHERE url = ?1")?;
            let mut forget = tx.prepare_cached("DELETE FROM pages WHERE url = ?1")?;
            for (url, depth, site_depth, trust, name) in rows {
                match Source::parse(&url) {
                    Ok(mut source) => {
                        if let Source::Magnet(magnet) = &mut source {
                            magnet.name = magnet.name.take().or(name);
                        }
                        mark.execute([&url])?;
                        jobs.push(Job { source, depth, site_depth, trust });
                    }
                    // Written by an older, laxer parser.
                    Err(_) => {
                        forget.execute([&url])?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(jobs)
    }

    /// Records a visit and schedules the next one. Returns the address's
    /// liveness: 1 or 0 after the first visit, then a moving average.
    pub fn finish(&self, url: &str, network: Network, visit: Visit, now: i64) -> rusqlite::Result<f32> {
        let db = self.db();
        let (liveness, failures, visits): (f64, i64, i64) = db
            .query_row("SELECT liveness, failures, visits FROM pages WHERE url = ?1", [url], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .unwrap_or((0.0, 0, 0));
        let blend = |answered: bool| {
            let sample = if answered { 1.0 } else { 0.0 };
            if visits == 0 { sample } else { liveness * 0.7 + sample * 0.3 }
        };
        let (state, next_visit, liveness, failures, counted) = match visit {
            Visit::Answered => (IDLE, now + revisit_after(network), blend(true), 0, true),
            Visit::Failed => (IDLE, now + backoff(failures + 1), blend(false), failures + 1, true),
            Visit::Gone => (IDLE, now + MAX_BACKOFF, 0.0, failures + 1, true),
            Visit::Deferred => (IDLE, now + DEFERRED, liveness, failures, false),
            Visit::Blocked => (BLOCKED, i64::MAX, 0.0, failures, true),
        };
        db.execute(
            "UPDATE pages SET state = ?2, next_visit = ?3, liveness = ?4, failures = ?5,
                 visits = visits + ?6, last_visit = ?7
             WHERE url = ?1",
            params![url, state, next_visit, liveness, failures, i64::from(counted), now],
        )?;
        Ok(liveness as f32)
    }

    /// Makes the addresses on `network` that wait out a deferral due now:
    /// they were put off because its router was missing ([`Visit::Deferred`]),
    /// and it is back. A Gemini capsule or Gopher hole on `.i2p` waits for
    /// I2P's router ([`Source::transport`]). Returns how many.
    pub fn retry_deferred(&self, network: Network, now: i64) -> rusqlite::Result<usize> {
        let db = self.db();
        let waiting: Vec<String> = db
            .prepare_cached("SELECT url FROM pages WHERE state = 0 AND next_visit > ?1 AND next_visit <= ?2")?
            .query_map(params![now, now + DEFERRED], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut due = db.prepare_cached("UPDATE pages SET next_visit = ?2 WHERE url = ?1 AND state = 0")?;
        let mut retried = 0;
        for url in waiting.iter().filter(|url| Source::parse(url).is_ok_and(|s| s.transport() == network)) {
            retried += due.execute(params![url, now])?;
        }
        Ok(retried)
    }

    /// The sites (as `Source::site_key`) that have blocked addresses.
    pub fn blocked_sites(&self) -> rusqlite::Result<Vec<String>> {
        let db = self.db();
        let mut sites = db.prepare_cached("SELECT DISTINCT site FROM pages WHERE state = 2 AND site IS NOT NULL")?;
        sites.query_map([], |row| row.get(0))?.collect()
    }

    /// Makes the blocked addresses on `sites` (as `Source::site_key`) and
    /// their subdomains due now: a list blocked them that no longer does, such
    /// as a published blocklist naming a seed the operator chose. Returns how
    /// many.
    pub fn unblock_sites<S: AsRef<str>>(
        &self,
        sites: impl IntoIterator<Item = S>,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let db = self.db();
        let mut unblock = db.prepare_cached(
            "UPDATE pages SET state = 0, next_visit = ?1
             WHERE state = 2 AND (site = ?2 OR (length(site) > length(?2) AND substr(site, -length(?2) - 1) = '.' || ?2))",
        )?;
        let mut unblocked = 0;
        for site in sites {
            unblocked += unblock.execute(params![now, site.as_ref().to_ascii_lowercase()])?;
        }
        Ok(unblocked)
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().expect("crawl store lock poisoned")
    }
}

/// Gives a store from before sites were recorded its `site` and
/// `site_depth` columns, and each row its site.
fn add_sites(db: &mut Connection) -> rusqlite::Result<()> {
    if db.prepare("SELECT 1 FROM pragma_table_info('pages') WHERE name = 'site'")?.exists([])? {
        return Ok(());
    }
    let tx = db.transaction()?;
    tx.execute_batch(
        "ALTER TABLE pages ADD COLUMN site TEXT;
         ALTER TABLE pages ADD COLUMN site_depth INTEGER NOT NULL DEFAULT 0;",
    )?;
    let urls: Vec<String> =
        tx.prepare("SELECT url FROM pages")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    {
        let mut set = tx.prepare("UPDATE pages SET site = ?2 WHERE url = ?1")?;
        for url in &urls {
            if let Ok(source) = Source::parse(url) {
                set.execute(params![url, source.site_key()])?;
            }
        }
    }
    tx.commit()
}

fn name(job: &Job) -> Option<&str> {
    match &job.source {
        Source::Magnet(magnet) => magnet.name.as_deref(),
        _ => None,
    }
}

/// How long a page that answered stays fresh. Onion and I2P sites churn more
/// than the clearnet; Hyphanet content is immutable per edition, and IPFS
/// content per CID (an IPNS name moves, but is checked as seldom). A Nostr
/// event never changes, but a profile's latest notes do. Gemini capsules and
/// Gopher holes change as seldom as the web.
fn revisit_after(network: Network) -> i64 {
    match network {
        Network::Onion | Network::I2p | Network::Magnet | Network::Nostr => 3 * DAY,
        Network::Clearnet | Network::Hyphanet | Network::Ipfs | Network::Gemini | Network::Gopher => 7 * DAY,
    }
}

/// 1h, 2h, 4h... capped at 30 days.
fn backoff(failures: i64) -> i64 {
    let shift = (failures - 1).clamp(0, 20) as u32;
    (HOUR << shift).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(url: &str, depth: u32, trust: f32) -> Job {
        Job { source: Source::parse(url).unwrap(), depth, site_depth: 0, trust }
    }

    fn urls(jobs: &[Job]) -> Vec<String> {
        jobs.iter().map(|j| j.source.to_string()).collect()
    }

    #[test]
    fn keeps_one_site_from_filling_the_store() {
        let store = CrawlStore::in_memory().unwrap();
        let pages: Vec<_> = (0..10).map(|i| job(&format!("http://big.example/{i}"), 1, 0.5)).collect();
        assert_eq!(store.discover_within(&pages, 0, 4).unwrap(), 4, "four pages a site");
        let more = [job("http://big.example/more", 1, 0.5), job("http://small.example/", 1, 0.5)];
        assert_eq!(store.discover_within(&more, 0, 4).unwrap(), 1, "the big site is full, the other is not");
        // Seeds and addresses people ask for are not held to it.
        store.schedule(&[job("http://big.example/asked-for", 0, 0.9)], 0).unwrap();
        assert_eq!(store.known().unwrap(), 6);
    }

    #[test]
    fn takes_turns_between_sites() {
        let store = CrawlStore::in_memory().unwrap();
        let big: Vec<_> = (0..5).map(|i| job(&format!("http://big.example/{i}"), 0, 0.5)).collect();
        store.discover(&big, 0).unwrap();
        store.discover(&[job("http://a.example/", 2, 0.5), job("http://b.example/", 2, 0.5)], 0).unwrap();
        let due = urls(&store.take_due(10, 3).unwrap());
        assert_eq!(due.len(), 3);
        assert_eq!(due.iter().filter(|u| u.contains("big.example")).count(), 1, "one page a site per turn: {due:?}");
    }

    #[test]
    fn records_sites_in_an_older_store() {
        // A store from before sites were recorded gains the columns, and its
        // rows their sites.
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE pages (url TEXT PRIMARY KEY, depth INTEGER NOT NULL, trust REAL NOT NULL,
                 state INTEGER NOT NULL DEFAULT 0, next_visit INTEGER NOT NULL, visits INTEGER NOT NULL DEFAULT 0,
                 failures INTEGER NOT NULL DEFAULT 0, liveness REAL NOT NULL DEFAULT 0, last_visit INTEGER,
                 name TEXT) WITHOUT ROWID;
             INSERT INTO pages (url, depth, trust, next_visit) VALUES ('http://old.example/a', 1, 0.5, 0);",
        )
        .unwrap();
        let store = CrawlStore::init(db).unwrap();
        let site: String = store.db().query_row("SELECT site FROM pages", [], |r| r.get(0)).unwrap();
        assert_eq!(site, "old.example");
        let due = store.take_due(0, 5).unwrap();
        assert_eq!((due.len(), due[0].site_depth), (1, 0));
    }

    #[test]
    fn retries_what_waited_for_a_router() {
        let store = CrawlStore::in_memory().unwrap();
        let waiting = ["http://a.i2p/", "http://b.example/", "freenet:USK@abc,def,AQACAAE/site/1/"];
        let jobs: Vec<_> = waiting.iter().map(|url| job(url, 0, 0.5)).collect();
        store.schedule(&jobs, 0).unwrap();
        for job in &jobs {
            let url = job.source.to_string();
            assert!(store.claim(&url, 0).unwrap());
            store.finish(&url, job.source.network(), Visit::Deferred, 100).unwrap();
        }
        assert!(store.take_due(200, 10).unwrap().is_empty(), "all three wait out the deferral");
        // The I2P router is up: its address is due, the others still wait.
        assert_eq!(store.retry_deferred(Network::I2p, 200).unwrap(), 1);
        assert_eq!(urls(&store.take_due(200, 10).unwrap()), ["http://a.i2p/"]);
        assert_eq!(store.retry_deferred(Network::Hyphanet, 200).unwrap(), 1);
        assert_eq!(urls(&store.take_due(200, 10).unwrap()), ["freenet:USK@abc,def,AQACAAE/site/1/"]);
        assert_eq!(store.retry_deferred(Network::I2p, 200).unwrap(), 0, "nothing twice");
    }

    #[test]
    fn discovers_each_address_once() {
        let store = CrawlStore::in_memory().unwrap();
        assert_eq!(
            store.discover(&[job("http://a.example/", 1, 0.5), job("http://a.example/#x", 1, 0.5)], 0).unwrap(),
            1
        );
        assert_eq!(store.discover(&[job("http://a.example/", 0, 0.9)], 0).unwrap(), 0);
        assert_eq!(store.known().unwrap(), 1);
    }

    #[test]
    fn hands_out_due_jobs_once_shallowest_first() {
        let store = CrawlStore::in_memory().unwrap();
        store.discover(&[job("http://deep.example/", 3, 0.5), job("http://shallow.example/", 1, 0.5)], 0).unwrap();
        store.discover(&[job("http://later.example/", 0, 0.5)], 100).unwrap();

        assert_eq!(urls(&store.take_due(50, 10).unwrap()), ["http://shallow.example/", "http://deep.example/"]);
        assert!(store.take_due(50, 10).unwrap().is_empty(), "queued jobs are not handed out again");
        assert_eq!(urls(&store.take_due(100, 10).unwrap()), ["http://later.example/"]);
    }

    #[test]
    fn schedules_known_addresses_now_and_keeps_the_better_provenance() {
        let store = CrawlStore::in_memory().unwrap();
        store.discover(&[job("http://a.example/", 2, 0.5)], 0).unwrap();
        let taken = store.take_due(0, 10).unwrap();
        store.finish("http://a.example/", Network::Clearnet, Visit::Answered, 0).unwrap();
        assert_eq!(taken.len(), 1);
        assert!(store.take_due(10, 10).unwrap().is_empty(), "fresh for a week");

        store.schedule(&[job("http://a.example/", 0, 0.8)], 10).unwrap();
        let due = store.take_due(10, 10).unwrap();
        assert_eq!((due[0].depth, due[0].trust), (0, 0.8));
    }

    #[test]
    fn tracks_liveness_and_backs_off_failures() {
        let store = CrawlStore::in_memory().unwrap();
        let url = "http://a.example/";
        store.discover(&[job(url, 0, 0.5)], 0).unwrap();
        assert_eq!(store.finish(url, Network::Clearnet, Visit::Answered, 0).unwrap(), 1.0);
        let after_failure = store.finish(url, Network::Clearnet, Visit::Failed, 0).unwrap();
        assert!((after_failure - 0.7).abs() < 1e-6);
        assert!(store.take_due(HOUR - 1, 10).unwrap().is_empty());
        assert_eq!(store.take_due(HOUR, 10).unwrap().len(), 1, "first retry after an hour");

        store.finish(url, Network::Clearnet, Visit::Failed, HOUR).unwrap();
        assert!(store.take_due(HOUR + 2 * HOUR - 1, 10).unwrap().is_empty(), "then two");
        let deferred = store.finish(url, Network::Clearnet, Visit::Deferred, 0).unwrap();
        assert!((deferred - after_failure * 0.7).abs() < 1e-6, "deferring keeps liveness");
        assert_eq!(store.take_due(DEFERRED, 10).unwrap().len(), 1, "a missing router only defers");
        assert_eq!(backoff(40), MAX_BACKOFF);
    }

    #[test]
    fn claims_a_due_address_once() {
        let store = CrawlStore::in_memory().unwrap();
        store.schedule(&[job("http://a.example/", 0, 0.5)], 0).unwrap();
        assert!(store.claim("http://a.example/", 0).unwrap());
        assert!(!store.claim("http://a.example/", 0).unwrap(), "already queued");
        assert!(store.take_due(0, 10).unwrap().is_empty(), "claimed jobs are not handed out again");
        assert!(!store.claim("http://unknown.example/", 0).unwrap());
    }

    #[test]
    fn keeps_magnet_names_through_the_store() {
        let store = CrawlStore::in_memory().unwrap();
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        store.discover(&[job(&format!("{magnet}&dn=Ubuntu+ISO"), 0, 0.5)], 0).unwrap();
        let due = store.take_due(0, 1).unwrap();
        let Source::Magnet(m) = &due[0].source else { panic!("not a magnet") };
        assert_eq!(m.name.as_deref(), Some("Ubuntu ISO"));
    }

    #[test]
    fn blocked_addresses_never_come_back() {
        let store = CrawlStore::in_memory().unwrap();
        let url = "http://bad.example/";
        store.discover(&[job(url, 0, 0.5)], 0).unwrap();
        store.take_due(0, 10).unwrap();
        store.finish(url, Network::Clearnet, Visit::Blocked, 0).unwrap();
        store.schedule(&[job(url, 0, 0.8)], 0).unwrap();
        assert!(store.take_due(i64::MAX - 1, 10).unwrap().is_empty());
    }

    #[test]
    fn a_site_is_unblocked_with_its_subdomains_only() {
        let store = CrawlStore::in_memory().unwrap();
        let urls =
            ["http://seed.example/", "http://doc.seed.example/a", "http://notseed.example/", "http://other.example/"];
        let jobs: Vec<Job> = urls.iter().map(|url| job(url, 0, 0.5)).collect();
        store.discover(&jobs, 0).unwrap();
        store.take_due(0, 10).unwrap();
        for url in urls {
            store.finish(url, Network::Clearnet, Visit::Blocked, 0).unwrap();
        }
        assert_eq!(store.unblock_sites(["Seed.Example"], 5).unwrap(), 2);
        let mut due: Vec<String> = store.take_due(5, 10).unwrap().into_iter().map(|j| j.source.to_string()).collect();
        due.sort();
        assert_eq!(due, ["http://doc.seed.example/a", "http://seed.example/"]);
        assert_eq!(store.unblock_sites(["seed.example"], 6).unwrap(), 0, "nothing left blocked there");
        let mut left = store.blocked_sites().unwrap();
        left.sort();
        assert_eq!(left, ["notseed.example", "other.example"]);
    }

    #[test]
    fn interrupted_jobs_are_due_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crawl.sqlite");
        {
            let store = CrawlStore::open(&path).unwrap();
            store.discover(&[job("http://a.example/", 0, 0.5)], 0).unwrap();
            assert_eq!(store.take_due(0, 10).unwrap().len(), 1);
        }
        let store = CrawlStore::open(&path).unwrap();
        assert_eq!(store.take_due(0, 10).unwrap().len(), 1);
    }
}
