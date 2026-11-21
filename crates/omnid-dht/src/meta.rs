//! Info dicts: the hash that names them, and the parts of BEP 3 and BEP 47
//! the index shows. The hash is taken over the bytes as received, never a
//! re-encoding, so a non-canonical encoding still verifies.

use omnid_core::FileMeta;
use sha1::{Digest, Sha1};

use crate::TorrentMeta;
use crate::bencode::{self, Limits, Value};

pub(crate) fn info_hash(info: &[u8]) -> [u8; 20] {
    Sha1::digest(info).into()
}

/// Reads name, files and sizes from a verified info dict. Multi-file paths
/// are relative to the torrent's root directory, which is `name`, and
/// joined with `/`. `peers` is left at zero for the caller.
pub(crate) fn parse_info(info: &[u8]) -> Result<TorrentMeta, String> {
    let root = bencode::decode(info, Limits::INFO).map_err(|e| format!("info dict: {e}"))?;
    if !matches!(root, Value::Dict(_)) {
        return Err("info is not a dictionary".into());
    }
    let name = text(&root, b"name", b"name.utf-8").ok_or("no name")?;
    let piece_length = root
        .get(b"piece length")
        .and_then(Value::as_int)
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or("no piece length")?;
    let private = root.get(b"private").and_then(Value::as_int) == Some(1);
    let files = match (root.get(b"length"), root.get(b"files")) {
        (Some(length), _) => vec![FileMeta { path: name.clone(), size: size(length)? }],
        (None, Some(Value::List(entries))) => {
            let mut files = Vec::with_capacity(entries.len());
            for entry in entries {
                // BEP 47 padding files align pieces; they are not content.
                if entry.get(b"attr").and_then(Value::as_bytes).is_some_and(|attr| attr.contains(&b'p')) {
                    continue;
                }
                let size = size(entry.get(b"length").ok_or("file without a length")?)?;
                files.push(FileMeta { path: path(entry)?, size });
            }
            files
        }
        _ => return Err("neither length nor files".into()),
    };
    let total_size = files.iter().try_fold(0u64, |total, f| total.checked_add(f.size)).ok_or("total size overflows")?;
    Ok(TorrentMeta { name, files, total_size, piece_length, private, peers: 0 })
}

fn size(value: &Value<'_>) -> Result<u64, String> {
    value.as_int().and_then(|n| u64::try_from(n).ok()).ok_or_else(|| "bad file length".into())
}

/// Prefers `utf8_key` when it holds valid UTF-8; older clients wrote names
/// in the uploader's local encoding under the plain key, which is decoded
/// lossily.
fn text(dict: &Value<'_>, key: &[u8], utf8_key: &[u8]) -> Option<String> {
    if let Some(name) = dict.get(utf8_key).and_then(Value::as_bytes).and_then(|b| std::str::from_utf8(b).ok()) {
        return Some(clean(name));
    }
    dict.get(key).and_then(Value::as_bytes).map(|b| clean(&String::from_utf8_lossy(b)))
}

fn path(file: &Value<'_>) -> Result<String, String> {
    let utf8 = components(file, b"path.utf-8").and_then(|parts| {
        parts.into_iter().map(|p| std::str::from_utf8(p).ok().map(clean)).collect::<Option<Vec<_>>>()
    });
    let parts = match utf8 {
        Some(parts) => parts,
        None => components(file, b"path")
            .ok_or("file without a path")?
            .into_iter()
            .map(|p| clean(&String::from_utf8_lossy(p)))
            .collect(),
    };
    Ok(parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/"))
}

fn components<'a>(file: &Value<'a>, key: &[u8]) -> Option<Vec<&'a [u8]>> {
    file.get(key)?.as_list()?.iter().map(Value::as_bytes).collect()
}

/// Names come from strangers and end up in logs, terminals and the UI.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(value: Value<'_>) -> Vec<u8> {
        bencode::encode(&value)
    }

    #[test]
    fn hashes_the_bytes_as_received() {
        // SHA-1("abc"), from FIPS 180.
        assert_eq!(
            info_hash(b"abc"),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2, 0x6c, 0x9c,
                0xd0, 0xd8, 0x9d
            ]
        );
    }

    #[test]
    fn reads_a_single_file_torrent() {
        let info = encode(Value::dict(vec![
            ("length", Value::Int(700 << 20)),
            ("name", Value::Bytes(b"debian-13-amd64-netinst.iso")),
            ("piece length", Value::Int(262144)),
            ("pieces", Value::Bytes(&[0; 40])),
        ]));
        let meta = parse_info(&info).unwrap();
        assert_eq!(meta.name, "debian-13-amd64-netinst.iso");
        assert_eq!(meta.files, vec![FileMeta { path: "debian-13-amd64-netinst.iso".into(), size: 700 << 20 }]);
        assert_eq!((meta.total_size, meta.piece_length, meta.private), (700 << 20, 262144, false));
    }

    #[test]
    fn reads_a_multi_file_torrent() {
        let file = |len: i64, path: &[&'static str]| {
            Value::dict(vec![
                ("length", Value::Int(len)),
                ("path", Value::List(path.iter().map(|p| Value::Bytes(p.as_bytes())).collect())),
            ])
        };
        let info = encode(Value::dict(vec![
            (
                "files",
                Value::List(vec![
                    file(100, &["Sintel.mp4"]),
                    file(5, &["subs", "Sintel.en.srt"]),
                    // BEP 47 padding, not content.
                    Value::dict(vec![
                        ("attr", Value::Bytes(b"p")),
                        ("length", Value::Int(16000)),
                        ("path", Value::List(vec![Value::Bytes(b".pad"), Value::Bytes(b"16000")])),
                    ]),
                    file(7, &["a", "", "b.txt"]),
                ]),
            ),
            ("name", Value::Bytes(b"Sintel")),
            ("piece length", Value::Int(131072)),
            ("pieces", Value::Bytes(&[0; 20])),
            ("private", Value::Int(1)),
        ]));
        let meta = parse_info(&info).unwrap();
        assert_eq!(meta.name, "Sintel");
        let paths: Vec<(&str, u64)> = meta.files.iter().map(|f| (f.path.as_str(), f.size)).collect();
        assert_eq!(paths, vec![("Sintel.mp4", 100), ("subs/Sintel.en.srt", 5), ("a/b.txt", 7)]);
        assert_eq!(meta.total_size, 112);
        assert!(meta.private);
    }

    #[test]
    fn prefers_utf8_keys_and_strips_control_characters() {
        let info = encode(Value::dict(vec![
            (
                "files",
                Value::List(vec![
                    Value::dict(vec![
                        ("length", Value::Int(1)),
                        // Latin-1 "caf\xe9" under the plain key, UTF-8 alongside.
                        ("path", Value::List(vec![Value::Bytes(b"caf\xe9.txt")])),
                        ("path.utf-8", Value::List(vec![Value::Bytes("café.txt".as_bytes())])),
                    ]),
                    Value::dict(vec![
                        ("length", Value::Int(2)),
                        ("path", Value::List(vec![Value::Bytes(b"old\xe9\x1b[31m.txt")])),
                    ]),
                    Value::dict(vec![
                        ("length", Value::Int(3)),
                        ("path", Value::List(vec![Value::Bytes(b"plain.txt")])),
                        // Invalid UTF-8 under the .utf-8 key falls back.
                        ("path.utf-8", Value::List(vec![Value::Bytes(b"\xff\xfe")])),
                    ]),
                ]),
            ),
            ("name", Value::Bytes(b"M\xfcsic\n")),
            ("name.utf-8", Value::Bytes("Müsic\r\n".as_bytes())),
            ("piece length", Value::Int(16384)),
        ]));
        let meta = parse_info(&info).unwrap();
        assert_eq!(meta.name, "Müsic");
        let paths: Vec<&str> = meta.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["café.txt", "old\u{fffd}[31m.txt", "plain.txt"]);

        let lossy = encode(Value::dict(vec![
            ("length", Value::Int(1)),
            ("name", Value::Bytes(b"M\xfcsic\x07")),
            ("piece length", Value::Int(16384)),
        ]));
        assert_eq!(parse_info(&lossy).unwrap().name, "M\u{fffd}sic");
    }

    #[test]
    fn rejects_broken_info_dicts() {
        let cases: Vec<Vec<u8>> = vec![
            b"le".to_vec(),
            b"d4:name1:xe".to_vec(),
            encode(Value::dict(vec![("name", Value::Bytes(b"x")), ("piece length", Value::Int(1))])),
            encode(Value::dict(vec![("length", Value::Int(1)), ("piece length", Value::Int(1))])),
            encode(Value::dict(vec![
                ("length", Value::Int(-5)),
                ("name", Value::Bytes(b"x")),
                ("piece length", Value::Int(1)),
            ])),
            encode(Value::dict(vec![
                ("length", Value::Int(1)),
                ("name", Value::Bytes(b"x")),
                ("piece length", Value::Int(0)),
            ])),
            encode(Value::dict(vec![
                ("files", Value::List(vec![Value::dict(vec![("length", Value::Int(1))])])),
                ("name", Value::Bytes(b"x")),
                ("piece length", Value::Int(1)),
            ])),
            encode(Value::dict(vec![
                (
                    "files",
                    Value::List(vec![
                        Value::dict(vec![
                            ("length", Value::Int(i64::MAX)),
                            ("path", Value::List(vec![Value::Bytes(b"a")])),
                        ]),
                        Value::dict(vec![
                            ("length", Value::Int(i64::MAX)),
                            ("path", Value::List(vec![Value::Bytes(b"b")])),
                        ]),
                        Value::dict(vec![("length", Value::Int(2)), ("path", Value::List(vec![Value::Bytes(b"c")]))]),
                    ]),
                ),
                ("name", Value::Bytes(b"x")),
                ("piece length", Value::Int(1)),
            ])),
        ];
        for info in cases {
            assert!(parse_info(&info).is_err(), "{:?} should not parse", String::from_utf8_lossy(&info));
        }
    }
}
