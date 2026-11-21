//! A small bencode codec. Everything it decodes came off the network, so it
//! never panics, caps nesting depth and the number of values, and borrows
//! strings from the input rather than allocating by length prefix.

/// A decoded value. Strings borrow from the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    /// Sorted by key: decoding sorts and rejects duplicates, and
    /// [`Value::dict`] sorts, so lookups can binary search.
    Dict(Vec<(&'a [u8], Value<'a>)>),
}

impl<'a> Value<'a> {
    /// Builds a dictionary, sorting the entries by key.
    pub(crate) fn dict(entries: Vec<(&'a str, Value<'a>)>) -> Self {
        let mut entries: Vec<_> = entries.into_iter().map(|(k, v)| (k.as_bytes(), v)).collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        Value::Dict(entries)
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        match self {
            Value::Dict(entries) => entries.binary_search_by(|(k, _)| k.cmp(&key)).ok().map(|i| &entries[i].1),
            _ => None,
        }
    }

    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(items) => Some(items.as_slice()),
            _ => None,
        }
    }
}

/// Bounds on what one decode may build. The value count bounds memory: a
/// decoded value takes tens of bytes however few bytes encoded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub depth: usize,
    pub values: usize,
}

impl Limits {
    /// One KRPC datagram. Real messages nest three or four levels deep.
    pub(crate) const KRPC: Limits = Limits { depth: 16, values: 8192 };
    /// An extension message on the peer wire.
    pub(crate) const PEER: Limits = Limits { depth: 16, values: 4096 };
    /// A torrent's info dict. A million values covers well over 100 000 files.
    pub(crate) const INFO: Limits = Limits { depth: 64, values: 1 << 20 };
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("input ends early")]
    Eof,
    #[error("unexpected byte {byte:#04x} at offset {at}")]
    Unexpected { at: usize, byte: u8 },
    #[error("malformed number at offset {0}")]
    BadNumber(usize),
    #[error("nested deeper than {0}")]
    TooDeep(usize),
    #[error("more than {0} values")]
    TooManyValues(usize),
    #[error("duplicate dictionary key")]
    DuplicateKey,
    #[error("{0} bytes after the value")]
    Trailing(usize),
}

/// Decodes exactly one value spanning the whole input.
pub(crate) fn decode(input: &[u8], limits: Limits) -> Result<Value<'_>, DecodeError> {
    let (value, used) = decode_prefix(input, limits)?;
    match input.len() - used {
        0 => Ok(value),
        extra => Err(DecodeError::Trailing(extra)),
    }
}

/// Decodes one value from the start of the input and returns it with the
/// number of bytes it took. ut_metadata pieces follow their header this way.
pub(crate) fn decode_prefix(input: &[u8], limits: Limits) -> Result<(Value<'_>, usize), DecodeError> {
    let mut decoder = Decoder { input, pos: 0, limits, values: 0 };
    let value = decoder.value(0)?;
    Ok((value, decoder.pos))
}

struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    limits: Limits,
    values: usize,
}

impl<'a> Decoder<'a> {
    fn peek(&self) -> Result<u8, DecodeError> {
        self.input.get(self.pos).copied().ok_or(DecodeError::Eof)
    }

    fn count(&mut self) -> Result<(), DecodeError> {
        self.values += 1;
        if self.values > self.limits.values { Err(DecodeError::TooManyValues(self.limits.values)) } else { Ok(()) }
    }

    /// `depth` is the number of containers around this value.
    fn value(&mut self, depth: usize) -> Result<Value<'a>, DecodeError> {
        self.count()?;
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                self.int().map(Value::Int)
            }
            b'0'..=b'9' => self.bytes().map(Value::Bytes),
            b'l' => {
                self.enter(depth)?;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            b'd' => {
                self.enter(depth)?;
                let mut entries: Vec<(&[u8], Value)> = Vec::new();
                let mut sorted = true;
                while self.peek()? != b'e' {
                    let at = self.pos;
                    let byte = self.peek()?;
                    if !byte.is_ascii_digit() {
                        return Err(DecodeError::Unexpected { at, byte });
                    }
                    self.count()?;
                    let key = self.bytes()?;
                    let value = self.value(depth + 1)?;
                    if let Some((last, _)) = entries.last() {
                        match (*last).cmp(key) {
                            std::cmp::Ordering::Less => {}
                            std::cmp::Ordering::Equal => return Err(DecodeError::DuplicateKey),
                            std::cmp::Ordering::Greater => sorted = false,
                        }
                    }
                    entries.push((key, value));
                }
                self.pos += 1;
                // The format requires sorted keys, but not every encoder
                // complies. Sorting here keeps lookups correct either way.
                if !sorted {
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                    if entries.windows(2).any(|w| w[0].0 == w[1].0) {
                        return Err(DecodeError::DuplicateKey);
                    }
                }
                Ok(Value::Dict(entries))
            }
            byte => Err(DecodeError::Unexpected { at: self.pos, byte }),
        }
    }

    fn enter(&mut self, depth: usize) -> Result<(), DecodeError> {
        if depth >= self.limits.depth {
            return Err(DecodeError::TooDeep(self.limits.depth));
        }
        self.pos += 1;
        Ok(())
    }

    /// The digits of an integer, after the `i`. No leading zeros, no `-0`.
    fn int(&mut self) -> Result<i64, DecodeError> {
        let start = self.pos;
        let negative = self.peek()? == b'-';
        if negative {
            self.pos += 1;
        }
        let digits = self.digits()?;
        if self.peek()? != b'e' {
            return Err(self.unexpected());
        }
        self.pos += 1;
        let bad = || DecodeError::BadNumber(start);
        if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') || (negative && digits == b"0") {
            return Err(bad());
        }
        let mut n: i64 = 0;
        for d in digits {
            let d = i64::from(d - b'0');
            let next = n.checked_mul(10).and_then(|n| if negative { n.checked_sub(d) } else { n.checked_add(d) });
            n = next.ok_or_else(bad)?;
        }
        Ok(n)
    }

    /// A length-prefixed string. The length is checked against what is
    /// left of the input before anything is sliced.
    fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let start = self.pos;
        let digits = self.digits()?;
        if self.peek()? != b':' {
            return Err(self.unexpected());
        }
        self.pos += 1;
        let bad = || DecodeError::BadNumber(start);
        if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
            return Err(bad());
        }
        let mut len: usize = 0;
        for d in digits {
            len = len.checked_mul(10).and_then(|n| n.checked_add(usize::from(d - b'0'))).ok_or_else(bad)?;
        }
        if len > self.input.len() - self.pos {
            return Err(DecodeError::Eof);
        }
        let bytes = &self.input[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    fn digits(&mut self) -> Result<&'a [u8], DecodeError> {
        let start = self.pos;
        while self.peek()?.is_ascii_digit() {
            self.pos += 1;
        }
        Ok(&self.input[start..self.pos])
    }

    fn unexpected(&self) -> DecodeError {
        match self.input.get(self.pos) {
            Some(&byte) => DecodeError::Unexpected { at: self.pos, byte },
            None => DecodeError::Eof,
        }
    }
}

pub(crate) fn encode(value: &Value<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &Value<'_>, out: &mut Vec<u8>) {
    match value {
        Value::Int(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(b) => encode_bytes(b, out),
        Value::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        Value::Dict(entries) => {
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            out.push(b'd');
            for (key, value) in sorted {
                encode_bytes(key, out);
                encode_into(value, out);
            }
            out.push(b'e');
        }
    }
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(input: &[u8]) -> Result<Value<'_>, DecodeError> {
        decode(input, Limits::INFO)
    }

    #[test]
    fn round_trips() {
        let value = Value::dict(vec![
            ("zeta", Value::Int(-42)),
            ("alpha", Value::List(vec![Value::Bytes(b"spam"), Value::Int(0), Value::Bytes(b"")])),
            ("mid", Value::dict(vec![("k", Value::Int(i64::MAX)), ("j", Value::Int(i64::MIN))])),
        ]);
        let encoded = encode(&value);
        assert_eq!(
            encoded,
            b"d5:alphal4:spami0e0:e3:midd1:ji-9223372036854775808e1:ki9223372036854775807ee4:zetai-42ee".to_vec()
        );
        assert_eq!(dec(&encoded).unwrap(), value);
        assert_eq!(value.get(b"zeta").and_then(Value::as_int), Some(-42));
        assert_eq!(value.get(b"missing"), None);
    }

    #[test]
    fn decodes_binary_strings() {
        let input = b"3:\x00\xff\x13";
        assert_eq!(dec(input).unwrap(), Value::Bytes(&[0, 0xff, 0x13]));
    }

    #[test]
    fn sorts_unsorted_dicts_and_rejects_duplicate_keys() {
        let value = dec(b"d1:bi2e1:ai1ee").unwrap();
        assert_eq!(value.get(b"a").and_then(Value::as_int), Some(1));
        assert_eq!(value.get(b"b").and_then(Value::as_int), Some(2));
        assert_eq!(dec(b"d1:ai1e1:ai2ee"), Err(DecodeError::DuplicateKey));
        assert_eq!(dec(b"d1:ai1e1:bi2e1:ai3ee"), Err(DecodeError::DuplicateKey));
    }

    #[test]
    fn rejects_malformed_input() {
        for input in [
            &b""[..],
            b"i",
            b"i42",
            b"ie",
            b"i-e",
            b"i-0e",
            b"i03e",
            b"i1x2e",
            b"i99999999999999999999e",
            b"4:abc",
            b"03:abc",
            b":abc",
            b"l",
            b"li1e",
            b"d",
            b"d1:a",
            b"di1ei2ee",
            b"dlei1ee",
            b"x",
            b"e",
            b"-1:a",
            b"18446744073709551616:a",
        ] {
            assert!(dec(input).is_err(), "{:?} should not decode", String::from_utf8_lossy(input));
        }
    }

    #[test]
    fn rejects_truncations_of_a_valid_message() {
        let full = encode(&Value::dict(vec![
            ("a", Value::List(vec![Value::Int(1), Value::Bytes(b"xyz")])),
            ("b", Value::dict(vec![("c", Value::Int(-7))])),
        ]));
        assert!(dec(&full).is_ok());
        for end in 0..full.len() {
            assert!(dec(&full[..end]).is_err(), "prefix of length {end} decoded");
        }
    }

    #[test]
    fn rejects_trailing_bytes_but_prefix_decoding_reports_them() {
        assert_eq!(dec(b"i1eXYZ"), Err(DecodeError::Trailing(3)));
        let (value, used) = decode_prefix(b"d1:ai1eeRAW", Limits::PEER).unwrap();
        assert_eq!(used, 8);
        assert_eq!(value.get(b"a").and_then(Value::as_int), Some(1));
    }

    #[test]
    fn caps_nesting_depth() {
        let ok = format!("{}{}", "l".repeat(64), "e".repeat(64));
        assert!(dec(ok.as_bytes()).is_ok());
        let deep = format!("{}{}", "l".repeat(65), "e".repeat(65));
        assert_eq!(dec(deep.as_bytes()), Err(DecodeError::TooDeep(64)));
        // A depth bomb far deeper than the stack would allow.
        let bomb = "l".repeat(1_000_000);
        assert_eq!(dec(bomb.as_bytes()), Err(DecodeError::TooDeep(64)));
        let dict_bomb = "d1:a".repeat(100_000);
        assert_eq!(dec(dict_bomb.as_bytes()), Err(DecodeError::TooDeep(64)));
    }

    #[test]
    fn caps_value_count() {
        let many = format!("l{}e", "i0e".repeat(100));
        let limits = Limits { depth: 4, values: 50 };
        assert_eq!(decode(many.as_bytes(), limits), Err(DecodeError::TooManyValues(50)));
        assert!(decode(many.as_bytes(), Limits { depth: 4, values: 101 }).is_ok());
    }

    #[test]
    fn huge_length_prefixes_do_not_allocate() {
        // Claims 2^63 bytes; must fail on the bounds check, not try to
        // reserve or slice that much.
        assert_eq!(dec(b"9223372036854775807:abc"), Err(DecodeError::Eof));
        assert_eq!(dec(b"l4294967296:xe"), Err(DecodeError::Eof));
    }
}
