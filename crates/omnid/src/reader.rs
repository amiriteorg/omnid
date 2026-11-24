//! The reader: Gemini capsules and Gopher holes, which no ordinary browser
//! opens, as pages of the search page's own (`/read?url=…`). It reaches them
//! as omnid's browser reaches the web ([`Route::Browser`]): through Tor, on
//! circuits of the site's own, `.i2p` through I2P, never directly. What a
//! server sends is shown as text: its lines become headings, paragraphs,
//! lists and links, escaped, and the page runs no script.

use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use omnid_core::{Network, Source};
use omnid_crawl::Verdict;
use omnid_crawl::extract::{decode, extract};
use omnid_crawl::small_web::{GEMTEXT_TYPE, GOPHER_MENU_TYPE, GemLine, gemtext, gopher_menu};
use omnid_net::{FetchOptions, Route, SmallWeb};
use serde::Deserialize;

use crate::api::AppState;

/// The search page's policy, without scripts, and with forms that come back
/// here: a Gemini page that asks for input is answered with one.
const CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; \
                   base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
/// Redirects followed within Gemini and Gopher; one elsewhere is shown.
const MAX_REDIRECTS: usize = 5;
const MAX_PAGE_BYTES: usize = 2 << 20;
/// The search page's mark, from `ui/index.html`.
const BRAND_MARK: &str = r#"<svg class="brand-mark" viewBox="0 0 49 49" aria-hidden="true"><path d="M2.89,15.68a12.79,12.79 0 1,0 25.58,0a12.79,12.79 0 1,0 -25.58,0M20.53,15.68a12.79,12.79 0 1,0 25.58,0a12.79,12.79 0 1,0 -25.58,0M2.89,33.32a12.79,12.79 0 1,0 25.58,0a12.79,12.79 0 1,0 -25.58,0M20.53,33.32a12.79,12.79 0 1,0 25.58,0a12.79,12.79 0 1,0 -25.58,0"/></svg>"#;

#[derive(Deserialize)]
pub(crate) struct ReadParams {
    url: String,
    /// What a Gemini page asked for, sent as its query.
    input: Option<String>,
    /// Shown inside the Android app, whose browser opens onion and `.i2p`
    /// links; anywhere else they are shown for copying.
    embedded: Option<String>,
}

pub(crate) async fn read(State(state): State<AppState>, Query(params): Query<ReadParams>) -> Response {
    let embedded = params.embedded.is_some();
    let page = Page { embedded, ..Page::default() };
    let source = match Source::parse(&params.url) {
        Ok(source @ (Source::Gemini(_) | Source::Gopher(_))) => source,
        _ => return page.message(StatusCode::BAD_REQUEST, "Not a Gemini or Gopher address", &params.url),
    };
    let mut source = match &params.input {
        Some(input) => match with_input(&source, input) {
            Some(source) => source,
            None => return page.message(StatusCode::BAD_REQUEST, "That input does not fit in an address", ""),
        },
        None => source,
    };
    let opts = FetchOptions { max_bytes: MAX_PAGE_BYTES, accept: readable, max_redirects: 0 };
    for _ in 0..=MAX_REDIRECTS {
        let page = Page { source: Some(source.clone()), ..page.clone() };
        if state.safety.check_source(&source) != Verdict::Allow {
            return page.message(StatusCode::FORBIDDEN, "That site is on the denylist", "");
        }
        let route = Route::Browser { site: source.site_key() };
        let answer = match state.gateway.small_web(&source, &opts, &route).await {
            Ok(answer) => answer,
            Err(e) => return page.message(StatusCode::BAD_GATEWAY, "Could not reach it", &e.to_string()),
        };
        match answer {
            SmallWeb::Redirect { location, .. } => match source.join(&location) {
                Some(next @ (Source::Gemini(_) | Source::Gopher(_))) => source = next,
                _ => return page.moved(&location),
            },
            answer => return page.answer(answer),
        }
    }
    page.message(StatusCode::BAD_GATEWAY, "Too many redirects", "")
}

/// What the reader shows: text, never images or files.
fn readable(content_type: Option<&str>) -> bool {
    let mime = content_type.unwrap_or_default().split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    matches!(mime.as_str(), GEMTEXT_TYPE | GOPHER_MENU_TYPE | "text/plain" | "text/html" | "text/markdown")
}

/// A Gemini address with `input` as its query, percent-encoded as Gemini
/// asks; `None` when it would not be an address (over 1024 bytes).
fn with_input(source: &Source, input: &str) -> Option<Source> {
    let Source::Gemini(url) = source else { return None };
    let mut url = url.clone();
    let mut query = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            query.push(byte as char);
        } else {
            let _ = write!(query, "%{byte:02X}");
        }
    }
    url.set_query(Some(&query));
    (url.as_str().len() <= 1024).then(|| Source::from_url(url).ok()).flatten()
}

#[derive(Clone, Default)]
struct Page {
    source: Option<Source>,
    embedded: bool,
}

impl Page {
    fn answer(&self, answer: SmallWeb) -> Response {
        let body = match answer {
            SmallWeb::Page { content_type, body: Some(body), truncated } => {
                let text = decode(Some(&content_type), &body);
                let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
                let mut html = match mime.as_str() {
                    GEMTEXT_TYPE => self.gemtext(&text),
                    GOPHER_MENU_TYPE => self.menu(&text),
                    "text/html" => {
                        let page = extract(Some(&content_type), &body);
                        format!(
                            "<p class=\"reader-note\">An HTML page, shown as its text.</p><p>{}</p>",
                            esc(&page.text)
                        )
                    }
                    _ => format!("<pre class=\"plain\">{}</pre>", esc(&text)),
                };
                if truncated {
                    html.push_str(
                        "<p class=\"reader-note\">The rest of this page is longer than the reader shows.</p>",
                    );
                }
                let title = extract(Some(&content_type), &body).title;
                return self.render(StatusCode::OK, &title, &html);
            }
            SmallWeb::Page { content_type, body: None, .. } => format!(
                "<p>This is a file of type <code>{}</code>, which the reader does not open.</p>",
                esc(&content_type)
            ),
            SmallWeb::Input { prompt, sensitive } => {
                let url = self.source.as_ref().map(ToString::to_string).unwrap_or_default();
                let kind = if sensitive { "password" } else { "text" };
                let embedded = if self.embedded { "<input type=\"hidden\" name=\"embedded\" value=\"\">" } else { "" };
                format!(
                    "<form class=\"reader-input\" method=\"get\" action=\"/read\">\
                     <label for=\"input\">{}</label>\
                     <input type=\"hidden\" name=\"url\" value=\"{}\">{embedded}\
                     <input id=\"input\" name=\"input\" type=\"{kind}\" autocomplete=\"off\" required>\
                     <button class=\"button primary\" type=\"submit\">Send</button></form>",
                    esc(if prompt.is_empty() { "This page asks for input" } else { &prompt }),
                    esc(&url),
                )
            }
            SmallWeb::Failed { status, message } => {
                let what = match status {
                    51 => "Not found",
                    52 => "Gone for good",
                    44 => "The server asks to slow down",
                    40..=49 => "The server cannot answer now",
                    60..=69 => "This page is for holders of a client certificate",
                    _ => "The server refused the request",
                };
                return self.message(StatusCode::OK, what, &message);
            }
            SmallWeb::Redirect { location, .. } => return self.moved(&location),
        };
        self.render(StatusCode::OK, "", &body)
    }

    /// A text/gemini page, line by line.
    fn gemtext(&self, text: &str) -> String {
        let mut html = String::new();
        let mut list = false;
        for line in gemtext(text) {
            let item = matches!(line, GemLine::Item(_));
            if list && !item {
                html.push_str("</ul>");
            } else if !list && item {
                html.push_str("<ul>");
            }
            list = item;
            match line {
                GemLine::Heading { level, text } => {
                    let _ = write!(html, "<h{level}>{}</h{level}>", esc(text));
                }
                GemLine::Text(text) if text.trim().is_empty() => html.push_str("<p class=\"gap\"></p>"),
                GemLine::Text(text) => {
                    let _ = write!(html, "<p>{}</p>", esc(text));
                }
                GemLine::Item(text) => {
                    let _ = write!(html, "<li>{}</li>", esc(text));
                }
                GemLine::Quote(text) => {
                    let _ = write!(html, "<blockquote>{}</blockquote>", esc(text));
                }
                GemLine::Link { url, label } => html.push_str(&self.link(url, label)),
                GemLine::Toggle { opens: true, alt } => {
                    let _ = write!(html, "<pre aria-label=\"{}\">", esc(alt));
                }
                GemLine::Toggle { opens: false, .. } => html.push_str("</pre>"),
                GemLine::Preformatted(text) => {
                    html.push_str(&esc(text));
                    html.push('\n');
                }
            }
        }
        if list {
            html.push_str("</ul>");
        }
        // A block the page never closed.
        if html.matches("<pre").count() > html.matches("</pre>").count() {
            html.push_str("</pre>");
        }
        html
    }

    /// A Gopher menu: its lines of text as they are, its items as links.
    fn menu(&self, text: &str) -> String {
        let mut html = String::from("<div class=\"menu\">");
        for item in gopher_menu(text) {
            let kind = match item.item_type {
                '1' => "dir",
                '0' => "txt",
                'h' => "web",
                '7' => "find",
                'i' | '3' => "",
                _ => "file",
            };
            let line = match (kind, item.url()) {
                ("", _) => esc(item.display),
                ("dir" | "txt" | "web", Some(url)) => self.anchor(&url, item.display),
                _ => format!("<span class=\"unopened\">{}</span>", esc(item.display)),
            };
            let _ = write!(html, "<div class=\"line\"><span class=\"kind\">{kind}</span>{line}</div>");
        }
        html.push_str("</div>");
        html
    }

    /// A Gemini link line.
    fn link(&self, url: &str, label: &str) -> String {
        format!("<p class=\"link\">{}</p>", self.anchor(url, if label.is_empty() { url } else { label }))
    }

    /// A link to `url` (relative to the page) with `label`, where it can be
    /// opened from here; the address as text where it cannot.
    fn anchor(&self, url: &str, label: &str) -> String {
        match self.href(url) {
            Some(href) => format!("<a href=\"{}\" rel=\"noreferrer noopener\">{}</a>", esc(&href), esc(label)),
            None => format!("<span class=\"unopened\" title=\"{}\">{}</span>", esc(url), esc(label)),
        }
    }

    /// Where a link goes, as the search page decides (`linkable` and `href`
    /// in `ui/app.js`): Gemini and Gopher here; the web in the browser;
    /// onions and `.i2p` only inside the app, whose browser reaches them;
    /// Hyphanet through the node; IPFS through the gateway outside the app.
    fn href(&self, url: &str) -> Option<String> {
        let target = match &self.source {
            Some(page) => page.join(url)?,
            None => Source::parse(url).ok()?,
        };
        let embedded = self.embedded;
        match &target {
            Source::Gemini(_) | Source::Gopher(_) => Some(reader_href(&target.to_string(), embedded)),
            Source::Clearnet(url) => Some(url.to_string()),
            Source::Onion(url) | Source::I2p(url) => embedded.then(|| url.to_string()),
            Source::Hyphanet(key) if embedded => Some(format!("http://{}/{key}", omnid_net::HYPHANET_HOST)),
            Source::Hyphanet(key) => Some(format!("/hyphanet/{key}")),
            Source::Ipfs(_) => (!embedded).then(|| format!("/{}", target.gateway_path().unwrap_or_default())),
            Source::Nostr(_) | Source::Magnet(_) => Some(target.to_string()),
            // An IRC app opens it, which reaches an onion or `.i2p` server
            // only if it was set up for one: inside OMNId's app, it asks.
            Source::Irc(_) => (embedded || target.transport() == Network::Clearnet).then(|| target.to_string()),
        }
    }

    fn moved(&self, location: &str) -> Response {
        let body = format!("<p>This page has moved: {}</p>", self.anchor(location, location));
        self.render(StatusCode::OK, "Moved", &body)
    }

    fn message(&self, status: StatusCode, what: &str, detail: &str) -> Response {
        let detail =
            if detail.is_empty() { String::new() } else { format!("<p class=\"reader-note\">{}</p>", esc(detail)) };
        self.render(status, what, &format!("<h1>{}</h1>{detail}", esc(what)))
    }

    fn render(&self, status: StatusCode, title: &str, body: &str) -> Response {
        let (badge, address) = match &self.source {
            Some(source) => {
                let network = source.network();
                let name = if network == Network::Gemini { "Gemini" } else { "Gopher" };
                (format!("<span class=\"badge net-{network}\">{name}</span>"), esc(&source.to_string()))
            }
            None => (String::new(), String::new()),
        };
        let home = if self.embedded { "/?embedded" } else { "/" };
        let class = if self.embedded { " class=\"embedded\"" } else { "" };
        let title = if title.is_empty() { address.clone() } else { esc(title) };
        let html = format!(
            "<!doctype html><html lang=\"en\"{class}><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\
             <meta name=\"referrer\" content=\"no-referrer\"><meta name=\"color-scheme\" content=\"dark light\">\
             <title>{title} · OMNId</title><link rel=\"icon\" href=\"/icon.png\"><link rel=\"stylesheet\" href=\"/app.css\">\
             </head><body><header class=\"top\"><a class=\"brand\" href=\"{home}\">{BRAND_MARK}OMNId</a></header>\
             <main class=\"reader\"><div class=\"address-row\">{badge}<span class=\"addr\">{address}</span></div>\
             <article class=\"capsule\">{body}</article></main></body></html>"
        );
        let mut response = (status, html).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        response
    }
}

/// The reader's own address for `url`.
fn reader_href(url: &str, embedded: bool) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
    format!("/read?url={encoded}{}", if embedded { "&embedded" } else { "" })
}

/// Text for HTML, in an element or a quoted attribute.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    fn page(url: &str, embedded: bool) -> Page {
        Page { source: Some(Source::parse(url).unwrap()), embedded }
    }

    #[test]
    fn shows_a_page_s_lines_as_escaped_text() {
        let html = page("gemini://a.example/", false).gemtext(
            "# <b>Log</b>\n<script>alert(1)</script>\n* one\n* two\n> said\n```art\n<x> & y\n```\n=> /x \"><img src=x>",
        );
        assert!(!html.contains("<script>") && !html.contains("<b>") && !html.contains("<img"), "{html}");
        assert!(html.contains("<h1>&lt;b&gt;Log&lt;/b&gt;</h1>"));
        assert!(html.contains("<ul><li>one</li><li>two</li></ul>"));
        assert!(html.contains("<pre aria-label=\"art\">&lt;x&gt; &amp; y\n</pre>"));
        assert!(html.contains("&quot;&gt;&lt;img src=x&gt;</a>"), "{html}");
        let open = page("gemini://a.example/", false).gemtext("```\nnever closed");
        assert!(open.ends_with("</pre>"));
    }

    #[test]
    fn links_open_where_the_search_page_would_open_them() {
        let reader = page("gemini://a.example/log/", false);
        assert_eq!(reader.href("post.gmi").unwrap(), "/read?url=gemini%3A%2F%2Fa.example%2Flog%2Fpost.gmi");
        assert_eq!(reader.href("gopher://b.example/1/x").unwrap(), "/read?url=gopher%3A%2F%2Fb.example%2F1%2Fx");
        assert_eq!(reader.href("https://example.com/").unwrap(), "https://example.com/");
        assert_eq!(reader.href(&format!("http://{ONION}/")), None, "an onion only inside the app");
        assert_eq!(reader.href("mailto:a@example.com"), None);
        let inside = page("gemini://a.example/", true);
        assert_eq!(inside.href(&format!("http://{ONION}/")).unwrap(), format!("http://{ONION}/"));
        assert_eq!(inside.href("gemini://a.example/x").unwrap(), "/read?url=gemini%3A%2F%2Fa.example%2Fx&embedded");
        assert!(page("gemini://a.example/", false).anchor(&format!("http://{ONION}/"), "hidden").contains("unopened"));
    }

    #[test]
    fn shows_a_menu_with_its_items_linked() {
        let html = page("gopher://h.example/", false).menu(
            "iWelcome <home>\t\terror.host\t1\r\n1Docs\t/docs\th.example\t70\r\n9Tarball\t/a.tgz\th.example\t70\r\n",
        );
        assert!(html.contains("Welcome &lt;home&gt;"));
        assert!(html.contains("<a href=\"/read?url=gopher%3A%2F%2Fh.example%2F1%2Fdocs\""), "{html}");
        assert!(html.contains("<span class=\"kind\">file</span><span class=\"unopened\">Tarball</span>"));
    }

    #[test]
    fn input_becomes_the_query() {
        let source = Source::parse("gemini://a.example/search?old").unwrap();
        assert_eq!(
            with_input(&source, "lichen & moss").unwrap().to_string(),
            "gemini://a.example/search?lichen%20%26%20moss"
        );
        assert!(with_input(&source, &"x".repeat(1100)).is_none(), "past Gemini's 1024 bytes");
        assert!(with_input(&Source::parse("gopher://a.example/").unwrap(), "x").is_none());
    }
}
