#![no_main]

//! robots.txt: `src/robots.rs` `Robots::from_response`.
//!
//! Both the page reader and the crawler parse the file the site publishes, then
//! ask it about a path that is attacker controlled as well.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let body = String::from_utf8_lossy(data);
    let robots = rustxng::robots::Robots::from_response(200, &body, "rustxng");
    let _ = robots.allowed(&body);
    let _ = robots.crawl_delay();
    let _ = robots.sitemaps();
});
