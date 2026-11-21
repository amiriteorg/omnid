//! Page languages as primary language subtags (`en`, `ja`, `tk`): what a
//! page declares, what its text looks like, and what a reader prefers.

/// The primary subtag of a language tag (`en-US`, `pt_BR`, `zh-Hans`), lower
/// case, with withdrawn codes replaced. `None` for anything that is not a
/// language (`x-default`, `*`, empty).
pub fn primary_subtag(tag: &str) -> Option<String> {
    let primary = tag.trim().split(['-', '_']).next()?.to_ascii_lowercase();
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    Some(
        match primary.as_str() {
            "iw" => "he",
            "in" => "id",
            "ji" => "yi",
            "no" => "nb",
            other => other,
        }
        .to_owned(),
    )
}

/// The language of `text`, when the detector is confident about it. Short or
/// mixed text gives `None` rather than a guess.
pub fn detect(text: &str) -> Option<String> {
    // A few kilobytes decide it; more only costs time.
    let sample = text.get(..text.len().min(4_096)).unwrap_or_else(|| {
        let end = (0..=4_096.min(text.len())).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        &text[..end]
    });
    let info = whatlang::detect(sample).filter(whatlang::Info::is_reliable)?;
    Some(iso639_1(info.lang().code()).to_owned())
}

/// The languages a reader asked for in an `Accept-Language` header, best
/// first, without repeats; at most six.
pub fn preferred(accept_language: &str) -> Vec<String> {
    let mut ranked: Vec<(f32, usize, String)> = accept_language
        .split(',')
        .enumerate()
        .filter_map(|(position, item)| {
            let mut parts = item.split(';');
            let tag = primary_subtag(parts.next()?)?;
            let quality = parts
                .filter_map(|p| p.trim().strip_prefix("q="))
                .find_map(|q| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            (quality > 0.0).then_some((quality, position, tag))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut languages: Vec<String> = Vec::new();
    for (_, _, tag) in ranked {
        if !languages.contains(&tag) {
            languages.push(tag);
        }
    }
    languages.truncate(6);
    languages
}

/// whatlang names languages in ISO 639-3; pages and browsers use 639-1.
fn iso639_1(code: &str) -> &str {
    match code {
        "afr" => "af",
        "aka" => "ak",
        "amh" => "am",
        "ara" => "ar",
        "aze" => "az",
        "bel" => "be",
        "ben" => "bn",
        "bul" => "bg",
        "cat" => "ca",
        "ces" => "cs",
        "cmn" => "zh",
        "cym" => "cy",
        "dan" => "da",
        "deu" => "de",
        "ell" => "el",
        "eng" => "en",
        "epo" => "eo",
        "est" => "et",
        "fin" => "fi",
        "fra" => "fr",
        "guj" => "gu",
        "heb" => "he",
        "hin" => "hi",
        "hrv" => "hr",
        "hun" => "hu",
        "hye" => "hy",
        "ind" => "id",
        "ita" => "it",
        "jav" => "jv",
        "jpn" => "ja",
        "kan" => "kn",
        "kat" => "ka",
        "khm" => "km",
        "kor" => "ko",
        "lat" => "la",
        "lav" => "lv",
        "lit" => "lt",
        "mal" => "ml",
        "mar" => "mr",
        "mkd" => "mk",
        "mya" => "my",
        "nep" => "ne",
        "nld" => "nl",
        "nob" => "nb",
        "ori" => "or",
        "pan" => "pa",
        "pes" => "fa",
        "pol" => "pl",
        "por" => "pt",
        "ron" => "ro",
        "rus" => "ru",
        "sin" => "si",
        "slk" => "sk",
        "slv" => "sl",
        "sna" => "sn",
        "spa" => "es",
        "srp" => "sr",
        "swe" => "sv",
        "tam" => "ta",
        "tel" => "te",
        "tgl" => "tl",
        "tha" => "th",
        "tuk" => "tk",
        "tur" => "tr",
        "ukr" => "uk",
        "urd" => "ur",
        "uzb" => "uz",
        "vie" => "vi",
        "yid" => "yi",
        "zul" => "zu",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_declared_tags() {
        assert_eq!(primary_subtag("en-US").as_deref(), Some("en"));
        assert_eq!(primary_subtag(" PT_br ").as_deref(), Some("pt"));
        assert_eq!(primary_subtag("zh-Hans-CN").as_deref(), Some("zh"));
        assert_eq!(primary_subtag("iw").as_deref(), Some("he"));
        for junk in ["", "*", "x-default", "e", "english", "e1"] {
            assert_eq!(primary_subtag(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn detects_confidently_or_not_at_all() {
        let english = "The Tor Project builds free software that protects your privacy online and helps you \
                       get around censorship, so that everyone can use the internet freely and safely.";
        assert_eq!(detect(english).as_deref(), Some("en"));
        let japanese = "Torプロジェクトは、オンラインでのプライバシーを守り、検閲を回避するための自由なソフトウェアを開発しています。";
        assert_eq!(detect(japanese).as_deref(), Some("ja"));
        assert_eq!(detect("ok"), None, "too short to say");
        assert_eq!(detect(&"é".repeat(10_000)), detect(&"é".repeat(10_000)), "long text is cut on a char boundary");
    }

    #[test]
    fn reads_accept_language_in_order_of_preference() {
        assert_eq!(preferred("de-CH, en;q=0.8, de;q=0.9, *;q=0.5"), ["de", "en"]);
        assert_eq!(preferred("ja,en-US;q=0.9,en;q=0.8"), ["ja", "en"]);
        assert_eq!(preferred("fr;q=0, en"), ["en"], "q=0 means not acceptable");
        assert!(preferred("").is_empty());
        assert_eq!(preferred("a1, en;q=bogus").as_slice(), ["en"]);
    }
}
