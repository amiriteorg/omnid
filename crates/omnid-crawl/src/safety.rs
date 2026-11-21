//! The gate between the crawler and the index.
//!
//! What it enforces now:
//!
//! * **Media is never downloaded.** Only text content types pass
//!   [`is_indexable_type`], and the transport checks it before reading a body.
//!   Images, video and archives never reach memory, disk or the index.
//! * **Denylisted sites are never fetched**, and links to them are not queued.
//!   Entries are hostnames (blocking a domain blocks its subdomains), Hyphanet
//!   keys, magnet links, or 32-hex-digit MD5 digests of a hostname, the form
//!   published blocklists use so they do not have to print the addresses.
//! * **Pages that match a blocked term are dropped**, and their links are not
//!   followed, so a matching directory page does not lead the crawler deeper.
//! * **A published blocklist** of MD5-hashed hostnames (Ahmia's list of
//!   onion services reported for abuse) can be swapped in while the crawler
//!   runs, as the daemon refreshes it ([`Safety::set_published`]). It is
//!   someone else's judgement and has named sites that are plainly not abuse
//!   (ProPublica, Proton, Qubes and Riseup in November 2026), so it does not
//!   overrule the sites the operator chose, their seeds
//!   ([`Safety::choose_sites`]); the operator's own denylist still does.
//!
//! What it does not do yet: match media against known-abuse hash lists
//! (PhotoDNA, NCMEC, IWF). Those lists are licensed to vetted operators and
//! cannot ship with the code; that check belongs here before any media
//! handling is added.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::RwLock;

use md5::{Digest, Md5};
use omnid_core::Source;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    DeniedSite,
    BlockedTerm,
}

/// The most digests taken from a published blocklist: Ahmia's had 60,401 in
/// November 2026; this leaves it room to grow and bounds what a hostile list
/// can cost.
pub const MAX_PUBLISHED: usize = 1_000_000;

#[derive(Debug, Default)]
pub struct Safety {
    sites: HashSet<String>,
    site_md5s: HashSet<[u8; 16]>,
    /// From a published blocklist, replaced whole as it is refreshed.
    published: RwLock<HashSet<[u8; 16]>>,
    /// Sites the operator chose: the published blocklist does not apply to
    /// them or their subdomains.
    chosen: HashSet<String>,
    /// Lowercase.
    terms: Vec<String>,
}

/// Whether a response with this Content-Type may be downloaded. A missing
/// header is refused: without one the body could be anything.
pub fn is_indexable_type(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return false;
    };
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    matches!(mime.as_str(), "text/html" | "application/xhtml+xml" | "text/plain")
}

impl Safety {
    /// Loads the operator's lists: one entry per line, `#` starts a comment.
    pub fn load(denylist: Option<&Path>, blocked_terms: Option<&Path>) -> io::Result<Self> {
        let read = |path: Option<&Path>| path.map(std::fs::read_to_string).transpose().map(Option::unwrap_or_default);
        Ok(Self::from_lists(&read(denylist)?, &read(blocked_terms)?))
    }

    pub fn from_lists(denylist: &str, blocked_terms: &str) -> Self {
        let mut safety = Safety::default();
        for entry in entries(denylist) {
            if let Some(digest) = md5_hex(entry) {
                safety.site_md5s.insert(digest);
            } else {
                let site = Source::parse(entry).map(|s| s.site_key()).unwrap_or_else(|_| entry.to_owned());
                safety.sites.insert(site.to_ascii_lowercase());
            }
        }
        safety.terms = entries(blocked_terms).map(str::to_lowercase).collect();
        safety
    }

    /// Exempts the sites the operator chose to crawl (as `Source::site_key`)
    /// and their subdomains from the published blocklist.
    pub fn choose_sites<S: AsRef<str>>(&mut self, sites: impl IntoIterator<Item = S>) {
        self.chosen.extend(sites.into_iter().map(|site| site.as_ref().to_ascii_lowercase()));
    }

    /// Whether the operator's own denylist lets `site` (as `Source::site_key`)
    /// through, whatever a published list says.
    pub fn operator_allows(&self, site: &str) -> bool {
        let site = site.to_ascii_lowercase();
        !parent_domains(&site).any(|candidate| {
            self.sites.contains(candidate)
                || self.site_md5s.contains(&<[u8; 16]>::from(Md5::digest(candidate.as_bytes())))
        })
    }

    pub fn denylist_len(&self) -> usize {
        self.sites.len() + self.site_md5s.len() + self.published().len()
    }

    /// Replaces the published blocklist with `list`: one MD5 hex digest of a
    /// hostname per line, other lines skipped, at most [`MAX_PUBLISHED`].
    /// Returns how many digests it holds now.
    pub fn set_published(&self, list: &str) -> usize {
        let digests: HashSet<[u8; 16]> =
            list.lines().filter_map(|line| md5_hex(line.trim())).take(MAX_PUBLISHED).collect();
        let held = digests.len();
        *self.published.write().expect("blocklist lock poisoned") = digests;
        held
    }

    fn published(&self) -> std::sync::RwLockReadGuard<'_, HashSet<[u8; 16]>> {
        self.published.read().expect("blocklist lock poisoned")
    }

    pub fn blocked_terms_len(&self) -> usize {
        self.terms.len()
    }

    /// Checks an address before it is queued or fetched.
    pub fn check_source(&self, source: &Source) -> Verdict {
        let published = self.published();
        if self.sites.is_empty() && self.site_md5s.is_empty() && published.is_empty() {
            return Verdict::Allow;
        }
        let site = source.site_key().to_ascii_lowercase();
        let chosen = parent_domains(&site).any(|candidate| self.chosen.contains(candidate));
        let denied = parent_domains(&site).any(|candidate| {
            let digest = <[u8; 16]>::from(Md5::digest(candidate.as_bytes()));
            self.sites.contains(candidate)
                || self.site_md5s.contains(&digest)
                // A bare top-level name ("onion") is never taken from a
                // published list: one such line would shut a whole network.
                || (!chosen && candidate.contains('.') && published.contains(&digest))
        });
        if denied { Verdict::DeniedSite } else { Verdict::Allow }
    }

    /// Checks extracted content (address, title, text...) before indexing.
    pub fn check_content(&self, parts: &[&str]) -> Verdict {
        if self.terms.is_empty() {
            return Verdict::Allow;
        }
        let hit = parts.iter().any(|part| {
            let part = part.to_lowercase();
            self.terms.iter().any(|term| part.contains(term.as_str()))
        });
        if hit { Verdict::BlockedTerm } else { Verdict::Allow }
    }
}

/// A 32-hex-digit MD5 digest.
fn md5_hex(entry: &str) -> Option<[u8; 16]> {
    if entry.len() != 32 || !entry.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = [0u8; 16];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&entry[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

fn entries(list: &str) -> impl Iterator<Item = &str> {
    list.lines().map(|line| line.split('#').next().unwrap_or_default().trim()).filter(|line| !line.is_empty())
}

/// `a.b.example` → `a.b.example`, `b.example`, `example`.
fn parent_domains(site: &str) -> impl Iterator<Item = &str> {
    std::iter::once(site).chain(site.match_indices('.').map(move |(i, _)| &site[i + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    fn source(s: &str) -> Source {
        Source::parse(s).unwrap()
    }

    #[test]
    fn only_text_types_are_downloaded() {
        assert!(is_indexable_type(Some("text/html; charset=UTF-8")));
        assert!(is_indexable_type(Some("TEXT/PLAIN")));
        assert!(is_indexable_type(Some("application/xhtml+xml")));
        for refused in
            [Some("image/jpeg"), Some("video/mp4"), Some("application/zip"), Some("application/octet-stream"), None]
        {
            assert!(!is_indexable_type(refused), "{refused:?}");
        }
    }

    #[test]
    fn denies_listed_sites_and_their_subdomains() {
        let safety = Safety::from_lists(&format!("# comment\nhttp://{ONION}/some/page\nbad.example # trailing\n"), "");
        assert_eq!(safety.check_source(&source(&format!("http://www.{ONION}/x"))), Verdict::DeniedSite);
        assert_eq!(safety.check_source(&source("https://cdn.bad.example/")), Verdict::DeniedSite);
        assert_eq!(safety.check_source(&source("https://notbad.example/")), Verdict::Allow);
    }

    #[test]
    fn denies_md5_hashed_hosts() {
        let digest: [u8; 16] = Md5::digest(ONION.as_bytes()).into();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let safety = Safety::from_lists(&hex, "");
        assert_eq!(safety.check_source(&source(&format!("http://{ONION}/"))), Verdict::DeniedSite);
        assert_eq!(safety.check_source(&source("http://example.com/")), Verdict::Allow);
    }

    #[test]
    fn takes_a_published_blocklist_while_running() {
        let digest: [u8; 16] = Md5::digest(ONION.as_bytes()).into();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let safety = Safety::default();
        assert_eq!(safety.check_source(&source(&format!("http://{ONION}/"))), Verdict::Allow);
        // Ahmia's form: one digest per line; anything else is skipped.
        let list = format!("{hex}\nnot a digest\n{}\n", "0".repeat(32));
        assert_eq!(safety.set_published(&list), 2);
        assert_eq!(safety.check_source(&source(&format!("http://{ONION}/x"))), Verdict::DeniedSite);
        assert_eq!(safety.denylist_len(), 2);
        // A newer list replaces it.
        assert_eq!(safety.set_published(""), 0);
        assert_eq!(safety.check_source(&source(&format!("http://{ONION}/"))), Verdict::Allow);
    }

    fn md5(text: &str) -> String {
        Md5::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn a_published_list_does_not_overrule_the_operators_choice() {
        const OTHER: &str = "juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion";
        let mut safety = Safety::from_lists("denied.example", "");
        safety.choose_sites([ONION.to_uppercase().as_str(), "denied.example"]);
        safety.set_published(&format!("{}\n{}\n{}\n", md5(ONION), md5(OTHER), md5("onion")));
        assert_eq!(safety.check_source(&source(&format!("http://{ONION}/"))), Verdict::Allow, "a seed");
        assert_eq!(safety.check_source(&source(&format!("http://www.{ONION}/"))), Verdict::Allow, "and its subdomains");
        assert_eq!(safety.check_source(&source(&format!("http://{OTHER}/"))), Verdict::DeniedSite, "anything else");
        assert_eq!(
            safety.check_source(&source("http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/")),
            Verdict::Allow,
            "a listed top-level name does not shut the network"
        );
        assert_eq!(
            safety.check_source(&source("https://denied.example/")),
            Verdict::DeniedSite,
            "the operator's own denylist still applies"
        );
        assert!(safety.operator_allows(ONION) && safety.operator_allows(OTHER));
        assert!(!safety.operator_allows("www.denied.example"));
    }

    #[test]
    fn caps_a_published_blocklist() {
        let list: String = (0..MAX_PUBLISHED + 10).map(|i| format!("{i:032x}\n")).collect();
        assert_eq!(Safety::default().set_published(&list), MAX_PUBLISHED);
    }

    #[test]
    fn denies_magnets_and_freesites() {
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        let safety = Safety::from_lists(&format!("{magnet}\nfreenet:USK@bad,key,AQACAAE/site/1/"), "");
        assert_eq!(safety.check_source(&source(&format!("{magnet}&dn=renamed"))), Verdict::DeniedSite);
        assert_eq!(safety.check_source(&source("freenet:USK@bad,key,AQACAAE/site/7/page")), Verdict::DeniedSite);
    }

    #[test]
    fn blocks_terms_case_insensitively() {
        let safety = Safety::from_lists("", "Forbidden Phrase\n");
        assert_eq!(safety.check_content(&["title", "some FORBIDDEN phrase here"]), Verdict::BlockedTerm);
        assert_eq!(safety.check_content(&["harmless"]), Verdict::Allow);
    }

    #[test]
    fn empty_lists_allow_everything() {
        let safety = Safety::default();
        assert_eq!(safety.check_source(&source("http://example.com/")), Verdict::Allow);
        assert_eq!(safety.check_content(&["anything"]), Verdict::Allow);
    }
}
