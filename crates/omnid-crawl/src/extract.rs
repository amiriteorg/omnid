//! Turns a fetched body into the fields the index wants.
//!
//! HTML goes through html5ever's tokenizer only, with no tree builder.
//! Building a tree costs time proportional to nesting depth for every element,
//! so a hostile page of nested tags can pin a core for minutes. The
//! tokenizer is linear, and title, meta tags, links and visible text need no
//! tree.

use std::borrow::Cow;
use std::cell::RefCell;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

const MAX_TEXT_BYTES: usize = 100_000;
const MAX_TITLE_BYTES: usize = 300;
const MAX_DESCRIPTION_BYTES: usize = 500;
const FALLBACK_DESCRIPTION_BYTES: usize = 280;
const MAX_KEYWORDS: usize = 32;
const MAX_LINKS: usize = 500;
/// Elements whose text is not shown to a reader.
const INVISIBLE: [&str; 5] = ["script", "style", "noscript", "template", "svg"];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Extracted {
    pub title: String,
    pub description: String,
    pub keywords: Vec<String>,
    /// Visible text, whitespace collapsed.
    pub text: String,
    /// Link targets as written, unresolved.
    pub links: Vec<String>,
    /// `<meta name="robots">` asked not to index this page...
    pub noindex: bool,
    /// ...or not to follow its links.
    pub nofollow: bool,
}

pub fn extract(content_type: Option<&str>, body: &[u8]) -> Extracted {
    let html = content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("html"));
    let source = decode_text(content_type, body, html);
    if html {
        return extract_html(&source);
    }
    let text = collapse(&source, MAX_TEXT_BYTES);
    Extracted { description: truncate(&text, FALLBACK_DESCRIPTION_BYTES).to_owned(), text, ..Default::default() }
}

/// Decodes in the HTML spec's order of precedence: byte-order mark, the
/// Content-Type charset, a `<meta>` charset in the first 1024 bytes, UTF-8.
fn decode_text<'a>(content_type: Option<&str>, body: &'a [u8], html: bool) -> Cow<'a, str> {
    let from_header = content_type.and_then(charset_param).and_then(|label| Encoding::for_label(label.as_bytes()));
    let from_meta = || {
        let encoding = Encoding::for_label(meta_charset(body)?.as_bytes())?;
        // A page that says it is UTF-16 in ASCII is not UTF-16 (spec §13.2.3.2).
        Some(if encoding == UTF_16LE || encoding == UTF_16BE { UTF_8 } else { encoding })
    };
    let encoding = from_header.or_else(|| if html { from_meta() } else { None }).unwrap_or(UTF_8);
    // decode() lets a byte-order mark override the label.
    encoding.decode(body).0
}

fn charset_param(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim().eq_ignore_ascii_case("charset").then(|| value.trim().trim_matches(['"', '\'']))
    })
}

/// A simplified version of the spec's prescan: finds `charset=` inside a
/// `<meta` tag, which covers both `<meta charset>` and the `http-equiv` form.
fn meta_charset(body: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(1024)]).to_ascii_lowercase();
    let mut rest = head.as_str();
    while let Some(start) = rest.find("<meta") {
        rest = &rest[start + 5..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        let Some(at) = tag.find("charset") else { continue };
        let Some(value) = tag[at + 7..].trim_start().strip_prefix('=') else { continue };
        let value = value.trim_start().trim_start_matches(['"', '\'']);
        let end = value.find(['"', '\'', ';', ' ', '/', '>']).unwrap_or(value.len());
        if end > 0 {
            return Some(value[..end].to_owned());
        }
    }
    None
}

fn extract_html(source: &str) -> Extracted {
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(source));
    let tokenizer = Tokenizer::new(Sink::default(), TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let page = tokenizer.sink.0.take();

    let mut out = page.out;
    out.title = collapse(&page.title, MAX_TITLE_BYTES);
    out.text = collapse(&page.text, MAX_TEXT_BYTES);
    if out.description.is_empty() {
        out.description = truncate(&out.text, FALLBACK_DESCRIPTION_BYTES).to_owned();
    }
    out
}

#[derive(Default)]
struct Sink(RefCell<Page>);

#[derive(Default)]
struct Page {
    out: Extracted,
    title: String,
    text: String,
    in_title: bool,
    title_done: bool,
    /// Open invisible elements.
    hidden: usize,
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut page = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => return page.tag(&tag),
            Token::CharacterTokens(text) => page.characters(&text),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

impl Page {
    fn tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        // Tags separate words. One space per run of tags, or a page of bare
        // markup would fill the text budget with spaces.
        if !self.text.is_empty() && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
        if tag.kind == TagKind::EndTag {
            if name == "title" && self.in_title {
                self.in_title = false;
                self.title_done = true;
            }
            if INVISIBLE.contains(&name) {
                self.hidden = self.hidden.saturating_sub(1);
            }
            return TokenSinkResult::Continue;
        }

        match name {
            "title" if !self.title_done => self.in_title = true,
            "meta" => self.meta(tag),
            "a" if self.out.links.len() < MAX_LINKS => {
                if let Some(href) = attr(tag, "href") {
                    self.out.links.push(href.to_owned());
                }
            }
            _ => {}
        }
        if INVISIBLE.contains(&name) && !tag.self_closing {
            self.hidden += 1;
        }
        // The tokenizer switches states for these on the tree builder's say;
        // without one, the sink has to.
        match name {
            "script" => TokenSinkResult::RawData(RawKind::ScriptData),
            "style" | "noscript" | "xmp" | "iframe" | "noembed" | "noframes" => {
                TokenSinkResult::RawData(RawKind::Rawtext)
            }
            "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        }
    }

    fn meta(&mut self, tag: &Tag) {
        let name = attr(tag, "name").or_else(|| attr(tag, "property")).unwrap_or_default().to_ascii_lowercase();
        let content = attr(tag, "content").unwrap_or_default();
        let out = &mut self.out;
        match name.as_str() {
            "description" | "og:description" if out.description.is_empty() => {
                out.description = collapse(content, MAX_DESCRIPTION_BYTES);
            }
            "keywords" => {
                let room = MAX_KEYWORDS.saturating_sub(out.keywords.len());
                out.keywords
                    .extend(content.split(',').map(str::trim).filter(|k| !k.is_empty()).take(room).map(str::to_owned));
            }
            "robots" => {
                let directives = content.to_ascii_lowercase();
                out.noindex |= directives.contains("noindex") || directives.contains("none");
                out.nofollow |= directives.contains("nofollow") || directives.contains("none");
            }
            _ => {}
        }
    }

    fn characters(&mut self, text: &str) {
        if self.in_title {
            if self.title.len() < MAX_TITLE_BYTES * 2 {
                self.title.push_str(text);
            }
        } else if self.hidden == 0 && self.text.len() < MAX_TEXT_BYTES * 2 {
            self.text.push_str(text);
        }
    }
}

fn attr<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs.iter().find(|a| &*a.name.local == name).map(|a| &*a.value)
}

fn collapse(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() >= max_bytes {
            break;
        }
    }
    // The cut can land just after a space.
    truncate(&out, max_bytes).trim_end().to_owned()
}

fn truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><html><head>
        <title>  The   Hidden
        Wiki </title>
        <meta name="Description" content="A directory of onion services">
        <meta name="keywords" content="wiki, directory , ,onion">
        <style>body { color: red }</style>
        </head><body>
        <h1>Welcome</h1><script>var s = "</div><a href='/fake'>";</script>
        <p>Links <a href="/a">A</a> and <a href="http://example.com/b#x">B</a> &amp; more</p>
        <noscript><a href="/noscript">enable js</a></noscript>
        <a name="anchor-without-href">x</a>
        </body></html>"#;

    #[test]
    fn extracts_title_meta_links_and_visible_text() {
        let e = extract(Some("text/html; charset=utf-8"), PAGE.as_bytes());
        assert_eq!(e.title, "The Hidden Wiki");
        assert_eq!(e.description, "A directory of onion services");
        assert_eq!(e.keywords, ["wiki", "directory", "onion"]);
        assert_eq!(e.links, ["/a", "http://example.com/b#x"], "script and noscript bodies are not markup");
        assert_eq!(e.text, "Welcome Links A and B & more x");
        assert!(!e.noindex && !e.nofollow);
    }

    #[test]
    fn handles_omitted_end_tags() {
        let e = extract(Some("text/html"), b"<html><head><title>T</title><body><p>one<p>two");
        assert_eq!(e.title, "T");
        assert_eq!(e.text, "one two");
    }

    #[test]
    fn honours_meta_robots() {
        let e = extract(Some("text/html"), br#"<meta name="robots" content="noindex, nofollow"><p>x</p>"#);
        assert!(e.noindex && e.nofollow);
        let e = extract(Some("text/html"), br#"<meta name="ROBOTS" content="none">"#);
        assert!(e.noindex && e.nofollow);
    }

    #[test]
    fn falls_back_to_text_for_the_description() {
        let e = extract(Some("text/html"), b"<p>only   body text</p>");
        assert_eq!(e.description, "only body text");
    }

    #[test]
    fn plain_text_is_indexed_as_text() {
        let e = extract(Some("text/plain"), b"line one\n\nline two <a href=x>");
        assert_eq!(e.text, "line one line two <a href=x>");
        assert!(e.links.is_empty());
    }

    #[test]
    fn decodes_declared_charsets() {
        let (cyrillic, _, _) = encoding_rs::WINDOWS_1251.encode("<title>Привет, мир</title>");
        assert_eq!(extract(Some("text/html; charset=windows-1251"), &cyrillic).title, "Привет, мир");
        assert_eq!(extract(Some("text/html; charset=\"CP1251\""), &cyrillic).title, "Привет, мир");

        let mut page = b"<meta charset='windows-1251'>".to_vec();
        page.extend_from_slice(&cyrillic);
        assert_eq!(extract(Some("text/html"), &page).title, "Привет, мир", "<meta charset>");

        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("<title>日本語</title>");
        let mut page = br#"<meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS">"#.to_vec();
        page.extend_from_slice(&sjis);
        assert_eq!(extract(Some("text/html"), &page).title, "日本語", "http-equiv form");
    }

    #[test]
    fn bom_beats_labels_and_utf8_is_the_default() {
        let mut utf16: Vec<u8> = vec![0xFF, 0xFE];
        utf16.extend("<title>wide</title>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(extract(Some("text/html; charset=iso-8859-1"), &utf16).title, "wide");
        assert_eq!(extract(Some("text/html"), "<title>café</title>".as_bytes()).title, "café");
        assert_eq!(extract(Some("text/html"), b"<meta charset=utf-16><title>x</title>").title, "x");
    }

    #[test]
    fn deep_nesting_is_linear() {
        // A tree builder takes minutes on this; the tokenizer, milliseconds.
        let deep = "<div>".repeat(300_000) + "deep" + &"</div>".repeat(300_000);
        let e = extract(Some("text/html"), deep.as_bytes());
        assert_eq!(e.text, "deep");
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(collapse("a  b  c", 3), "a b");
        assert_eq!(collapse("ab cd", 3), "ab", "no trailing space where the cut falls after one");
    }
}
