//! What Gemini and Gopher servers answer, and how the crawler's fetch sees
//! it: as an HTTP exchange, so redirects, robots.txt and failures go the way
//! they do for the web.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::BoxError;
use crate::http::{RawResponse, push_capped};

/// A Gemini capsule's or a Gopher hole's answer, as its server gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmallWeb {
    /// A page of `content_type` (Gopher's menus are
    /// [`MENU_TYPE`](crate::GOPHER_MENU_TYPE)); no body when
    /// [`FetchOptions::accept`](crate::FetchOptions::accept) refused the type.
    Page { content_type: String, body: Option<Bytes>, truncated: bool },
    /// Gemini asks for a line of input (status 10, or 11 for one not to be
    /// shown), sent back as the URL's query.
    Input { prompt: String, sensitive: bool },
    /// Elsewhere, as written: relative to the page asked for.
    Redirect { location: String, permanent: bool },
    /// Gemini's status and the server's message: 4x is temporary, 5x is
    /// not (51 is "not found"), 6x asks for a client certificate. A Gopher
    /// error item is a 51.
    Failed { status: u8, message: String },
}

impl SmallWeb {
    /// The HTTP status and fields the crawler's fetch goes by. A page that
    /// asks for input or a certificate answered, with nothing to index: a
    /// 204 with no body.
    pub(crate) fn into_raw(self) -> RawResponse {
        let raw = |status| RawResponse {
            status,
            content_type: None,
            location: None,
            ipfs_gateway: false,
            body: None,
            truncated: false,
        };
        match self {
            SmallWeb::Page { content_type, body, truncated } => {
                RawResponse { content_type: Some(content_type), body, truncated, ..raw(200) }
            }
            SmallWeb::Input { .. } => raw(204),
            SmallWeb::Redirect { location, permanent } => {
                RawResponse { location: Some(location), ..raw(if permanent { 301 } else { 302 }) }
            }
            SmallWeb::Failed { status, .. } => raw(match status {
                44 => 429,
                40..=49 => 503,
                51 => 404,
                52 => 410,
                53 => 403,
                60..=69 => 204,
                _ => 400,
            }),
        }
    }
}

/// Reads to the end of `stream`, keeping at most `max` bytes. A TLS
/// connection the server closed without saying so (no `close_notify`, as
/// many Gemini servers do) ends the body like any close.
pub(crate) async fn read_capped<S>(stream: &mut S, max: usize) -> Result<(Bytes, bool), BoxError>
where
    S: AsyncRead + Unpin,
{
    let mut body = BytesMut::new();
    let mut chunk = vec![0; 16 << 10];
    loop {
        let read = match stream.read(&mut chunk).await {
            Ok(0) => return Ok((body.freeze(), false)),
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok((body.freeze(), false)),
            Err(e) => return Err(e.into()),
        };
        if push_capped(&mut body, &chunk[..read], max) {
            return Ok((body.freeze(), true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_answers_to_http() {
        let failed = |status| SmallWeb::Failed { status, message: String::new() }.into_raw().status;
        assert_eq!(failed(51), 404);
        assert_eq!(failed(52), 410);
        assert_eq!(failed(44), 429);
        assert_eq!(failed(41), 503, "temporary");
        assert_eq!(failed(59), 400);
        assert_eq!(failed(60), 204, "a page for certificate holders: answered, nothing to index");
        let input = SmallWeb::Input { prompt: "Search".into(), sensitive: false }.into_raw();
        assert_eq!((input.status, input.body.is_none()), (204, true));
        let moved = SmallWeb::Redirect { location: "/new".into(), permanent: true }.into_raw();
        assert_eq!((moved.status, moved.location.as_deref()), (301, Some("/new")));
    }

    #[tokio::test]
    async fn reads_bodies_up_to_the_cap() {
        let mut short: &[u8] = b"hello";
        assert_eq!(read_capped(&mut short, 10).await.unwrap(), (Bytes::from_static(b"hello"), false));
        let mut long: &[u8] = b"hello world";
        assert_eq!(read_capped(&mut long, 5).await.unwrap(), (Bytes::from_static(b"hello"), true));
    }
}
