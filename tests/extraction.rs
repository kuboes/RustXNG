//! Offline extraction corpus: every page declares what must be kept and what
//! must stay out of the readable text.
//!
//! The fixtures and the manifest are checked in, so a third party can rerun the
//! same assertions without network access.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use scraper::Html;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extraction")
}

fn spaced_punctuation(text: &str) -> usize {
    let bytes: Vec<char> = text.chars().collect();
    bytes
        .windows(2)
        .filter(|pair| {
            pair[0].is_whitespace()
                && matches!(pair[1], '.' | ',' | ':' | ';' | '!' | '?' | ')' | ']')
        })
        .count()
}

#[test]
fn every_page_keeps_its_content_and_drops_page_chrome() {
    let dir = corpus_dir();
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .expect("valid manifest json");
    let files = manifest["files"].as_array().expect("files array");

    let mut reported = Vec::new();
    for entry in files {
        let name = entry["file"].as_str().expect("file name");
        let html = std::fs::read_to_string(dir.join(name)).expect("fixture html");
        let document = Html::parse_document(&html);
        let text = rustxng::content::main_text_limited(&document, 1_000_000);

        for expected in entry["must_contain"].as_array().expect("must_contain") {
            let expected = expected.as_str().expect("string");
            assert!(
                text.contains(expected),
                "{name}: missing {expected:?}\n---\n{text}"
            );
        }
        for unexpected in entry["must_not_contain"]
            .as_array()
            .expect("must_not_contain")
        {
            let unexpected = unexpected.as_str().expect("string");
            assert!(
                !text.contains(unexpected),
                "{name}: kept {unexpected:?}\n---\n{text}"
            );
        }
        let min = entry["min_chars"].as_u64().expect("min_chars") as usize;
        assert!(
            text.chars().count() >= min,
            "{name}: only {} chars",
            text.chars().count()
        );
        let spaced = spaced_punctuation(&text);
        // `tidy` removes the space that joining inline elements leaves before
        // closing punctuation; the corpus keeps that from regressing.
        assert_eq!(
            spaced, 0,
            "{name}: {spaced} spaced punctuation marks\n---\n{text}"
        );
        reported.push((name.to_string(), text.chars().count(), spaced));
    }

    for (name, chars, spaced) in reported {
        println!("{name}: {chars} chars, {spaced} spaced punctuation");
    }
}

#[test]
fn every_fixture_is_described_by_the_manifest() {
    let dir = corpus_dir();
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .expect("valid manifest json");
    let described: BTreeSet<String> = manifest["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|entry| entry["file"].as_str().expect("file name").to_string())
        .collect();
    let present: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("corpus dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".html"))
        .collect();
    assert_eq!(present, described, "manifest and fixtures must match");
}
