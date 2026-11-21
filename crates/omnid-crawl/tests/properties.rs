//! Properties of extraction and the safety gate over generated inputs:
//! hostile bytes never panic, and the caps always hold.

use omnid_core::Source;
use omnid_crawl::Safety;
use omnid_crawl::extract::extract;
use proptest::prelude::*;

/// Fragments that exercise the tokenizer's states and the charset sniffing.
fn html() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        "[a-zA-Z0-9 \n\t&;#]{0,30}",
        Just("<title>".to_owned()),
        Just("</title>".to_owned()),
        Just("<script>".to_owned()),
        Just("</script>".to_owned()),
        Just("<style>x</style>".to_owned()),
        Just("<!--".to_owned()),
        Just("-->".to_owned()),
        Just("<meta charset=windows-1251>".to_owned()),
        Just(r#"<meta name="robots" content="noindex">"#.to_owned()),
        Just(r#"<meta name="description" content="d">"#.to_owned()),
        Just(r#"<meta name=keywords content="a,b,,c">"#.to_owned()),
        "<a href=\"[^\"]{0,20}\">",
        Just("</a>".to_owned()),
        Just("<div>".to_owned()),
        Just("<plaintext>".to_owned()),
        Just("<svg/>".to_owned()),
        Just("&amp;&lt;&#x1F600;&bogus;".to_owned()),
    ];
    prop::collection::vec(piece, 0..60).prop_map(|pieces| pieces.concat())
}

fn content_type() -> impl Strategy<Value = Option<String>> {
    prop::option::of(prop_oneof![
        Just("text/html".to_owned()),
        Just("text/html; charset=utf-8".to_owned()),
        Just("text/html; charset=\"shift_jis\"".to_owned()),
        Just("text/plain; charset=iso-8859-1".to_owned()),
        "[a-z]{1,8}/[a-z]{1,8}(; charset=[a-z0-9-]{1,12})?",
    ])
}

fn check(page: &omnid_crawl::extract::Extracted) -> Result<(), TestCaseError> {
    prop_assert!(page.title.len() <= 300);
    prop_assert!(page.description.len() <= 500);
    prop_assert!(page.text.len() <= 100_000);
    prop_assert!(page.links.len() <= 500);
    prop_assert!(page.keywords.len() <= 32);
    for text in [&page.title, &page.text] {
        prop_assert!(!text.starts_with(' ') && !text.ends_with(' ') && !text.contains("  "), "{:?}", text);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn arbitrary_bytes_never_panic(body in prop::collection::vec(any::<u8>(), 0..4_096), ct in content_type()) {
        check(&extract(ct.as_deref(), &body))?;
    }

    #[test]
    fn html_like_input_keeps_its_caps(body in html(), ct in content_type()) {
        check(&extract(ct.as_deref(), body.as_bytes()))?;
    }

    #[test]
    fn script_and_style_never_reach_the_text(visible in "[a-z]{1,10}", hidden in "[A-Z]{12,20}") {
        let page = format!("<p>{visible}</p><script>{hidden}</script><style>{hidden}</style><p>{visible}</p>");
        let extracted = extract(Some("text/html"), page.as_bytes());
        prop_assert!(!extracted.text.contains(&hidden));
        prop_assert!(extracted.text.contains(&visible));
    }

    #[test]
    fn the_safety_gate_never_panics(list in "\\PC{0,300}", terms in "\\PC{0,100}", text in "\\PC{0,300}", host in "[a-z0-9.-]{1,30}") {
        let safety = Safety::from_lists(&list, &terms);
        if let Ok(source) = Source::parse(&host) {
            let _ = safety.check_source(&source);
        }
        let _ = safety.check_content(&[&text]);
    }
}
