//! Gemini's pages (text/gemini) and Gopher's menus, line by line: for the
//! extractor, and for the reader that shows them. Both formats are lines of
//! text, so they cost time and memory in proportion to the page, which the
//! fetch has already capped.

use std::fmt::Write as _;

use crate::extract::{Extracted, MAX_TEXT_BYTES, collapse, truncate};

/// Gemini's own format, its servers' default.
pub const GEMTEXT_TYPE: &str = "text/gemini";
/// Gopher menus, as the gateway labels them (`omnid_net::GOPHER_MENU_TYPE`).
pub const GOPHER_MENU_TYPE: &str = "text/x-gopher-menu";

const MAX_TITLE_BYTES: usize = 300;
const DESCRIPTION_BYTES: usize = 280;
/// As many as an HTML page may give.
const MAX_LINKS: usize = 20_000;

/// One line of a text/gemini page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GemLine<'a> {
    Text(&'a str),
    /// `=> URL label`; the URL as written, perhaps relative.
    Link {
        url: &'a str,
        label: &'a str,
    },
    Heading {
        level: u8,
        text: &'a str,
    },
    /// `* item`
    Item(&'a str),
    /// `> quote`
    Quote(&'a str),
    /// A line between ```` ``` ```` toggles, kept as it is.
    Preformatted(&'a str),
    /// A ```` ``` ```` line, opening (with its alt text) or closing a
    /// preformatted block.
    Toggle {
        opens: bool,
        alt: &'a str,
    },
}

/// The lines of a text/gemini page.
pub fn gemtext(page: &str) -> impl Iterator<Item = GemLine<'_>> {
    let mut preformatted = false;
    page.lines().map(move |line| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(alt) = line.strip_prefix("```") {
            preformatted = !preformatted;
            return GemLine::Toggle { opens: preformatted, alt: alt.trim() };
        }
        if preformatted {
            return GemLine::Preformatted(line);
        }
        if let Some(rest) = line.strip_prefix("=>") {
            let rest = rest.trim_start();
            let (url, label) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if !url.is_empty() {
                return GemLine::Link { url, label: label.trim() };
            }
            return GemLine::Text(line);
        }
        for (level, marker) in [(3, "###"), (2, "##"), (1, "#")] {
            if let Some(text) = line.strip_prefix(marker) {
                return GemLine::Heading { level, text: text.trim() };
            }
        }
        if let Some(item) = line.strip_prefix("* ") {
            return GemLine::Item(item.trim());
        }
        if let Some(quote) = line.strip_prefix('>') {
            return GemLine::Quote(quote.trim());
        }
        GemLine::Text(line)
    })
}

/// One line of a Gopher menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItem<'a> {
    /// `1` a menu, `0` text, `h` HTML, `i` a line of text with no item...
    pub item_type: char,
    pub display: &'a str,
    pub selector: &'a str,
    pub host: &'a str,
    pub port: u16,
}

impl MenuItem<'_> {
    /// The item's address: a `gopher://` URL, or the web address of an `h`
    /// item whose selector is `URL:<address>`. `None` for lines with no item
    /// (`i`, errors) and items that are no place to go: searches, telnet
    /// sessions, phone books.
    pub fn url(&self) -> Option<String> {
        if matches!(self.item_type, 'i' | '3' | '7' | '8' | 'T' | '2') || self.host.is_empty() {
            return None;
        }
        if self.item_type == 'h'
            && let Some(web) = self.selector.strip_prefix("URL:")
        {
            return Some(web.to_owned());
        }
        let mut url = format!("gopher://{}", self.host);
        if self.port != 70 {
            let _ = write!(url, ":{}", self.port);
        }
        url.push('/');
        url.push(self.item_type);
        for byte in self.selector.bytes() {
            if byte.is_ascii_graphic() && !matches!(byte, b'%' | b'#' | b'"' | b'<' | b'>' | b'`' | b'{' | b'}') {
                url.push(byte as char);
            } else {
                let _ = write!(url, "%{byte:02X}");
            }
        }
        Some(url)
    }
}

/// The lines of a Gopher menu, up to its closing `.`. A line without the
/// tab-separated fields of an item is shown as text, as clients do.
pub fn gopher_menu(menu: &str) -> impl Iterator<Item = MenuItem<'_>> {
    menu.lines().map(|line| line.strip_suffix('\r').unwrap_or(line)).take_while(|line| *line != ".").filter_map(
        |line| {
            let mut chars = line.chars();
            let item_type = chars.next()?;
            let mut fields = chars.as_str().split('\t');
            let (display, selector, host, port) = (fields.next()?, fields.next(), fields.next(), fields.next());
            Some(match (selector, host, port) {
                (Some(selector), Some(host), Some(port)) => MenuItem {
                    item_type,
                    display,
                    selector,
                    host: host.trim(),
                    port: port.trim().parse().unwrap_or(70),
                },
                _ => MenuItem { item_type: 'i', display: line, selector: "", host: "", port: 0 },
            })
        },
    )
}

/// A text/gemini page's title (its first heading, else its first line of
/// text), text, description (its first paragraph), links and language
/// (the content type's `lang`).
pub(crate) fn extract_gemtext(content_type: Option<&str>, page: &str) -> Extracted {
    let mut out = Extracted { lang: content_type.and_then(lang_param), ..Default::default() };
    let mut text = String::new();
    let mut first_text = None;
    let mut heading = None;
    for line in gemtext(page) {
        let words = match line {
            GemLine::Link { url, label } => {
                if out.links.len() < MAX_LINKS {
                    out.links.push(url.to_owned());
                }
                label
            }
            GemLine::Heading { level, text } => {
                if heading.is_none_or(|(best, _)| level < best) && !text.is_empty() {
                    heading = Some((level, text));
                }
                text
            }
            GemLine::Text(line) | GemLine::Item(line) | GemLine::Quote(line) => {
                if first_text.is_none() && !line.trim().is_empty() {
                    first_text = Some(line);
                }
                line
            }
            GemLine::Preformatted(line) => line,
            GemLine::Toggle { alt, .. } => alt,
        };
        if text.len() < MAX_TEXT_BYTES {
            text.push_str(words);
            text.push('\n');
        }
    }
    out.title = collapse(heading.map(|(_, text)| text).or(first_text).unwrap_or_default(), MAX_TITLE_BYTES);
    out.description = collapse(first_text.unwrap_or_default(), DESCRIPTION_BYTES);
    out.text = collapse(&text, MAX_TEXT_BYTES);
    if out.description.is_empty() {
        out.description = truncate(&out.text, DESCRIPTION_BYTES).to_owned();
    }
    out
}

/// A Gopher menu's title (its first line of text with words in it), text
/// (every line's), description and links: its text, menu and HTML items.
pub(crate) fn extract_menu(menu: &str) -> Extracted {
    let mut out = Extracted::default();
    let mut text = String::new();
    let mut info = String::new();
    for item in gopher_menu(menu) {
        if item.item_type == 'i' {
            let line = item.display.trim();
            if out.title.is_empty() && line.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
                out.title = collapse(line, MAX_TITLE_BYTES);
            }
            if info.len() < DESCRIPTION_BYTES {
                info.push_str(line);
                info.push(' ');
            }
        } else if matches!(item.item_type, '0' | '1' | 'h')
            && out.links.len() < MAX_LINKS
            && let Some(url) = item.url()
        {
            out.links.push(url);
        }
        if text.len() < MAX_TEXT_BYTES {
            text.push_str(item.display);
            text.push('\n');
        }
    }
    out.text = collapse(&text, MAX_TEXT_BYTES);
    out.description = collapse(&info, DESCRIPTION_BYTES);
    if out.description.is_empty() {
        out.description = truncate(&out.text, DESCRIPTION_BYTES).to_owned();
    }
    out
}

/// A content type's `lang` parameter's primary subtag (`text/gemini;
/// lang=en-GB` is `en`).
fn lang_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|param| {
        let (name, value) = param.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("lang") {
            return None;
        }
        let primary = value.trim().trim_matches('"').split([',', '-', '_']).next()?.to_ascii_lowercase();
        (2..=3).contains(&primary.len()).then_some(primary).filter(|p| p.bytes().all(|b| b.is_ascii_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "# Gemlog\r\n\
        Notes from a small capsule.\n\
        \n\
        => /log/1.gmi First post\n\
        =>gemini://b.example/\n\
        => \n\
        ## Older\n\
        * an item\n\
        > a quote\n\
        ```ascii art\n\
        # not a heading\n\
        => not/a/link\n\
        ```\n";

    #[test]
    fn reads_gemtext_lines() {
        let lines: Vec<_> = gemtext(PAGE).collect();
        assert_eq!(lines[0], GemLine::Heading { level: 1, text: "Gemlog" });
        assert_eq!(lines[3], GemLine::Link { url: "/log/1.gmi", label: "First post" });
        assert_eq!(lines[4], GemLine::Link { url: "gemini://b.example/", label: "" });
        assert_eq!(lines[5], GemLine::Text("=> "), "a link line with no URL is text");
        assert_eq!(lines[6], GemLine::Heading { level: 2, text: "Older" });
        assert_eq!(lines[7], GemLine::Item("an item"));
        assert_eq!(lines[8], GemLine::Quote("a quote"));
        assert_eq!(lines[9], GemLine::Toggle { opens: true, alt: "ascii art" });
        assert_eq!(lines[10], GemLine::Preformatted("# not a heading"));
        assert_eq!(lines[11], GemLine::Preformatted("=> not/a/link"));
        assert_eq!(lines[12], GemLine::Toggle { opens: false, alt: "" });
    }

    #[test]
    fn extracts_a_gemini_page() {
        let page = extract_gemtext(Some("text/gemini; lang=en-GB"), PAGE);
        assert_eq!(page.title, "Gemlog");
        assert_eq!(page.description, "Notes from a small capsule.");
        assert_eq!(page.links, ["/log/1.gmi", "gemini://b.example/"]);
        assert_eq!(page.lang.as_deref(), Some("en"));
        assert!(page.text.contains("First post") && page.text.contains("a quote"));
        let untitled = extract_gemtext(None, "just text\n=> /x");
        assert_eq!((untitled.title.as_str(), untitled.lang), ("just text", None));
    }

    const MENU: &str = "i   ___  \t\terror.host\t1\r\n\
        iWelcome to the hole\t\terror.host\t1\r\n\
        1Phlog\t/phlog/\tgopher.example\t70\r\n\
        0About this server\t/about.txt\tgopher.example\t7070\r\n\
        9A binary\t/file.zip\tgopher.example\t70\r\n\
        hThe web site\tURL:https://example.com/\tgopher.example\t70\r\n\
        7Search\t/search\tgopher.example\t70\r\n\
        just a line\r\n\
        .\r\n\
        1After the end\t/x\tgopher.example\t70\r\n";

    #[test]
    fn reads_gopher_menus() {
        let items: Vec<_> = gopher_menu(MENU).collect();
        assert_eq!(items.len(), 8, "up to the closing dot");
        assert_eq!(items[2].url().unwrap(), "gopher://gopher.example/1/phlog/");
        assert_eq!(items[3].url().unwrap(), "gopher://gopher.example:7070/0/about.txt");
        assert_eq!(items[5].url().unwrap(), "https://example.com/");
        assert_eq!(items[6].url(), None, "a search");
        assert_eq!((items[7].item_type, items[7].display), ('i', "just a line"));
        let spaced = MenuItem { item_type: '0', display: "", selector: "/a b#c%", host: "h", port: 70 };
        assert_eq!(spaced.url().unwrap(), "gopher://h/0/a%20b%23c%25");
    }

    #[test]
    fn extracts_a_gopher_menu() {
        let page = extract_menu(MENU);
        assert_eq!(page.title, "Welcome to the hole", "not the ASCII art");
        assert_eq!(
            page.links,
            ["gopher://gopher.example/1/phlog/", "gopher://gopher.example:7070/0/about.txt", "https://example.com/"],
            "text, menus and the web; not binaries or searches"
        );
        assert!(page.text.contains("A binary"), "every item's name is text");
    }
}
