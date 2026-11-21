//! The unified index: one Tantivy index for every network, with the network
//! as a filter facet and the content hash as the key that folds mirrors of
//! the same content into one search result.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use omnid_core::{FileMeta, IndexedDoc, Network};
use serde::Serialize;
use tantivy::collector::{Collector, SegmentCollector, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::snippet::{Snippet, SnippetGenerator};
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer,
};
use tantivy::{
    BitSet, DateTime, DocId, DocSet, Index, IndexReader, IndexWriter, ReloadPolicy, Score, Searcher, SegmentOrdinal,
    SegmentReader, TERMINATED, TantivyDocument, Term,
};

pub use tantivy::TantivyError as Error;
pub type Result<T> = tantivy::Result<T>;

/// The on-disk format version. Callers put it in the index directory name,
/// so a schema change opens a fresh index instead of misreading an old one.
pub const SCHEMA_VERSION: u32 = 2;

/// Upper bound on the addresses listed for one piece of content.
const MAX_ROUTES: usize = 16;
const WRITER_HEAP_BYTES: usize = 50_000_000;
const SNIPPET_CHARS: usize = 220;
/// Lowercase, fold accents ("café" finds "cafe") and stem English
/// ("bananas" finds "banana").
const TOKENIZER: &str = "omnid";

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Tantivy query syntax: `"exact phrase"`, `-exclude`, `title:word`,
    /// `site:host`, `network:onion`. Parsed leniently, so user input never
    /// errors. Empty matches everything.
    pub text: String,
    /// Restrict to these networks. Empty means all.
    pub networks: Vec<Network>,
    /// Results to skip, counted in folded results, for paging.
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    /// Documents matching the query and the network filter. Mirrors count
    /// once per address.
    pub total: usize,
    /// Documents matching the query on each network, ignoring the network
    /// filter, for facet counts.
    pub network_counts: BTreeMap<Network, usize>,
    /// Every term was required and nothing matched, so the query was re-run
    /// with any term matching.
    pub relaxed: bool,
}

/// One piece of content and every address it is known at.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub title: String,
    /// The title with query terms marked.
    pub title_spans: Vec<Span>,
    pub description: String,
    /// The best passage of the page for the query, terms marked; the
    /// description when no passage matches.
    pub snippet: Vec<Span>,
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

/// A run of text, marked when it matched the query. Structured rather than
/// HTML so the page can render crawled text without trusting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub text: String,
    pub mark: bool,
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
    let text = |stored: bool| {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let options = TextOptions::default().set_indexing_options(indexing);
        if stored { options.set_stored() } else { options }
    };
    let mut b = Schema::builder();
    let fields = Fields {
        url: b.add_text_field("url", STRING | STORED),
        network: b.add_text_field("network", STRING | STORED),
        site: b.add_text_field("site", STRING | STORED),
        title: b.add_text_field("title", text(true)),
        description: b.add_text_field("description", text(true)),
        keywords: b.add_text_field("keywords", text(true)),
        // Stored for snippets and so documents can be rewritten without a
        // refetch. The doc store is compressed.
        body: b.add_text_field("body", text(true)),
        content_hash: b.add_text_field("content_hash", STRING | STORED),
        info_hash: b.add_text_field("info_hash", STRING | STORED),
        file_names: b.add_text_field("file_names", text(false)),
        files: b.add_text_field("files", STORED),
        first_seen: b.add_date_field("first_seen", STORED | FAST),
        last_seen: b.add_date_field("last_seen", STORED | FAST),
        liveness: b.add_f64_field("liveness", STORED | FAST),
        trust: b.add_f64_field("trust", STORED | FAST),
    };
    (b.build(), fields)
}

fn register_tokenizer(index: &Index) {
    let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .filter(Stemmer::new(Language::English))
        .build();
    index.tokenizers().register(TOKENIZER, analyzer);
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
        register_tokenizer(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        Ok(Self { index, reader, writer: None, fields })
    }

    pub fn in_memory() -> Result<Self> {
        let (schema, fields) = schema();
        Self::with_index(Index::create_in_ram(schema), fields)
    }

    fn with_index(index: Index, fields: Fields) -> Result<Self> {
        register_tokenizer(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        // One indexing thread keeps the heap small enough for phones.
        let writer = index.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
        Ok(Self { index, reader, writer: Some(Mutex::new(writer)), fields })
    }

    /// Inserts or replaces the document at `doc.url`, keeping the original
    /// `first_seen`. Visible to searches after the next [`commit`](Self::commit).
    pub fn upsert(&self, doc: &IndexedDoc) -> Result<()> {
        let f = self.fields;
        let first_seen = self.get(&doc.url)?.map_or(doc.first_seen, |stored| stored.first_seen.min(doc.first_seen));
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
        writer.delete_term(Term::from_field_text(f.url, &doc.url));
        writer.add_document(d)?;
        Ok(())
    }

    /// The committed document at `url`.
    pub fn get(&self, url: &str) -> Result<Option<IndexedDoc>> {
        let searcher = self.reader.searcher();
        let query = self.term_query(self.fields.url, url);
        let Some((_, address)) = searcher.search(&query, &TopDocs::with_limit(1).order_by_score())?.into_iter().next()
        else {
            return Ok(None);
        };
        let doc: TantivyDocument = searcher.doc(address)?;
        Ok(Some(self.indexed_doc(&doc)))
    }

    /// Records a re-crawl's verdict on an address without refetching its
    /// content. Returns whether the document exists.
    pub fn set_liveness(&self, url: &str, liveness: f32, last_seen: Option<i64>) -> Result<bool> {
        let Some(mut doc) = self.get(url)? else {
            return Ok(false);
        };
        doc.liveness = liveness;
        if let Some(last_seen) = last_seen {
            doc.last_seen = last_seen;
        }
        self.upsert(&doc)?;
        Ok(true)
    }

    pub fn delete(&self, url: &str) -> Result<()> {
        let writer = self.writer()?.lock().expect("index writer lock poisoned");
        writer.delete_term(Term::from_field_text(self.fields.url, url));
        Ok(())
    }

    /// Persists pending changes and makes them searchable.
    pub fn commit(&self) -> Result<()> {
        self.writer()?.lock().expect("index writer lock poisoned").commit()?;
        self.reader.reload()
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn search(&self, q: &SearchQuery) -> Result<SearchResults> {
        let searcher = self.reader.searcher();
        let mut results = self.search_with(&searcher, q, true)?;
        // AND first for precision; if that finds nothing, any term will do.
        if results.total == 0 && q.text.split_whitespace().nth(1).is_some() {
            results = self.search_with(&searcher, q, false)?;
            results.relaxed = true;
        }
        Ok(results)
    }

    fn search_with(&self, searcher: &Searcher, q: &SearchQuery, conjunction: bool) -> Result<SearchResults> {
        let f = self.fields;
        let limit = q.limit.clamp(1, 100);
        let text_query = self.parse(&q.text, conjunction);
        // Over-fetch so that folding mirrors together still fills the page.
        let window = (q.offset + limit) * 4;
        let top_docs = TopDocs::with_limit(window).tweak_score(rank);

        // The text query is the expensive part (phrases check positions in
        // every candidate), so it runs once: the same pass yields the top
        // documents and every match, and per-network counts come from
        // walking each network's postings against the matches.
        let (top, network_counts) = if q.text.trim().is_empty() {
            // Matching everything: the network postings alone give the counts.
            let counts = self.network_counts(searcher, None)?;
            let top = searcher.search(&*self.with_networks(text_query.box_clone(), &q.networks), &top_docs)?;
            (top, counts)
        } else if q.networks.is_empty() {
            let (top, matched) = searcher.search(&*text_query, &(top_docs, Matched))?;
            (top, self.network_counts(searcher, Some(&matched))?)
        } else {
            let matched = searcher.search(&*text_query, &Matched)?;
            let filtered = self.with_networks(text_query.box_clone(), &q.networks);
            (searcher.search(&*filtered, &top_docs)?, self.network_counts(searcher, Some(&matched))?)
        };
        let total = if q.networks.is_empty() {
            network_counts.values().sum()
        } else {
            q.networks.iter().filter_map(|n| network_counts.get(n)).sum()
        };

        let mut snippets = SnippetGenerator::create(searcher, &*text_query, f.body)?;
        snippets.set_max_num_chars(SNIPPET_CHARS);
        let mut titles = SnippetGenerator::create(searcher, &*text_query, f.title)?;
        titles.set_max_num_chars(1_000);

        let mut hits = Vec::new();
        let mut grouped = HashSet::new();
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let content_hash = text(&doc, f.content_hash);
            if !grouped.insert(content_hash.clone()) || grouped.len() <= q.offset {
                continue;
            }
            let title = text(&doc, f.title);
            let description = text(&doc, f.description);
            hits.push(SearchHit {
                routes: self.routes(searcher, &content_hash)?,
                files: files(&doc, f.files),
                info_hash: doc.get_first(f.info_hash).and_then(|v| v.as_str()).map(str::to_owned),
                first_seen: date(&doc, f.first_seen),
                last_seen: date(&doc, f.last_seen),
                title_spans: spans(&titles.snippet(&title), &title),
                snippet: spans(&snippets.snippet(&text(&doc, f.body)), &description),
                title,
                description,
                score,
                content_hash,
            });
            if hits.len() == limit {
                break;
            }
        }
        Ok(SearchResults { hits, total, network_counts, relaxed: false })
    }

    fn parse(&self, text: &str, conjunction: bool) -> Box<dyn Query> {
        if text.trim().is_empty() {
            return Box::new(AllQuery);
        }
        let f = self.fields;
        let mut parser =
            QueryParser::for_index(&self.index, vec![f.title, f.keywords, f.description, f.body, f.file_names]);
        if conjunction {
            parser.set_conjunction_by_default();
        }
        parser.set_field_boost(f.title, 3.0);
        parser.set_field_boost(f.keywords, 2.0);
        parser.set_field_boost(f.description, 1.5);
        parser.set_field_boost(f.file_names, 1.5);
        parser.parse_query_lenient(text).0
    }

    /// Documents on each network, among `matched` (one bitset per segment)
    /// or among all live documents.
    fn network_counts(&self, searcher: &Searcher, matched: Option<&[BitSet]>) -> Result<BTreeMap<Network, usize>> {
        let mut counts = BTreeMap::new();
        for network in Network::ALL {
            let term = Term::from_field_text(self.fields.network, network.as_str());
            let mut count = 0;
            for (ord, segment) in searcher.segment_readers().iter().enumerate() {
                let Some(mut postings) =
                    segment.inverted_index(self.fields.network)?.read_postings(&term, IndexRecordOption::Basic)?
                else {
                    continue;
                };
                let mut doc = postings.doc();
                while doc != TERMINATED {
                    let counted = match matched {
                        Some(matched) => matched[ord].contains(doc),
                        None => !segment.is_deleted(doc),
                    };
                    count += usize::from(counted);
                    doc = postings.advance();
                }
            }
            counts.insert(network, count);
        }
        Ok(counts)
    }

    fn with_networks(&self, query: Box<dyn Query>, networks: &[Network]) -> Box<dyn Query> {
        if networks.is_empty() {
            return query;
        }
        let networks = networks
            .iter()
            .map(|n| -> (Occur, Box<dyn Query>) {
                (Occur::Should, Box::new(self.term_query(self.fields.network, n.as_str())))
            })
            .collect();
        Box::new(BooleanQuery::new(vec![(Occur::Must, query), (Occur::Must, Box::new(BooleanQuery::new(networks)))]))
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

    fn indexed_doc(&self, doc: &TantivyDocument) -> IndexedDoc {
        let f = self.fields;
        IndexedDoc {
            url: text(doc, f.url),
            network: text(doc, f.network).parse().unwrap_or(Network::Clearnet),
            site: text(doc, f.site),
            title: text(doc, f.title),
            description: text(doc, f.description),
            keywords: doc.get_all(f.keywords).filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            body: text(doc, f.body),
            content_hash: text(doc, f.content_hash),
            info_hash: doc.get_first(f.info_hash).and_then(|v| v.as_str()).map(str::to_owned),
            files: files(doc, f.files),
            first_seen: date(doc, f.first_seen),
            last_seen: date(doc, f.last_seen),
            liveness: number(doc, f.liveness),
            trust: number(doc, f.trust),
        }
    }

    fn writer(&self) -> Result<&Mutex<IndexWriter>> {
        self.writer.as_ref().ok_or_else(|| Error::InvalidArgument("index is open read-only".into()))
    }

    fn term_query(&self, field: Field, value: &str) -> TermQuery {
        TermQuery::new(Term::from_field_text(field, value), IndexRecordOption::Basic)
    }
}

/// Collects every matching live document, one bitset per segment.
struct Matched;

struct MatchedSegment(BitSet);

impl Collector for Matched {
    type Fruit = Vec<BitSet>;
    type Child = MatchedSegment;

    fn for_segment(&self, _: SegmentOrdinal, segment: &SegmentReader) -> Result<MatchedSegment> {
        Ok(MatchedSegment(BitSet::with_max_value(segment.max_doc())))
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, segments: Vec<BitSet>) -> Result<Vec<BitSet>> {
        Ok(segments)
    }
}

impl SegmentCollector for MatchedSegment {
    type Fruit = BitSet;

    fn collect(&mut self, doc: DocId, _: Score) {
        self.0.insert(doc);
    }

    fn harvest(self) -> BitSet {
        self.0
    }
}

/// Relevance times provenance times reliability: an operator's seed that
/// answers every time outranks a stray link that is usually down. The
/// factors stay within 0.3..1.5, so text relevance still dominates.
fn rank(segment: &SegmentReader) -> impl Fn(DocId, Score) -> Score + use<> {
    let column =
        |name: &str, default: f64| segment.fast_fields().f64(name).ok().map(|c| c.first_or_default_col(default));
    let (trust, liveness) = (column("trust", 0.5), column("liveness", 0.0));
    move |doc, score| {
        let trust = trust.as_ref().map_or(0.5, |c| c.get_val(doc));
        let liveness = liveness.as_ref().map_or(0.0, |c| c.get_val(doc));
        score * ((0.5 + trust) * (0.6 + 0.4 * liveness)) as Score
    }
}

/// Splits a snippet into marked and unmarked runs; `fallback` unmarked when
/// nothing in it matched.
fn spans(snippet: &Snippet, fallback: &str) -> Vec<Span> {
    if snippet.highlighted().is_empty() {
        return vec![Span { text: fallback.to_owned(), mark: false }];
    }
    let fragment = snippet.fragment();
    let mut spans = Vec::new();
    let mut at = 0;
    for range in snippet.highlighted() {
        if range.start > at {
            spans.push(Span { text: fragment[at..range.start].to_owned(), mark: false });
        }
        spans.push(Span { text: fragment[range.clone()].to_owned(), mark: true });
        at = range.end;
    }
    if at < fragment.len() {
        spans.push(Span { text: fragment[at..].to_owned(), mark: false });
    }
    spans
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

fn files(doc: &TantivyDocument, field: Field) -> Vec<FileMeta> {
    doc.get_first(field).and_then(|v| v.as_str()).and_then(|json| serde_json::from_str(json).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(url: &str, network: Network, title: &str, content_hash: &str) -> IndexedDoc {
        IndexedDoc {
            url: url.into(),
            network,
            site: url.split('/').nth(2).unwrap_or_default().into(),
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
        SearchQuery { text: text.into(), networks: networks.to_vec(), offset: 0, limit: 10 }
    }

    fn index_of(docs: &[IndexedDoc]) -> SearchIndex {
        let index = SearchIndex::in_memory().unwrap();
        for d in docs {
            index.upsert(d).unwrap();
        }
        index.commit().unwrap();
        index
    }

    fn hits(index: &SearchIndex, text: &str) -> Vec<SearchHit> {
        index.search(&query(text, &[])).unwrap().hits
    }

    #[test]
    fn finds_documents_by_text() {
        let index = index_of(&[
            doc("http://a.example/", Network::Clearnet, "Hidden Wiki mirror", "h1"),
            doc("http://b.example/", Network::Clearnet, "Recipes", "h2"),
        ]);
        let hits = hits(&index, "wiki");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].routes[0].url, "http://a.example/");
    }

    #[test]
    fn folds_accents_and_stems() {
        let mut page = doc("http://a.example/", Network::Clearnet, "Café", "h1");
        page.body = "We sell bananas and run forums".into();
        let index = index_of(&[page]);
        for q in ["cafe", "CAFÉ", "banana", "forum", "bananas"] {
            assert_eq!(hits(&index, q).len(), 1, "{q}");
        }
    }

    #[test]
    fn requires_every_term_then_relaxes() {
        let index = index_of(&[
            doc("http://a.example/", Network::Clearnet, "apples", "h1"),
            doc("http://b.example/", Network::Clearnet, "apples pears", "h2"),
        ]);
        let both = index.search(&query("apples pears", &[])).unwrap();
        assert_eq!((both.hits.len(), both.relaxed), (1, false));
        let relaxed = index.search(&query("apples kiwis", &[])).unwrap();
        assert!(relaxed.relaxed);
        assert_eq!(relaxed.hits.len(), 2);
        assert_eq!(hits(&index, "\"pears apples\"").len(), 0, "phrases keep word order");
    }

    #[test]
    fn filters_and_counts_by_network() {
        let index = index_of(&[
            doc("http://a.example/", Network::Clearnet, "forum", "h1"),
            doc("http://b.i2p/", Network::I2p, "forum", "h2"),
            doc("http://c.i2p/", Network::I2p, "forum", "h3"),
        ]);
        let results = index.search(&query("forum", &[Network::I2p])).unwrap();
        assert_eq!(results.hits.len(), 2);
        assert_eq!(results.total, 2);
        assert_eq!(results.network_counts[&Network::Clearnet], 1, "counts ignore the filter");
        assert_eq!(results.network_counts[&Network::I2p], 2);
        assert_eq!(index.search(&query("network:i2p", &[])).unwrap().total, 2, "query syntax works too");
        assert!(index.search(&query("forum", &[Network::Onion])).unwrap().hits.is_empty());
    }

    #[test]
    fn counts_ignore_replaced_and_deleted_documents() {
        let index = index_of(&[
            doc("http://a.example/", Network::Clearnet, "forum", "h1"),
            doc("http://b.i2p/", Network::I2p, "forum", "h2"),
            doc("http://gone.i2p/", Network::I2p, "forum", "h3"),
        ]);
        // Replaced and deleted documents stay in the postings until a merge.
        index.upsert(&doc("http://a.example/", Network::Clearnet, "forum again", "h1")).unwrap();
        index.delete("http://gone.i2p/").unwrap();
        index.commit().unwrap();

        for text in ["forum", ""] {
            let results = index.search(&query(text, &[])).unwrap();
            assert_eq!(results.network_counts[&Network::Clearnet], 1, "{text:?}");
            assert_eq!(results.network_counts[&Network::I2p], 1, "{text:?}");
            assert_eq!(results.total, 2, "{text:?}");
            let filtered = index.search(&query(text, &[Network::I2p])).unwrap();
            assert_eq!((filtered.total, filtered.hits.len()), (1, 1), "{text:?}");
        }
    }

    #[test]
    fn folds_mirrors_into_one_hit_with_every_route() {
        let index = index_of(&[
            doc("https://mirror.example/", Network::Clearnet, "Library", "same"),
            doc("http://library.i2p/", Network::I2p, "Library", "same"),
            doc("http://other.example/", Network::Clearnet, "Library annex", "different"),
        ]);
        let hits = hits(&index, "library");
        assert_eq!(hits.len(), 2);
        let mirrored = hits.iter().find(|h| h.content_hash == "same").unwrap();
        let mut networks: Vec<_> = mirrored.routes.iter().map(|r| r.network).collect();
        networks.sort();
        assert_eq!(networks, [Network::Clearnet, Network::I2p]);

        let hits = index.search(&query("library", &[Network::I2p])).unwrap().hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].routes.len(), 2, "the filtered-out mirror is still listed");
    }

    #[test]
    fn ranks_by_trust_and_liveness_among_equals() {
        let mut seed = doc("http://seed.example/", Network::Clearnet, "market", "h1");
        seed.trust = 0.8;
        let mut dead = doc("http://dead.example/", Network::Clearnet, "market", "h2");
        dead.liveness = 0.0;
        let index = index_of(&[dead, doc("http://found.example/", Network::Clearnet, "market", "h3"), seed]);
        let order: Vec<_> = hits(&index, "market").iter().map(|h| h.routes[0].url.clone()).collect();
        assert_eq!(order, ["http://seed.example/", "http://found.example/", "http://dead.example/"]);
    }

    #[test]
    fn marks_query_terms_in_snippets_and_titles() {
        let mut page = doc("http://a.example/", Network::Clearnet, "Onion directory", "h1");
        page.body = "A long page. The onion services listed here were checked today.".into();
        let index = index_of(&[page]);
        let hit = &hits(&index, "onion")[0];
        assert!(hit.snippet.iter().any(|s| s.mark && s.text.eq_ignore_ascii_case("onion")), "{:?}", hit.snippet);
        assert_eq!(hit.title_spans[0], Span { text: "Onion".into(), mark: true });
        let unmatched = &hits(&index, "")[0];
        assert_eq!(unmatched.snippet, [Span { text: "about Onion directory".into(), mark: false }]);
    }

    #[test]
    fn pages_through_folded_results() {
        let docs: Vec<_> =
            (0..5).map(|i| doc(&format!("http://{i}.example/"), Network::Clearnet, "page", &format!("h{i}"))).collect();
        let index = index_of(&docs);
        let page = |offset| {
            let q = SearchQuery { offset, limit: 2, ..query("page", &[]) };
            index.search(&q).unwrap().hits.into_iter().map(|h| h.content_hash).collect::<Vec<_>>()
        };
        let all: Vec<_> = [page(0), page(2), page(4)].concat();
        assert_eq!(all.len(), 5);
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 5, "no repeats across pages");
    }

    #[test]
    fn upsert_replaces_and_keeps_first_seen() {
        let index = index_of(&[doc("http://a.example/", Network::Clearnet, "old title", "h1")]);
        let mut recrawled = doc("http://a.example/", Network::Clearnet, "new title", "h1");
        recrawled.first_seen = 5_000;
        recrawled.last_seen = 5_000;
        index.upsert(&recrawled).unwrap();
        index.commit().unwrap();

        assert_eq!(index.num_docs(), 1);
        assert!(hits(&index, "old").is_empty());
        let hit = &hits(&index, "new")[0];
        assert_eq!((hit.first_seen, hit.last_seen), (1_000, 5_000));
    }

    #[test]
    fn updates_liveness_without_the_content() {
        let mut page = doc("http://a.example/", Network::Clearnet, "kept", "h1");
        page.keywords = vec!["k1".into(), "k2".into()];
        let index = index_of(std::slice::from_ref(&page));
        assert!(index.set_liveness("http://a.example/", 0.25, Some(9_000)).unwrap());
        assert!(!index.set_liveness("http://missing.example/", 0.0, None).unwrap());
        index.commit().unwrap();

        let stored = index.get("http://a.example/").unwrap().unwrap();
        assert_eq!(stored, IndexedDoc { liveness: 0.25, last_seen: 9_000, ..page });
        index.delete("http://a.example/").unwrap();
        index.commit().unwrap();
        assert_eq!(index.num_docs(), 0);
    }

    #[test]
    fn stores_torrent_files() {
        let mut magnet = doc("magnet:?xt=urn:btih:00", Network::Magnet, "Debian ISO", "");
        magnet.info_hash = Some("00".into());
        magnet.files = vec![FileMeta { path: "debian-13-amd64-netinst.iso".into(), size: 700 << 20 }];
        let index = index_of(std::slice::from_ref(&magnet));
        let hit = &hits(&index, "netinst")[0];
        assert_eq!(hit.files, magnet.files);
        assert_eq!(hit.info_hash.as_deref(), Some("00"));
        assert_eq!(hit.content_hash, "magnet:?xt=urn:btih:00", "no hash falls back to the url");
    }

    #[test]
    fn malformed_queries_do_not_error() {
        let index = index_of(&[]);
        assert!(index.search(&query("title:( \"unterminated", &[])).is_ok());
    }

    #[test]
    fn reopens_from_disk_and_reads_alongside_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        {
            let index = SearchIndex::open(dir.path()).unwrap();
            index.upsert(&doc("http://a.example/", Network::Clearnet, "persisted", "h1")).unwrap();
            index.commit().unwrap();
        }
        let writer = SearchIndex::open(dir.path()).unwrap();
        assert_eq!(hits(&writer, "persisted").len(), 1);

        let reader = SearchIndex::open_read_only(dir.path()).unwrap();
        assert_eq!(hits(&reader, "persisted").len(), 1, "the reader registers the same analyzer");
        assert!(reader.upsert(&doc("http://b.example/", Network::Clearnet, "x", "h2")).is_err());
        assert!(SearchIndex::open_read_only(&dir.path().join("missing")).is_err());
    }
}
