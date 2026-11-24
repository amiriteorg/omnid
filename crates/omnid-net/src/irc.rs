//! IRC, for one thing: a network's list of channels (LIST), which any client
//! may ask for. omnid registers under a nick of its own, says it is a bot
//! and whose, asks for the channels with enough people in them, and leaves.
//! It never joins a channel, so it never sees what is said in one.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::BoxError;

/// Channels with fewer people than this are not listed: a channel of two or
/// three is a conversation more than a place, and most networks hide the
/// smallest anyway.
pub const MIN_USERS: u32 = 10;
/// The largest channels kept from one network's list.
pub const MAX_CHANNELS: usize = 2_000;
/// A line, tags included, is at most 8,191 + 512 bytes (IRCv3).
const MAX_LINE: usize = 9 << 10;
/// Libera.Chat lists about 25,000 channels; a list longer than this is not
/// read to its end.
const MAX_LINES: usize = 200_000;
const MAX_TOPIC_BYTES: usize = 1_000;
/// What omnid says it is, in its USER line: a bot, labelled, and what for.
const REALNAME: &str = "omnid search crawler: lists channels, joins none (https://github.com/amiriteorg/omnid)";

/// A channel as LIST gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcChannel {
    /// As the server wrote it, prefix included.
    pub name: String,
    pub users: u32,
    /// Without colour and formatting codes, or the channel modes some
    /// servers put first (`[+nt]`).
    pub topic: String,
}

/// A network's channels, the largest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IrcListing {
    /// The name the server gives its network (ISUPPORT `NETWORK=`).
    pub network: Option<String>,
    pub channels: Vec<IrcChannel>,
    /// Channels the list had with at least [`MIN_USERS`] people, before the
    /// largest 2,000 were kept.
    pub listed: usize,
}

/// Registers on the server at the other end of `stream`, asks for its
/// channels and quits. `nick` is tried first, then with digits after it.
pub(crate) async fn list<S>(mut stream: S, nick: &str) -> Result<IrcListing, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut nick = nick.to_owned();
    send(&mut stream, &format!("NICK {nick}\r\nUSER omnid 0 * :{REALNAME}\r\n")).await?;
    let mut reader = Lines::default();
    let mut listing = IrcListing::default();
    let (mut elist, mut bot_mode, mut registered, mut asked) = (false, None::<String>, false, false);
    let mut tries = 0;
    for _ in 0..MAX_LINES {
        let Some(line) = reader.next(&mut stream).await? else {
            return Err(if asked { "the server closed before the list ended" } else { "the server closed" }.into());
        };
        let message = Message::parse(&line);
        match message.command {
            "PING" => send(&mut stream, &format!("PONG :{}\r\n", message.params.last().copied().unwrap_or(""))).await?,
            "ERROR" => return Err(format!("the server said: {}", message.params.last().copied().unwrap_or("")).into()),
            "001" => registered = true,
            "005" => {
                for token in message.params.iter().skip(1) {
                    match token.split_once('=') {
                        Some(("NETWORK", name)) if !name.is_empty() => listing.network = Some(name.to_owned()),
                        Some(("ELIST", modes)) => elist = modes.contains(['U', 'u']),
                        Some(("BOT", mode)) if !mode.is_empty() => bot_mode = Some(mode.to_owned()),
                        _ => {}
                    }
                }
            }
            // Nick taken, or refused: another, a few times.
            "432" | "433" | "436" if !registered && tries < 3 => {
                tries += 1;
                nick = format!(
                    "{}{}",
                    nick.trim_end_matches(|c: char| c.is_ascii_digit()),
                    rand::random::<u16>() % 10_000
                );
                send(&mut stream, &format!("NICK {nick}\r\n")).await?;
            }
            // The end of the message of the day: registration is done.
            "376" | "422" if registered && !asked => {
                if let Some(mode) = &bot_mode {
                    send(&mut stream, &format!("MODE {nick} +{mode}\r\n")).await?;
                }
                let filter = if elist { format!(" >{}", MIN_USERS - 1) } else { String::new() };
                send(&mut stream, &format!("LIST{filter}\r\n")).await?;
                asked = true;
            }
            "322" if asked => {
                if let Some(channel) = channel(&message.params)
                    && channel.users >= MIN_USERS
                {
                    listing.listed += 1;
                    listing.channels.push(channel);
                    if listing.channels.len() >= 2 * MAX_CHANNELS {
                        keep_largest(&mut listing.channels);
                    }
                }
            }
            "323" if asked => {
                let _ = send(&mut stream, "QUIT :listed\r\n").await;
                keep_largest(&mut listing.channels);
                return Ok(listing);
            }
            // LIST is paced (Solanum answers RPL_LOAD2HI) or refused.
            "263" | "481" if asked => {
                let _ = send(&mut stream, "QUIT\r\n").await;
                return Err(
                    format!("the server would not list now: {}", message.params.last().copied().unwrap_or("")).into()
                );
            }
            "432" | "433" | "436" | "465" | "466" if !registered => {
                return Err(
                    format!("the server refused omnid: {}", message.params.last().copied().unwrap_or("")).into()
                );
            }
            _ => {}
        }
    }
    Err("a list longer than omnid reads".into())
}

/// The largest channels first, at most [`MAX_CHANNELS`].
fn keep_largest(channels: &mut Vec<IrcChannel>) {
    channels.sort_by(|a, b| b.users.cmp(&a.users).then_with(|| a.name.cmp(&b.name)));
    channels.truncate(MAX_CHANNELS);
}

/// A 322 reply's channel: `<client> <channel> <users> :<topic>`.
fn channel(params: &[&str]) -> Option<IrcChannel> {
    let [_, name, users, rest @ ..] = params else { return None };
    let users = users.parse().ok()?;
    let topic = strip_formatting(rest.last().copied().unwrap_or(""));
    // Solanum and others put the channel's modes first: `[+nt] topic`.
    let topic = match topic.strip_prefix("[+") {
        Some(rest) => rest.split_once("] ").map_or("", |(_, topic)| topic).to_owned(),
        None => topic,
    };
    let mut topic = topic.trim().to_owned();
    if topic.len() > MAX_TOPIC_BYTES {
        let mut end = MAX_TOPIC_BYTES;
        while !topic.is_char_boundary(end) {
            end -= 1;
        }
        topic.truncate(end);
    }
    Some(IrcChannel { name: (*name).to_owned(), users, topic })
}

/// Text without IRC's formatting codes: bold, colours (`\x03` with its
/// numbers, `\x04` with hex), italics, underline, reverse and reset.
pub(crate) fn strip_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A colour: up to two digits, and a background (`,` and up to
            // two more) only after a foreground.
            '\x03' => {
                let foreground = (0..2).filter(|_| chars.next_if(char::is_ascii_digit).is_some()).count();
                if foreground > 0 && chars.peek() == Some(&',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                        for _ in 0..2 {
                            chars.next_if(char::is_ascii_digit);
                        }
                    }
                }
            }
            '\x04' => {
                for _ in 0..6 {
                    chars.next_if(char::is_ascii_hexdigit);
                }
            }
            '\x02' | '\x0f' | '\x11' | '\x16' | '\x1d' | '\x1e' | '\x1f' => {}
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// One IRC message: its command and parameters, the trailing one included.
/// Tags and the prefix are read past.
#[derive(Debug, PartialEq, Eq)]
struct Message<'a> {
    command: &'a str,
    params: Vec<&'a str>,
}

impl<'a> Message<'a> {
    fn parse(line: &'a str) -> Self {
        let mut rest = line.trim_end_matches(['\r', '\n']);
        if rest.starts_with('@') {
            rest = rest.split_once(' ').map_or("", |(_, r)| r).trim_start();
        }
        if rest.starts_with(':') {
            rest = rest.split_once(' ').map_or("", |(_, r)| r).trim_start();
        }
        let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut params = Vec::new();
        while !rest.is_empty() && params.len() < 15 {
            rest = rest.trim_start_matches(' ');
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing);
                break;
            }
            let (param, next) = rest.split_once(' ').unwrap_or((rest, ""));
            if !param.is_empty() {
                params.push(param);
            }
            rest = next;
        }
        Message { command, params }
    }
}

/// Lines from a stream, each at most [`MAX_LINE`] bytes, as text (a line
/// that is not UTF-8 read as Latin-1, as older clients wrote).
#[derive(Default)]
struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    async fn next<S: AsyncRead + Unpin>(&mut self, stream: &mut S) -> Result<Option<String>, BoxError> {
        loop {
            if let Some(end) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=end).collect();
                return Ok(Some(match String::from_utf8(line) {
                    Ok(text) => text,
                    Err(e) => e.into_bytes().iter().map(|b| *b as char).collect(),
                }));
            }
            if self.buf.len() > MAX_LINE {
                return Err("an IRC line longer than 9 KiB".into());
            }
            let mut chunk = [0u8; 4096];
            let read = match stream.read(&mut chunk).await {
                Ok(read) => read,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
                Err(e) => return Err(e.into()),
            };
            if read == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, text: &str) -> Result<(), BoxError> {
    stream.write_all(text.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(fuzzing)]
pub(crate) mod fuzz {
    /// IRC's message and LIST reply parsers, for `fuzz/fuzz_targets/irc_lines.rs`.
    pub fn lines(data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        for line in text.split('\n') {
            let message = super::Message::parse(line);
            assert!(message.params.len() <= 15);
            if let Some(channel) = super::channel(&message.params) {
                assert!(channel.topic.len() <= super::MAX_TOPIC_BYTES);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages() {
        let m = Message::parse("@time=2026-11-24T10:00:00Z :irc.example 322 omnid #rust 1234 :[+nt] Rust! \r\n");
        assert_eq!((m.command, m.params.as_slice()), ("322", &["omnid", "#rust", "1234", "[+nt] Rust! "][..]));
        assert_eq!(Message::parse("PING :token").params, ["token"]);
        let isupport = Message::parse(":s 005 omnid NETWORK=Libera.Chat ELIST=CMNTU :are supported");
        assert_eq!(isupport.params[1..3], ["NETWORK=Libera.Chat", "ELIST=CMNTU"]);
        assert_eq!(Message::parse("").command, "");
        assert!(Message::parse(&"a ".repeat(100)).params.len() <= 15);
    }

    #[test]
    fn reads_channels_without_formatting_or_modes() {
        let m = Message::parse(":s 322 me #c 42 :[+Cnt] \x02Bold\x02 \x0304,12red\x03 and \x1Ditalic\x1D");
        assert_eq!(
            channel(&m.params).unwrap(),
            IrcChannel { name: "#c".into(), users: 42, topic: "Bold red and italic".into() }
        );
        assert_eq!(channel(&Message::parse(":s 322 me #c x :t").params), None, "a count that is no number");
        assert_eq!(strip_formatting("\x03,5x\x0399y\x04ff00ffz"), ",5xyz");
    }

    /// A server on the other end of an in-memory connection: it sends
    /// `script` at once, closes its side, and returns what the client said.
    /// The client reads it in order, so it answers each line in turn.
    async fn serve(script: &'static str) -> (Result<IrcListing, String>, String) {
        let (client, mut server) = tokio::io::duplex(1 << 20);
        let talk = tokio::spawn(async move {
            server.write_all(script.as_bytes()).await.unwrap();
            server.shutdown().await.unwrap();
            let mut heard = Vec::new();
            let _ = server.read_to_end(&mut heard).await;
            String::from_utf8_lossy(&heard).into_owned()
        });
        let listing = list(client, "omnid1234").await.map_err(|e| e.to_string());
        (listing, talk.await.unwrap())
    }

    #[tokio::test]
    async fn registers_lists_and_quits() {
        let (listing, heard) = serve(concat!(
            ":s 001 omnid1234 :Welcome\r\n",
            ":s 005 omnid1234 NETWORK=Testnet ELIST=MNU BOT=B :are supported\r\n",
            "PING :abc\r\n",
            ":s 376 omnid1234 :End of MOTD\r\n",
            ":s 321 omnid1234 Channel :Users Name\r\n",
            ":s 322 omnid1234 #small 3 :few\r\n",
            ":s 322 omnid1234 #big 900 :[+nt] Big \x02one\r\n",
            ":s 322 omnid1234 #mid 12 :\r\n",
            ":s 323 omnid1234 :End of /LIST\r\n",
        ))
        .await;
        let listing = listing.unwrap();
        assert_eq!(listing.network.as_deref(), Some("Testnet"));
        assert_eq!(listing.channels.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["#big", "#mid"]);
        assert_eq!(listing.channels[0].topic, "Big one");
        assert_eq!(listing.listed, 2, "#small has fewer than ten");
        assert!(heard.starts_with("NICK omnid1234\r\nUSER omnid 0 * :omnid search crawler"), "{heard}");
        assert!(heard.contains("PONG :abc") && heard.contains("MODE omnid1234 +B"), "{heard}");
        assert!(heard.contains("LIST >9\r\n") && heard.contains("QUIT"), "{heard}");
        assert!(!heard.contains("JOIN"), "never a channel");
    }

    #[tokio::test]
    async fn takes_another_nick_and_lists_without_elist() {
        let (listing, heard) = serve(concat!(
            ":s 433 * omnid1234 :Nickname is already in use\r\n",
            ":s 001 omnid42 :Welcome\r\n",
            ":s 422 omnid42 :MOTD File is missing\r\n",
            ":s 322 omnid42 #a 10 :t\r\n",
            ":s 323 omnid42 :End\r\n",
        ))
        .await;
        assert_eq!(listing.unwrap().channels.len(), 1);
        assert_eq!(heard.matches("NICK omnid").count(), 2, "{heard}");
        assert!(heard.contains("LIST\r\n"), "no ELIST: the whole list, filtered here");
    }

    #[tokio::test]
    async fn says_why_it_got_no_list() {
        let (paced, _) = serve(concat!(
            ":s 001 omnid1234 :Welcome\r\n:s 422 omnid1234 :No MOTD\r\n",
            ":s 263 omnid1234 LIST :Server load is temporarily too heavy\r\n",
        ))
        .await;
        assert!(paced.unwrap_err().contains("too heavy"));
        let (banned, _) = serve(":s 001 x :hi\r\nERROR :Closing Link: (Tor exit)\r\n").await;
        assert!(banned.unwrap_err().contains("Tor exit"));
        let (closed, _) = serve("").await;
        assert!(closed.unwrap_err().contains("closed"));
    }

    #[tokio::test]
    async fn an_endless_line_is_refused() {
        let (client, mut server) = tokio::io::duplex(64 << 10);
        tokio::spawn(async move {
            let _ = server.write_all(&vec![b'a'; 20 << 10]).await;
        });
        assert!(list(client, "omnid1").await.unwrap_err().to_string().contains("longer than 9 KiB"));
    }
}
