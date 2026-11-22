//! The unified index: one Tantivy index for every network, with the network
//! as a filter facet and the content hash as the key that folds mirrors of
//! the same content into one search result.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use omnid_core::{FileMeta, IndexedDoc, Network};
use serde::Serialize;
use tantivy::collector::{Collector, SegmentCollector, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, QueryParser, TermQuery};
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
pub const SCHEMA_VERSION: u32 = 3;

/// The deepest a search pages: past this many results nothing more is
/// returned. The offset comes from whoever asks, and the work grows with it.
pub const MAX_OFFSET: usize = 10_000;

/// Upper bound on the addresses listed for one piece of content.
const MAX_ROUTES: usize = 16;
const WRITER_HEAP_BYTES: usize = 50_000_000;
const SNIPPET_CHARS: usize = 220;
/// Lowercase, fold accents ("café" finds "cafe") and stem English
/// ("bananas" finds "banana").
const TOKENIZER: &str = "omnid";
/// The word being typed matches the words it starts from this many letters.
const MIN_PREFIX: usize = 3;
/// A word is matched with one letter wrong, missing, extra or swapped from
/// this many letters when nothing matches as typed, and two from twice it.
const MIN_FUZZY: usize = 4;
/// Factor for a page in a language the reader did not ask for: it needs
/// about 1.4x the text relevance to outrank a page in theirs.
const OTHER_LANGUAGE: Score = 0.7;
/// Each step down the reader's list of languages costs this much.
const LATER_LANGUAGE: Score = 0.05;
/// A site's root page is boosted by this much, falling off with depth: short
/// queries more often mean a site than one of its pages.
const ROOT_BOOST: Score = 0.2;

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
    /// The reader's languages, best first (from `Accept-Language`). Pages in
    /// them rank above pages in other languages; pages whose language is
    /// unknown are not penalised. Empty means no preference.
    pub languages: Vec<String>,
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
    /// Nothing matched as typed, so words were matched with a letter or two
    /// wrong.
    pub fuzzy: bool,
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
    /// Primary language subtag, declared by the page or detected.
    pub lang: Option<String>,
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
    lang: Field,
    prior: Field,
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
        liveness: b.add_f64_field("liveness", STORED),
        trust: b.add_f64_field("trust", STORED),
        lang: b.add_text_field("lang", STRING | STORED | FAST),
        // Trust, liveness and depth folded into one factor when the document
        // is written, so ranking reads one column instead of three.
        prior: b.add_f64_field("prior", FAST),
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
        let lang = match &doc.lang {
            Some(lang) => Some(lang.clone()),
            // A full body says it best; titles often keep an English brand name.
            None if doc.body.len() >= 200 => omnid_core::lang::detect(&doc.body),
            None => omnid_core::lang::detect(&format!("{} {} {}", doc.title, doc.description, doc.body)),
        };
        if let Some(lang) = lang {
            d.add_text(f.lang, lang);
        }
        d.add_f64(f.prior, prior(doc).into());

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
        let several = q.text.split_whitespace().nth(1).is_some();
        // Every word first, for precision; then every word with spelling
        // slips; then, with several words, any of them, as typed and then
        // with slips.
        let mut results = self.search_with(&searcher, q, self.parse(&q.text, true))?;
        if results.total == 0
            && let Some(close) = self.close_spellings(&q.text, true)
        {
            results = SearchResults { fuzzy: true, ..self.search_with(&searcher, q, close)? };
        }
        if results.total == 0 && several {
            results = SearchResults { relaxed: true, ..self.search_with(&searcher, q, self.parse(&q.text, false))? };
            if results.total == 0
                && let Some(close) = self.close_spellings(&q.text, false)
            {
                results = SearchResults { relaxed: true, fuzzy: true, ..self.search_with(&searcher, q, close)? };
            }
        }
        Ok(results)
    }

    fn search_with(&self, searcher: &Searcher, q: &SearchQuery, text_query: Box<dyn Query>) -> Result<SearchResults> {
        let f = self.fields;
        let limit = q.limit.clamp(1, 100);
        // Over-fetch so that folding mirrors together still fills the page.
        // The offset comes from the request: unbounded, it made Tantivy try to
        // allocate terabytes and abort the process, which no task can catch.
        let offset = q.offset.min(MAX_OFFSET);
        let window = (offset + limit) * 4;
        let languages: Arc<[String]> = q.languages.iter().cloned().collect();
        let top_docs =
            TopDocs::with_limit(window).tweak_score(move |segment: &SegmentReader| rank(segment, &languages));

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
            if !grouped.insert(content_hash.clone()) || grouped.len() <= offset {
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
                lang: doc.get_first(f.lang).and_then(|v| v.as_str()).map(str::to_owned),
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
        Ok(SearchResults { hits, total, network_counts, relaxed: false, fuzzy: false })
    }

    /// The fields text is searched in, with their weights.
    fn text_fields(&self) -> [(Field, Score); 5] {
        let f = self.fields;
        [(f.title, 3.0), (f.keywords, 2.0), (f.description, 1.5), (f.body, 1.0), (f.file_names, 1.5)]
    }

    fn parse(&self, text: &str, conjunction: bool) -> Box<dyn Query> {
        if text.trim().is_empty() {
            return Box::new(AllQuery);
        }
        let parse = |text: &str| {
            let fields = self.text_fields();
            let mut parser = QueryParser::for_index(&self.index, fields.iter().map(|(field, _)| *field).collect());
            if conjunction {
                parser.set_conjunction_by_default();
            }
            for (field, boost) in fields {
                parser.set_field_boost(field, boost);
            }
            parser.parse_query_lenient(text).0
        };
        let exact = parse(text);
        // The last word may still be being typed: it also matches the words
        // it starts, alongside the query as typed.
        let Some((head, word)) = last_word(text).filter(|(_, word)| word.chars().count() >= MIN_PREFIX) else {
            return exact;
        };
        let Some(prefix) = self.prefix_query(word) else {
            return exact;
        };
        let typing = if conjunction && !head.trim().is_empty() {
            Box::new(BooleanQuery::new(vec![(Occur::Must, parse(head)), (Occur::Must, prefix)]))
        } else {
            prefix
        };
        Box::new(BooleanQuery::new(vec![(Occur::Should, exact), (Occur::Should, typing)]))
    }

    /// Words starting with `word`, in every text field. Folded but not
    /// stemmed: what has been typed so far is the start of a word, not one.
    fn prefix_query(&self, word: &str) -> Option<Box<dyn Query>> {
        let mut folding = TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(AsciiFoldingFilter)
            .build();
        let start = single_token(&mut folding, word)?;
        let fields = self
            .text_fields()
            .into_iter()
            .map(|(field, boost)| -> (Occur, Box<dyn Query>) {
                let query = FuzzyTermQuery::new_prefix(Term::from_field_text(field, &start), 0, false);
                (Occur::Should, Box::new(BoostQuery::new(Box::new(query), boost)))
            })
            .collect();
        Some(Box::new(BooleanQuery::new(fields)))
    }

    /// The query with spelling slips allowed in each word, for when nothing
    /// matches as typed. Plain words only: a query with phrases, fields or
    /// exclusions is taken as typed.
    fn close_spellings(&self, text: &str, conjunction: bool) -> Option<Box<dyn Query>> {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() || !words.iter().all(|w| is_plain(w)) {
            return None;
        }
        let mut analyzer = self.index.tokenizer_for_field(self.fields.body).ok()?;
        let occur = if conjunction { Occur::Must } else { Occur::Should };
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for word in words {
            let term = single_token(&mut analyzer, word)?;
            let letters = word.chars().count();
            let distance = if letters >= 2 * MIN_FUZZY { 2 } else { u8::from(letters >= MIN_FUZZY) };
            let fields = self
                .text_fields()
                .into_iter()
                .map(|(field, boost)| -> (Occur, Box<dyn Query>) {
                    let term = Term::from_field_text(field, &term);
                    let query: Box<dyn Query> = if distance == 0 {
                        Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs))
                    } else {
                        Box::new(FuzzyTermQuery::new(term, distance, true))
                    };
                    (Occur::Should, Box::new(BoostQuery::new(query, boost)))
                })
                .collect();
            clauses.push((occur, Box::new(BooleanQuery::new(fields))));
        }
        Some(Box::new(BooleanQuery::new(clauses)))
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
            lang: doc.get_first(f.lang).and_then(|v| v.as_str()).map(str::to_owned),
        }
    }

    /// Copies every live document of an index written with an older schema
    /// into this one, deriving what the old schema lacked (language, ranking
    /// prior), and commits. Fields are matched by name. Returns how many
    /// documents were copied.
    pub fn migrate_from(&self, old_dir: &Path) -> Result<u64> {
        let old = Index::open_in_dir(old_dir)?;
        let schema = old.schema();
        let field = |name: &str| schema.get_field(name).ok();
        let (text_of, number_of, date_of) = (
            |doc: &TantivyDocument, name: &str| field(name).map(|f| text(doc, f)).unwrap_or_default(),
            |doc: &TantivyDocument, name: &str| field(name).map(|f| number(doc, f)).unwrap_or_default(),
            |doc: &TantivyDocument, name: &str| field(name).map(|f| date(doc, f)).unwrap_or_default(),
        );
        let reader: IndexReader = old.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let searcher = reader.searcher();
        let mut copied = 0;
        for segment in searcher.segment_readers() {
            let store = segment.get_store_reader(64)?;
            for doc in store.iter::<TantivyDocument>(segment.alive_bitset()) {
                let doc = doc?;
                self.upsert(&IndexedDoc {
                    url: text_of(&doc, "url"),
                    network: text_of(&doc, "network").parse().unwrap_or(Network::Clearnet),
                    site: text_of(&doc, "site"),
                    title: text_of(&doc, "title"),
                    description: text_of(&doc, "description"),
                    keywords: field("keywords")
                        .map(|f| doc.get_all(f).filter_map(|v| v.as_str().map(str::to_owned)).collect())
                        .unwrap_or_default(),
                    body: text_of(&doc, "body"),
                    content_hash: text_of(&doc, "content_hash"),
                    info_hash: Some(text_of(&doc, "info_hash")).filter(|h| !h.is_empty()),
                    files: field("files").map(|f| files(&doc, f)).unwrap_or_default(),
                    first_seen: date_of(&doc, "first_seen"),
                    last_seen: date_of(&doc, "last_seen"),
                    liveness: number_of(&doc, "liveness"),
                    trust: number_of(&doc, "trust"),
                    lang: field("lang").map(|f| text(&doc, f)).filter(|l| !l.is_empty()),
                })?;
                copied += 1;
            }
        }
        self.commit()?;
        Ok(copied)
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

/// Relevance times provenance, reliability, language and depth: an
/// operator's seed that answers every time outranks a stray link that is
/// usually down, a page in the reader's language outranks a translation, and
/// a site's front page edges out its inner pages. Each factor is modest, so
/// text relevance still dominates.
fn rank(segment: &SegmentReader, languages: &[String]) -> impl Fn(DocId, Score) -> Score + use<> {
    let fast = segment.fast_fields();
    let prior = fast.f64("prior").ok().map(|c| c.first_or_default_col(1.0));
    // The reader's languages as this segment's term ordinals, best first; a
    // language with no document in the segment keeps its place as `None`.
    let lang = fast.str("lang").ok().flatten().filter(|_| !languages.is_empty());
    let preferred: Vec<Option<u64>> = match &lang {
        Some(column) => languages.iter().map(|l| column.dictionary().term_ord(l).ok().flatten()).collect(),
        None => vec![],
    };
    move |doc, score| {
        let prior = prior.as_ref().map_or(1.0, |c| c.get_val(doc)) as Score;
        let language = match lang.as_ref().map(|c| c.term_ords(doc).next()) {
            Some(Some(ord)) => match preferred.iter().position(|p| *p == Some(ord)) {
                Some(rank) => 1.0 - LATER_LANGUAGE * rank as Score,
                None => OTHER_LANGUAGE,
            },
            // No preference, or the page's language is unknown.
            _ => 1.0,
        };
        score * prior * language
    }
}

/// What a page's score is multiplied by before the reader's languages: its
/// trust, whether it was alive when last checked, and how near its site's root
/// it is.
fn prior(doc: &IndexedDoc) -> Score {
    let shallow = 1.0 + ROOT_BOOST / (1.0 + depth(&doc.url) as Score);
    (0.5 + doc.trust) * (0.6 + 0.4 * doc.liveness) * shallow
}

/// How far below its site's root a page is: path segments for URLs, past the
/// key and edition for freesites, 0 for magnets.
fn depth(url: &str) -> u64 {
    let segments = |path: &str| path.split('/').filter(|s| !s.is_empty()).count() as u64;
    match omnid_core::Source::parse(url) {
        Ok(omnid_core::Source::Hyphanet(key)) => {
            // USK@key/site/edition/... and SSK@key/site-edition/...
            let base = if key.starts_with("USK@") { 3 } else { 2 };
            segments(&key).saturating_sub(base)
        }
        Ok(omnid_core::Source::Ipfs(url)) => segments(url.path()) + u64::from(url.query().is_some()),
        Ok(source) => source.url().map_or(0, |u| segments(u.path()) + u64::from(u.query().is_some())),
        Err(_) => 0,
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

/// A plain word: letters and digits only, no phrase, field or exclusion.
fn is_plain(word: &str) -> bool {
    !word.is_empty() && word.chars().all(char::is_alphanumeric)
}

/// The query's last word, if a plain one, and the text before it.
fn last_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_end();
    let start = text.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    let word = &text[start..];
    is_plain(word).then_some((&text[..start], word))
}

/// The one token `analyzer` makes of `word`, if it makes exactly one.
fn single_token(analyzer: &mut TextAnalyzer, word: &str) -> Option<String> {
    let mut tokens = Vec::new();
    analyzer.token_stream(word).process(&mut |token| tokens.push(token.text.clone()));
    (tokens.len() == 1).then(|| tokens.remove(0))
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
            lang: Some("en".into()),
        }
    }

    fn query(text: &str, networks: &[Network]) -> SearchQuery {
        SearchQuery { text: text.into(), networks: networks.to_vec(), limit: 10, ..SearchQuery::default() }
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
    fn matches_the_word_being_typed() {
        let index = index_of(&[
            doc("http://reg.i2p/", Network::I2p, "Name registry", "h1"),
            doc("http://forum.example/", Network::Clearnet, "Privacy forum", "h2"),
        ]);
        for typed in ["reg", "regis", "registr", "registry", "name regis", "Name REGIS"] {
            assert_eq!(hits(&index, typed).len(), 1, "{typed}");
        }
        assert!(hits(&index, "re").is_empty(), "two letters are too few to go on");
        assert!(hits(&index, "forum regis -privacy").is_empty(), "the rest of the query still holds");
        assert_eq!(hits(&index, "\"name registry\"").len(), 1, "phrases as before");
        assert!(!index.search(&query("regis", &[])).unwrap().fuzzy);
    }

    #[test]
    fn finds_close_spellings_when_nothing_matches() {
        let index = index_of(&[
            doc("http://t.onion/", Network::Onion, "Tor Project", "h1"),
            doc("http://n.example/", Network::Clearnet, "Network news", "h2"),
        ]);
        let exact = index.search(&query("tor project", &[])).unwrap();
        assert_eq!((exact.hits.len(), exact.fuzzy, exact.relaxed), (1, false, false));
        // A swapped pair and a missing letter: every word still required.
        let close = index.search(&query("tor projetc", &[])).unwrap();
        assert_eq!((close.hits.len(), close.fuzzy, close.relaxed), (1, true, false));
        assert_eq!(hits(&index, "netwrk").len(), 1);
        // Short words are not guessed at, and nothing like it finds nothing.
        assert!(hits(&index, "tox").is_empty());
        assert!(hits(&index, "zzzzzz").is_empty());
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

    fn in_language(url: &str, title: &str, content_hash: &str, lang: &str) -> IndexedDoc {
        IndexedDoc { lang: Some(lang.into()), ..doc(url, Network::Clearnet, title, content_hash) }
    }

    fn urls(index: &SearchIndex, q: &SearchQuery) -> Vec<String> {
        index.search(q).unwrap().hits.iter().map(|h| h.routes[0].url.clone()).collect()
    }

    #[test]
    fn ranks_the_readers_languages_first() {
        // On trust alone the Turkmen translation wins.
        let mut turkmen = in_language("http://tor.example/tk/", "tor browser", "h1", "tk");
        turkmen.trust = 0.7;
        let index = index_of(&[
            turkmen,
            in_language("http://tor.example/de/", "tor browser", "h2", "de"),
            in_language("http://tor.example/en/", "tor browser", "h3", "en"),
        ]);
        let ask = |languages: &[&str]| {
            let languages = languages.iter().map(|l| (*l).to_owned()).collect();
            urls(&index, &SearchQuery { languages, ..query("tor browser", &[]) })
        };
        assert_eq!(ask(&[])[0], "http://tor.example/tk/", "no preference: trust decides");
        assert_eq!(ask(&["en"])[0], "http://tor.example/en/");
        assert_eq!(
            ask(&["de", "en"])[..2],
            ["http://tor.example/de/", "http://tor.example/en/"],
            "in the reader's order"
        );
        assert_eq!(ask(&["xx"]), ask(&[]), "a language no page has changes nothing");
        assert_eq!(ask(&["en"]).len(), 3, "a preference reorders, never filters");
    }

    #[test]
    fn does_not_demote_pages_of_unknown_language() {
        let mut unknown = doc("http://a.example/x/", Network::Clearnet, "zq7", "h1");
        unknown.lang = None;
        let index = index_of(&[in_language("http://b.example/x/", "zq7", "h2", "fr"), unknown]);
        let hits = index.search(&SearchQuery { languages: vec!["en".into()], ..query("zq7", &[]) }).unwrap().hits;
        assert_eq!(hits[0].routes[0].url, "http://a.example/x/");
        assert_eq!(hits[0].lang, None, "too little text to tell");
    }

    #[test]
    fn detects_the_language_of_pages_that_declare_none() {
        let mut english = doc("http://a.example/", Network::Clearnet, "history", "h1");
        english.body = "The Tor Project began in the mid-1990s, when researchers asked whether there was a way \
                        to create internet connections that do not reveal who is talking to whom, even to \
                        someone watching the network. It grew into the network millions use today."
            .into();
        let mut japanese = doc("http://b.example/", Network::Clearnet, "history", "h2");
        japanese.title = "Torの歴史".into();
        japanese.description =
            "Torプロジェクトは1990年代半ばに始まりました。誰が誰と話しているかを明かさない接続を作る研究です。".into();
        english.lang = None;
        japanese.lang = None;
        let index = index_of(&[english, japanese]);
        let lang_of = |url: &str| index.get(url).unwrap().unwrap().lang;
        assert_eq!(lang_of("http://a.example/").as_deref(), Some("en"), "from the body");
        assert_eq!(lang_of("http://b.example/").as_deref(), Some("ja"), "a short body: with title and description");
    }

    #[test]
    fn ranks_a_sites_root_above_its_deeper_pages() {
        // Deepest first, so insertion order cannot be what puts the root first.
        let index = index_of(&[
            doc("http://a.example/x/y/z/", Network::Clearnet, "market", "h1"),
            doc("http://a.example/x/", Network::Clearnet, "market", "h2"),
            doc("http://a.example/", Network::Clearnet, "market", "h3"),
        ]);
        assert_eq!(
            urls(&index, &query("market", &[])),
            ["http://a.example/", "http://a.example/x/", "http://a.example/x/y/z/"]
        );
    }

    #[test]
    fn measures_depth_below_a_sites_root() {
        assert_eq!(depth("http://a.example/"), 0);
        assert_eq!(depth("http://a.example/about/history/"), 2);
        assert_eq!(depth("http://a.example/search?q=tor"), 2, "a query counts as a level");
        assert_eq!(depth("USK@abc,def,AQACAAE/site/3/"), 0, "a USK's site and edition are its root");
        assert_eq!(depth("USK@abc,def,AQACAAE/site/3/about/history.html"), 2);
        assert_eq!(depth("SSK@abc,def,AQACAAE/site-3/page.html"), 1);
        assert_eq!(depth("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"), 0);
        assert_eq!(depth("not an address"), 0);
    }

    #[test]
    fn migrates_an_older_index_and_detects_its_languages() {
        // index-v2's stored fields: these, without `lang` and `depth`.
        let mut b = Schema::builder();
        for name in ["url", "network", "site", "content_hash", "info_hash"] {
            b.add_text_field(name, STRING | STORED);
        }
        for name in ["title", "description", "keywords", "body", "files"] {
            b.add_text_field(name, STORED);
        }
        for name in ["first_seen", "last_seen"] {
            b.add_date_field(name, STORED);
        }
        for name in ["liveness", "trust"] {
            b.add_f64_field(name, STORED);
        }
        let old_dir = tempfile::tempdir().unwrap();
        let old = Index::create_in_dir(old_dir.path(), b.build()).unwrap();
        let field = |name: &str| old.schema().get_field(name).unwrap();
        let mut writer: IndexWriter = old.writer(15_000_000).unwrap();
        let mut page = TantivyDocument::default();
        page.add_text(field("url"), "http://tor.example/about/history/");
        page.add_text(field("network"), "clearnet");
        page.add_text(field("site"), "tor.example");
        page.add_text(field("title"), "History");
        page.add_text(field("keywords"), "tor");
        page.add_text(field("keywords"), "history");
        page.add_text(
            field("body"),
            "The Tor Project began in the mid-1990s, when researchers asked whether there was a way to create \
             internet connections that do not reveal who is talking to whom, even to someone watching the network.",
        );
        page.add_text(field("content_hash"), "h1");
        page.add_date(field("first_seen"), DateTime::from_timestamp_secs(1_000));
        page.add_date(field("last_seen"), DateTime::from_timestamp_secs(2_000));
        page.add_f64(field("liveness"), 0.5);
        page.add_f64(field("trust"), 0.8);
        writer.add_document(page).unwrap();
        let mut gone = TantivyDocument::default();
        gone.add_text(field("url"), "http://tor.example/gone/");
        writer.add_document(gone).unwrap();
        writer.delete_term(Term::from_field_text(field("url"), "http://tor.example/gone/"));
        writer.commit().unwrap();
        drop(writer);

        let index = SearchIndex::in_memory().unwrap();
        assert_eq!(index.migrate_from(old_dir.path()).unwrap(), 1, "deleted documents stay behind");
        let doc = index.get("http://tor.example/about/history/").unwrap().unwrap();
        assert_eq!(
            (doc.title.as_str(), doc.keywords.as_slice()),
            ("History", ["tor".to_owned(), "history".to_owned()].as_slice())
        );
        assert_eq!((doc.first_seen, doc.last_seen, doc.liveness, doc.trust), (1_000, 2_000, 0.5, 0.8));
        assert_eq!(doc.lang.as_deref(), Some("en"), "detected on the way in");
        assert_eq!(hits(&index, "researchers").len(), 1, "indexed with this version's analyzer");
        assert!(index.migrate_from(&old_dir.path().join("missing")).is_err());
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
        // An offset from a hostile request: an empty page, not an allocation
        // of terabytes that aborts the process.
        assert!(page(1 << 40).is_empty());
        assert!(page(usize::MAX).is_empty(), "no overflow either");
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
