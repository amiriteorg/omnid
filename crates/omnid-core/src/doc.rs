use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Network;

/// One crawled thing, harmonised across networks. This is what the index
/// stores and what search results are built from.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedDoc {
    /// Canonical address: the `Display` form of a `Source`.
    pub url: String,
    pub network: Network,
    /// Host, freesite key or `btih:` hash (`Source::site_key`).
    pub site: String,
    pub title: String,
    pub description: String,
    pub keywords: Vec<String>,
    /// Visible text. Indexed for search, not stored.
    pub body: String,
    /// Hash of the normalised content. Mirrors of the same content on
    /// different networks share it and collapse into one search result.
    pub content_hash: String,
    /// BitTorrent v1 info hash, lowercase hex, for magnets.
    pub info_hash: Option<String>,
    pub files: Vec<FileMeta>,
    /// Unix seconds. Preserved across re-crawls by the index.
    pub first_seen: i64,
    /// Unix seconds.
    pub last_seen: i64,
    /// 0..1, how reliably the address answers.
    pub liveness: f32,
    /// 0..1, provenance: operator seeds rank above discovered links.
    pub trust: f32,
    /// Primary language subtag (`en`, `ja`), as the page declares it. `None`
    /// lets the index detect it from the text.
    pub lang: Option<String>,
}

/// A file inside a torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    pub path: String,
    pub size: u64,
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}
