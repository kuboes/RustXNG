#![no_main]

//! RESP reply parser: `src/engines/valkey_server.rs` `Connection::read_reply`.
//!
//! The reply is whatever the configured key-value server sends, and it is the
//! only untrusted byte stream on that path: bulk lengths, array counts and
//! markers all come from the peer. `Connection` is private and reads from a
//! socket, so this harness serves the fuzz input from a loopback server and
//! runs the real engine against it (`search_offline` above the parser).
//!
//! Known finding (2026-11-21): a bulk length such as `$99999999999` makes the
//! parser allocate the announced size before reading it, which aborts the
//! process. `fuzz/README.md` records the reproduction and the fix location;
//! the parser is outside this change's file scope, so the harness reports it
//! instead of hiding it.

use std::io::Write;
use std::net::{Shutdown, TcpListener};
use std::sync::{Arc, Mutex, OnceLock};

use libfuzzer_sys::fuzz_target;
use rustxng::Query;
use rustxng::engine::Engine;
use rustxng::engines::valkey_server::ValkeyServer;
use rustxng::settings::EngineConfig;

/// The reply stream served to the next connection.
static REPLY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// One loopback reply server for the whole run: every connection receives the
/// current [`REPLY`] and a clean half-close, so the parser sees an end of
/// stream when the bytes run out instead of blocking.
fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the reply server");
        let port = listener.local_addr().expect("reply server address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let reply = REPLY.lock().expect("reply lock").clone();
                std::thread::spawn(move || {
                    let _ = stream.write_all(&reply);
                    let _ = stream.shutdown(Shutdown::Write);
                    let _ = std::io::copy(&mut stream, &mut std::io::sink());
                });
            }
        });
        port
    })
}

/// The minimal query the engine needs; the text only feeds the lookup keys.
fn query() -> Query {
    Query {
        raw: "fuzz".into(),
        query_parts: Vec::new(),
        user_query_parts: vec!["fuzz".into()],
        languages: Vec::new(),
        timeout_limit: None,
        external_bang: None,
        specific: false,
        engine_refs: Vec::new(),
        autocomplete_list: Vec::new(),
        redirect_to_first_result: false,
        disabled_engines: Vec::new(),
        locale: String::new(),
        pageno: 1,
        time_range: None,
        safesearch: 0,
        categories: Vec::new(),
        engines: Vec::new(),
        max_page: 0,
    }
}

fn engine() -> Arc<ValkeyServer> {
    let mut config = EngineConfig::new("fuzz", "valkey_server", "fuzz", &["general"]);
    config.timeout = Some(1.0);
    config
        .extra
        .insert("port".into(), toml::Value::Integer(port() as i64));
    ValkeyServer::from_config(&config).expect("engine configuration")
}

fuzz_target!(|data: &[u8]| {
    *REPLY.lock().expect("reply lock") = data.to_vec();
    let _ = engine().search_offline(&query(), 1);
});
