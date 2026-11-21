//! The unified index: one Tantivy index for every network, with the network
//! as a filter facet and the content hash as the key that folds mirrors of
//! the same content into one search result.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use omnid_core::{FileMeta, IndexedDoc, Network};
use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{FAST, Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value};
use tantivy::{DateTime, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term};

pub use tantivy::TantivyError as Error;
pub type Result<T> = tantivy::Result<T>;

/// The on-disk format version. Callers put it in the index directory name,
/// so a schema change opens a fresh index instead of misreading an old one.
pub const SCHEMA_VERSION: u32 = 1;

/// Upper bound on the addresses listed for one piece of content.
const MAX_ROUTES: usize = 16;
const WRITER_HEAP_BYTES: usize = 50_000_000;

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Tantivy query syntax; parsed leniently, so user input never errors.
    /// Empty matches everything.
    pub text: String,
    /// Restrict to these networks. Empty means all.
    pub networks: Vec<Network>,
    pub limit: usize,
}

/// One piece of content and every address it is known at.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub description: String,
    pub content_hash: String,
    pub score: f32,
    /// Best first. Mirrors on other networks are listed even when the query
    /// filtered them out: knowing a clearnet mirror exists is useful.
    pub routes: Vec<Route>,
    pub files: Vec<FileMeta>,
    pub info_hash: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Route {
    pub url: String,
    pub network: Network,
    pub liveness: f32,
    pub trust: f32,
}

#[derive(Clone, Copy)]
struct Fields {
    url: Field,
    network: Field,
    site: Field,
    title: Field,
    description: Field,
    keywords: Field,
    body: Field,
    content_hash: Field,
    info_hash: Field,
    file_names: Field,
    files: Field,
    first_seen: Field,
    last_seen: Field,
    liveness: Field,
    trust: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let fields = Fields {
        url: b.add_text_field("url", STRING | STORED),
        network: b.add_text_field("network", STRING | STORED),
        site: b.add_text_field("site", STRING | STORED),
        title: b.add_text_field("title", TEXT | STORED),
        description: b.add_text_field("description", TEXT | STORED),
        keywords: b.add_text_field("keywords", TEXT | STORED),
        body: b.add_text_field("body", TEXT),
        content_hash: b.add_text_field("content_hash", STRING | STORED),
        info_hash: b.add_text_field("info_hash", STRING | STORED),
        file_names: b.add_text_field("file_names", TEXT),
        files: b.add_text_field("files", STORED),
        first_seen: b.add_date_field("first_seen", STORED | FAST),
        last_seen: b.add_date_field("last_seen", STORED | FAST),
        liveness: b.add_f64_field("liveness", STORED | FAST),
        trust: b.add_f64_field("trust", STORED | FAST),
    };
    (b.build(), fields)
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    /// `None` when opened read-only.
    writer: Option<Mutex<IndexWriter>>,
    fields: Fields,
}

impl SearchIndex {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = schema();
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        Self::with_index(index, fields)
    }

    /// Opens an existing index for searching only. It takes no writer lock,
    /// so it works while a daemon is writing, and it sees each new commit.
    pub fn open_read_only(dir: &Path) -> Result<Self> {
        let (schema, fields) = schema();
        let index = Index::open_in_dir(dir)?;
        if index.schema() != schema {
            return Err(Error::SchemaError(format!("{} has a different schema", dir.display())));
        }
        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        Ok(Self { index, reader, writer: None, fields })
    }

    pub fn in_memory() -> Result<Self> {
        let (schema, fields) = schema();
        Self::with_index(Index::create_in_ram(schema), fields)
    }

    fn with_index(index: Index, fields: Fields) -> Result<Self> {
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        // One indexing thread keeps the heap small enough for phones.
        let writer = index.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
        Ok(Self { index, reader, writer: Some(Mutex::new(writer)), fields })
    }

    /// Inserts or replaces the document at `doc.url`, keeping the original
    /// `first_seen`. Visible to searches after the next [`commit`](Self::commit).
    pub fn upsert(&self, doc: &IndexedDoc) -> Result<()> {
        let f = self.fields;
        let url_term = Term::from_field_text(f.url, &doc.url);
        let first_seen = self.stored_first_seen(&url_term)?.map_or(doc.first_seen, |s| s.min(doc.first_seen));
        // Without a hash a document is only a mirror of itself.
        let content_hash = if doc.content_hash.is_empty() { &doc.url } else { &doc.content_hash };

        let mut d = TantivyDocument::default();
        d.add_text(f.url, &doc.url);
        d.add_text(f.network, doc.network.as_str());
        d.add_text(f.site, &doc.site);
        d.add_text(f.title, &doc.title);
        d.add_text(f.description, &doc.description);
        for keyword in &doc.keywords {
            d.add_text(f.keywords, keyword);
        }
        d.add_text(f.body, &doc.body);
        d.add_text(f.content_hash, content_hash);
        if let Some(info_hash) = &doc.info_hash {
            d.add_text(f.info_hash, info_hash);
        }
        if !doc.files.is_empty() {
            for file in &doc.files {
                d.add_text(f.file_names, &file.path);
            }
            d.add_text(f.files, serde_json::to_string(&doc.files).expect("FileMeta always serializes"));
        }
        d.add_date(f.first_seen, DateTime::from_timestamp_secs(first_seen));
        d.add_date(f.last_seen, DateTime::from_timestamp_secs(doc.last_seen));
        d.add_f64(f.liveness, doc.liveness.into());
        d.add_f64(f.trust, doc.trust.into());

        let writer = self.writer()?.lock().expect("index writer lock poisoned");
        writer.delete_term(url_term);
        writer.add_document(d)?;
        Ok(())
    }

    /// Persists pending upserts and makes them searchable.
    pub fn commit(&self) -> Result<()> {
        self.writer()?.lock().expect("index writer lock poisoned").commit()?;
        self.reader.reload()
    }

    fn writer(&self) -> Result<&Mutex<IndexWriter>> {
        self.writer.as_ref().ok_or_else(|| Error::InvalidArgument("index is open read-only".into()))
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        let f = self.fields;
        let searcher = self.reader.searcher();
        let limit = q.limit.clamp(1, 100);

        let text_query: Box<dyn Query> = if q.text.trim().is_empty() {
            Box::new(AllQuery)
        } else {
            let mut parser =
                QueryParser::for_index(&self.index, vec![f.title, f.keywords, f.description, f.body, f.file_names]);
            parser.set_conjunction_by_default();
            parser.set_field_boost(f.title, 3.0);
            parser.set_field_boost(f.keywords, 2.0);
            parser.set_field_boost(f.description, 1.5);
            parser.parse_query_lenient(&q.text).0
        };
        let query: Box<dyn Query> = if q.networks.is_empty() {
            text_query
        } else {
            let networks = q
                .networks
                .iter()
                .map(|n| -> (Occur, Box<dyn Query>) {
                    (Occur::Should, Box::new(self.term_query(f.network, n.as_str())))
                })
                .collect();
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, text_query),
                (Occur::Must, Box::new(BooleanQuery::new(networks))),
            ]))
        };

        // Over-fetch so that folding mirrors together still fills the page.
        let top = searcher.search(&query, &TopDocs::with_limit(limit * 4).order_by_score())?;
        let mut hits = Vec::new();
        let mut grouped = HashSet::new();
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let content_hash = text(&doc, f.content_hash);
            if !grouped.insert(content_hash.clone()) {
                continue;
            }
            let routes = self.routes(&searcher, &content_hash)?;
            hits.push(SearchHit {
                title: text(&doc, f.title),
                description: text(&doc, f.description),
                score,
                routes,
                files: doc
                    .get_first(f.files)
                    .and_then(|v| v.as_str())
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default(),
                info_hash: doc.get_first(f.info_hash).and_then(|v| v.as_str()).map(str::to_owned),
                first_seen: date(&doc, f.first_seen),
                last_seen: date(&doc, f.last_seen),
                content_hash,
            });
            if hits.len() == limit {
                break;
            }
        }
        Ok(hits)
    }

    /// Every indexed address carrying this content, most reliable first.
    fn routes(&self, searcher: &Searcher, content_hash: &str) -> Result<Vec<Route>> {
        let f = self.fields;
        let query = self.term_query(f.content_hash, content_hash);
        let mut routes = searcher
            .search(&query, &TopDocs::with_limit(MAX_ROUTES).order_by_score())?
            .into_iter()
            .map(|(_, address)| {
                let doc: TantivyDocument = searcher.doc(address)?;
                Ok(Route {
                    url: text(&doc, f.url),
                    network: text(&doc, f.network).parse().unwrap_or(Network::Clearnet),
                    liveness: number(&doc, f.liveness),
                    trust: number(&doc, f.trust),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        routes
            .sort_by(|a, b| (b.liveness * b.trust).total_cmp(&(a.liveness * a.trust)).then_with(|| a.url.cmp(&b.url)));
        Ok(routes)
    }

    fn stored_first_seen(&self, url_term: &Term) -> Result<Option<i64>> {
        let searcher = self.reader.searcher();
        let query = TermQuery::new(url_term.clone(), IndexRecordOption::Basic);
        let Some((_, address)) = searcher.search(&query, &TopDocs::with_limit(1).order_by_score())?.into_iter().next()
        else {
            return Ok(None);
        };
        let doc: TantivyDocument = searcher.doc(address)?;
        Ok(doc.get_first(self.fields.first_seen).and_then(|v| v.as_datetime()).map(DateTime::into_timestamp_secs))
    }

    fn term_query(&self, field: Field, value: &str) -> TermQuery {
        TermQuery::new(Term::from_field_text(field, value), IndexRecordOption::Basic)
    }
}

fn text(doc: &TantivyDocument, field: Field) -> String {
    doc.get_first(field).and_then(|v| v.as_str()).unwrap_or_default().to_owned()
}

fn number(doc: &TantivyDocument, field: Field) -> f32 {
    doc.get_first(field).and_then(|v| v.as_f64()).unwrap_or_default() as f32
}

fn date(doc: &TantivyDocument, field: Field) -> i64 {
    doc.get_first(field).and_then(|v| v.as_datetime()).map(DateTime::into_timestamp_secs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(url: &str, network: Network, title: &str, content_hash: &str) -> IndexedDoc {
        IndexedDoc {
            url: url.into(),
            network,
            site: "site".into(),
            title: title.into(),
            description: format!("about {title}"),
            keywords: vec![],
            body: format!("{title} body text"),
            content_hash: content_hash.into(),
            info_hash: None,
            files: vec![],
            first_seen: 1_000,
            last_seen: 1_000,
            liveness: 1.0,
            trust: 0.5,
        }
    }

    fn query(text: &str, networks: &[Network]) -> SearchQuery {
        SearchQuery { text: text.into(), networks: networks.to_vec(), limit: 10 }
    }

    #[test]
    fn finds_documents_by_text() {
        let index = SearchIndex::in_memory().unwrap();
        index.upsert(&doc("http://a.example/", Network::Clearnet, "Hidden Wiki mirror", "h1")).unwrap();
        index.upsert(&doc("http://b.example/", Network::Clearnet, "Recipes", "h2")).unwrap();
        index.commit().unwrap();

        let hits = index.search(&query("wiki", &[])).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].routes[0].url, "http://a.example/");
    }

    #[test]
    fn filters_by_network() {
        let index = SearchIndex::in_memory().unwrap();
        index.upsert(&doc("http://a.example/", Network::Clearnet, "forum", "h1")).unwrap();
        index.upsert(&doc("http://b.i2p/", Network::I2p, "forum", "h2")).unwrap();
        index.commit().unwrap();

        let hits = index.search(&query("forum", &[Network::I2p])).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].routes[0].network, Network::I2p);
        assert_eq!(index.search(&query("", &[Network::Clearnet, Network::I2p])).unwrap().len(), 2);
        assert!(index.search(&query("forum", &[Network::Onion])).unwrap().is_empty());
    }

    #[test]
    fn folds_mirrors_into_one_hit_with_every_route() {
        let index = SearchIndex::in_memory().unwrap();
        index.upsert(&doc("https://mirror.example/", Network::Clearnet, "Library", "same")).unwrap();
        index.upsert(&doc("http://library.i2p/", Network::I2p, "Library", "same")).unwrap();
        index.upsert(&doc("http://other.example/", Network::Clearnet, "Library annex", "different")).unwrap();
        index.commit().unwrap();

        let hits = index.search(&query("library", &[])).unwrap();
        assert_eq!(hits.len(), 2);
        let mirrored = hits.iter().find(|h| h.content_hash == "same").unwrap();
        let mut networks: Vec<_> = mirrored.routes.iter().map(|r| r.network).collect();
        networks.sort();
        assert_eq!(networks, [Network::Clearnet, Network::I2p]);

        // Filtering to one network still reports the mirror on the other.
        let hits = index.search(&query("library", &[Network::I2p])).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].routes.len(), 2);
    }

    #[test]
    fn upsert_replaces_and_keeps_first_seen() {
        let index = SearchIndex::in_memory().unwrap();
        index.upsert(&doc("http://a.example/", Network::Clearnet, "old title", "h1")).unwrap();
        index.commit().unwrap();

        let mut recrawled = doc("http://a.example/", Network::Clearnet, "new title", "h1");
        recrawled.first_seen = 5_000;
        recrawled.last_seen = 5_000;
        index.upsert(&recrawled).unwrap();
        index.commit().unwrap();

        assert_eq!(index.num_docs(), 1);
        assert!(index.search(&query("old", &[])).unwrap().is_empty());
        let hit = &index.search(&query("new", &[])).unwrap()[0];
        assert_eq!((hit.first_seen, hit.last_seen), (1_000, 5_000));
    }

    #[test]
    fn stores_torrent_files() {
        let index = SearchIndex::in_memory().unwrap();
        let mut magnet = doc("magnet:?xt=urn:btih:00", Network::Magnet, "Debian ISO", "");
        magnet.info_hash = Some("00".into());
        magnet.files = vec![FileMeta { path: "debian-13-amd64-netinst.iso".into(), size: 700 << 20 }];
        index.upsert(&magnet).unwrap();
        index.commit().unwrap();

        let hit = &index.search(&query("netinst", &[])).unwrap()[0];
        assert_eq!(hit.files, magnet.files);
        assert_eq!(hit.info_hash.as_deref(), Some("00"));
        assert_eq!(hit.content_hash, "magnet:?xt=urn:btih:00", "no hash falls back to the url");
    }

    #[test]
    fn malformed_queries_do_not_error() {
        let index = SearchIndex::in_memory().unwrap();
        index.commit().unwrap();
        assert!(index.search(&query("title:( \"unterminated", &[])).is_ok());
    }

    #[test]
    fn reopens_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        {
            let index = SearchIndex::open(dir.path()).unwrap();
            index.upsert(&doc("http://a.example/", Network::Clearnet, "persisted", "h1")).unwrap();
            index.commit().unwrap();
        }
        let index = SearchIndex::open(dir.path()).unwrap();
        assert_eq!(index.search(&query("persisted", &[])).unwrap().len(), 1);
    }

    #[test]
    fn reads_alongside_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let writer = SearchIndex::open(dir.path()).unwrap();
        writer.upsert(&doc("http://a.example/", Network::Clearnet, "shared", "h1")).unwrap();
        writer.commit().unwrap();

        let reader = SearchIndex::open_read_only(dir.path()).unwrap();
        assert_eq!(reader.search(&query("shared", &[])).unwrap().len(), 1);
        assert!(reader.upsert(&doc("http://b.example/", Network::Clearnet, "x", "h2")).is_err());
        assert!(SearchIndex::open_read_only(&dir.path().join("missing")).is_err());
    }
}
