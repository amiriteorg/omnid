//! Gopher (RFC 1436): the selector as one line, then the item until the
//! server closes the connection. Only text is asked for: menus, text files
//! and HTML; the item type says which before anything is sent.

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{BoxError, FetchOptions, SmallWeb, small_web};

/// The content type a menu (item type `1`) is given here: Gopher has none.
pub const MENU_TYPE: &str = "text/x-gopher-menu";
/// Selectors are short paths; a longer one is not a server's.
const MAX_SELECTOR: usize = 1024;

/// The content type of a Gopher item type that is text, `None` for others
/// (binaries, images, searches, telnet sessions), which are never fetched.
pub fn content_type(item_type: char) -> Option<&'static str> {
    match item_type {
        '1' => Some(MENU_TYPE),
        '0' => Some("text/plain"),
        'h' => Some("text/html"),
        _ => None,
    }
}

/// Asks a Gopher server for the item `selector` of type `item_type` over
/// `stream`, which reaches its host.
pub(crate) async fn get<S>(
    mut stream: S,
    item_type: char,
    selector: &[u8],
    opts: &FetchOptions,
) -> Result<SmallWeb, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let content_type = content_type(item_type).ok_or_else(|| format!("Gopher item type {item_type:?} is not text"))?;
    if selector.len() > MAX_SELECTOR || selector.iter().any(|b| matches!(b, b'\t' | b'\r' | b'\n')) {
        return Err("not a Gopher selector".into());
    }
    stream.write_all(selector).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    let (body, truncated) = small_web::read_capped(&mut stream, opts.max_bytes).await?;
    if let Some(message) = error_item(&body) {
        return Ok(SmallWeb::Failed { status: 51, message });
    }
    Ok(SmallWeb::Page { content_type: content_type.to_owned(), body: Some(body), truncated })
}

/// A server that has no such item answers with a menu of one error item
/// (type `3`), whatever type was asked for: its text, if this is one.
pub(crate) fn error_item(body: &[u8]) -> Option<String> {
    let line = body.split(|b| *b == b'\n').next()?;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let rest = line.strip_prefix(b"3")?;
    let fields: Vec<&[u8]> = rest.split(|b| *b == b'\t').collect();
    (fields.len() >= 3).then(|| String::from_utf8_lossy(fields[0]).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_text_items_have_a_type() {
        assert_eq!(content_type('1'), Some(MENU_TYPE));
        assert_eq!(content_type('0'), Some("text/plain"));
        for binary in ['9', 'g', 'I', 's', '5', '7', '8', 'T', ';', 'd', 'P'] {
            assert_eq!(content_type(binary), None, "{binary}");
        }
    }

    #[test]
    fn finds_error_items() {
        assert_eq!(
            error_item(b"3'/nope' does not exist\terror.host\t1\r\n.\r\n"),
            Some("'/nope' does not exist".into())
        );
        assert_eq!(error_item(b"3 no tabs here\r\n"), None, "a text that starts with 3");
        assert_eq!(error_item(b"iWelcome\t\terror.host\t1\r\n"), None);
        assert_eq!(error_item(b""), None);
    }
}
