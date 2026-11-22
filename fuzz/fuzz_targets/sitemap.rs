#![no_main]

//! Sitemap discovery: `src/crawl.rs` `sitemap_locations`.
//!
//! A sitemap is fetched from a third-party site and parsed as XML before any of
//! its locations are checked, so the parser itself must survive arbitrary bytes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rustxng::crawl::sitemap_locations(data, 1000);
});
