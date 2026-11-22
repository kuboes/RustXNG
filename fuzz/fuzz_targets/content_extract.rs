#![no_main]

//! Main-text extraction: `src/content.rs` `main_text_limited`.
//!
//! The input is the body of a third-party page, so the HTML parser and the
//! extraction heuristics see arbitrary bytes on every raw-content read.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data);
    let document = scraper::Html::parse_document(&html);
    let _ = rustxng::content::main_text_limited(&document, 4096);
});
