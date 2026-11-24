//! Addressing and the document model shared by every omnid layer.
//!
//! Everything the gateway touches is a [`Source`]: one canonical address that
//! also says which network carries it. The crawler, the index and the API all
//! speak in `Source`s and [`IndexedDoc`]s, so a new network only has to teach
//! this crate how to parse its addresses and the transport layer how to fetch
//! them.

mod doc;
mod ipfs;
pub mod irc;
pub mod lang;
pub mod nostr;
mod source;

pub use doc::{FileMeta, IndexedDoc, now_secs};
pub use source::{Magnet, Network, Source, SourceError};
