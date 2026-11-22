#![no_main]

//! XPath subset: `src/engine/xpath.rs` `XPath::parse`.
//!
//! Expressions come from engine definitions and from operator configuration, but
//! they are compiled once per engine and a panic there takes the process down.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(expression) = std::str::from_utf8(data) {
        let _ = rustxng::engine::xpath::XPath::parse(expression);
    }
});
