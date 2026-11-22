#![no_main]

//! Image proxy request: `src/web/image_proxy.rs` `is_valid` and `is_public_url`.
//!
//! `url` and `h` arrive as query parameters of `/image_proxy`. `is_valid`
//! compares a hex candidate against a keyed digest in constant time and
//! `is_public_url` decides whether the target may be contacted at all; both run
//! before any request is made.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (url, candidate) = text.split_once('\n').unwrap_or((text.as_ref(), ""));
    let _ = rustxng::web::image_proxy::is_valid("fuzz-secret", url, candidate);
    let _ = rustxng::web::image_proxy::is_public_url(url);
});
