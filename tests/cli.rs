//! CLI behaviour: `--check-config` reports readiness without opening a socket
//! and without contacting any provider.

use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rustxng"))
}

fn json_stdout(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn check_config_reports_the_clean_profile_readiness() {
    let output = binary()
        .args(["--check-config", "--profile", "clean"])
        .output()
        .expect("run the binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["profile"], "clean");
    // Valid but surprising settings are reported next to the readiness.
    assert!(
        json["warnings"].is_array(),
        "warnings are part of the report: {json}"
    );
    assert_eq!(json["engines"], 0);
    // The two indexes are local build artefacts and are gitignored, so a clean
    // checkout has none and a developer checkout may have both. With no engine
    // enabled the index alone decides readiness, which keeps this assertion
    // true either way.
    let local_index = json["local_index"]
        .as_bool()
        .unwrap_or_else(|| panic!("local_index is a boolean: {json}"));
    assert_eq!(
        json["ready"].as_bool(),
        Some(local_index),
        "an opened index is the only source: {json}"
    );
    if !local_index {
        assert!(
            json["reasons"].as_array().is_some_and(|reasons| reasons
                .iter()
                .any(|reason| reason == "no local index is available")),
            "the missing index is explained: {json}"
        );
    }
}

#[test]
fn check_config_reports_a_profile_without_sources() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.toml");
    std::fs::write(
        &path,
        "use_default_settings = false\n[search]\nlocal_index_path = \"\"\nwikipedia_index_path = \"\"\n",
    )
    .expect("write settings");
    let output = binary()
        .args([
            "--check-config",
            "--config",
            path.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("run the binary");
    assert!(output.status.success());
    let json = json_stdout(&output);
    assert_eq!(json["ready"], false, "{json}");
    assert_eq!(json["engines"], 0);
    assert_eq!(json["reasons"].as_array().map(Vec::len), Some(2), "{json}");
}

#[test]
fn check_config_reports_the_circuit_breaker_policy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.toml");
    std::fs::write(
        &path,
        "use_default_settings = false\n[search]\nlocal_index_path = \"\"\nwikipedia_index_path = \"\"\n[search.circuit]\nenabled = false\nwindow = 5\nfailure_ratio = 0.5\nminimum_samples = 4\nopen_seconds = 60\nmax_open_seconds = 120\n",
    )
    .expect("write settings");
    let output = binary()
        .args([
            "--check-config",
            "--config",
            path.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("run the binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["circuit"]["enabled"], false, "{json}");
    assert_eq!(json["circuit"]["window"], 5, "{json}");
    assert_eq!(json["circuit"]["failure_ratio"], 0.5, "{json}");
    assert_eq!(json["circuit"]["minimum_samples"], 4, "{json}");
    assert_eq!(json["circuit"]["open_seconds"], 60, "{json}");
    assert_eq!(json["circuit"]["max_open_seconds"], 120, "{json}");
    // Valid but surprising: disabling the breaker is reported as a warning.
    assert!(
        json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("search.circuit.enabled"))),
        "{json}"
    );
}

#[test]
fn check_config_fails_on_malformed_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "server = \"not a table\"\n").expect("write settings");
    let output = binary()
        .args([
            "--check-config",
            "--config",
            path.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("run the binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "{stderr}");
}

#[test]
fn check_config_cannot_be_combined_with_mcp() {
    let output = binary()
        .args(["mcp", "--check-config"])
        .output()
        .expect("run the binary");
    assert_eq!(output.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn sigterm_stops_the_server_gracefully() {
    use std::io::Write as _;
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("temp dir");
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("pick a port");
        listener.local_addr().expect("local addr").port()
    };
    let settings = dir.path().join("settings.toml");
    let mut file = std::fs::File::create(&settings).expect("create settings");
    write!(
        file,
        "use_default_settings = false\n[server]\nbind_address = \"127.0.0.1\"\nport = {port}\nsecret_key = \"cli-test-secret\"\n[search]\nlocal_index_path = \"\"\n"
    )
    .expect("write settings");
    drop(file);

    let mut child = Command::new(env!("CARGO_BIN_EXE_rustxng"))
        .args(["--config", settings.to_str().expect("utf-8 path")])
        .env("RUST_LOG", "error")
        .env("RUSTXNG_IDLE_TIMEOUT", "0")
        .spawn()
        .expect("start the server");

    // Wait for a real HTTP answer: the shutdown future is polled by the same
    // server future, so answering requests means SIGTERM is already watched.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            use std::io::{Read as _, Write as _};
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let request = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
            if stream.write_all(request).is_ok() {
                let mut answer = String::new();
                if stream.read_to_string(&mut answer).is_ok() && answer.contains(" 200 ") {
                    ready = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !ready {
        let _ = child.kill();
        panic!("the server did not answer /healthz on port {port}");
    }

    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(status.success(), "kill -TERM failed");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("wait for the server") {
            Some(status) => {
                assert_eq!(
                    status.code(),
                    Some(0),
                    "SIGTERM must stop the server cleanly"
                );
                return;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("the server did not exit within 15s of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// A fake Mwmbl engine that answers only after `delay`.
#[cfg(unix)]
fn spawn_delayed_engine(
    delay: std::time::Duration,
) -> (
    String,
    std::sync::mpsc::Receiver<()>,
    std::thread::JoinHandle<()>,
) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake engine");
    let address = listener.local_addr().expect("local address");
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            // The server has the request in flight now; a runner that starts it
            // slowly must not be raced by the signal below.
            let _ = accepted_tx.send(());
            std::thread::sleep(delay);
            let body = r#"[{"url":"https://example.com/slow","title":[{"value":"Slow result"}],"extract":[{"value":"Answered after the delay."}]}]"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    (format!("http://{address}/api/v1"), accepted_rx, handle)
}

#[cfg(unix)]
fn shutdown_settings(path: &std::path::Path, engine_url: &str, port: u16, grace: u64) {
    use std::io::Write as _;
    let mut file = std::fs::File::create(path).expect("create settings");
    write!(
        file,
        "use_default_settings = false\n[server]\nbind_address = \"127.0.0.1\"\nport = {port}\nsecret_key = \"cli-test-secret\"\nshutdown_grace = {grace}\n[search]\nlocal_index_path = \"\"\ntimeout_limit = 20.0\n[[engines]]\nname = \"mwmbl\"\nengine = \"mwmbl\"\nshortcut = \"mwm\"\nbase_url = \"{engine_url}\"\n"
    )
    .expect("write settings");
}

#[cfg(unix)]
fn wait_for_healthz(port: u16) {
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server did not listen on port {port}");
}

#[cfg(unix)]
fn start_search(port: u16) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => stream,
            Err(error) => return format!("connect failed: {error}"),
        };
        let _ = stream.write_all(
            b"GET /search?q=slow&format=json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    })
}

#[cfg(unix)]
fn wait_for_exit(
    child: &mut std::process::Child,
    limit: std::time::Duration,
) -> std::process::ExitStatus {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait().expect("wait for the server") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("the server did not exit within {limit:?}");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(unix)]
#[test]
fn sigterm_drains_an_in_flight_request() {
    use std::time::Duration;
    let dir = tempfile::tempdir().expect("temp dir");
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("pick a port");
        listener.local_addr().expect("local addr").port()
    };
    let settings = dir.path().join("settings.toml");
    let (engine_url, accepted, engine) = spawn_delayed_engine(Duration::from_millis(800));
    shutdown_settings(&settings, &engine_url, port, 8);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustxng"))
        .args(["--config", settings.to_str().expect("utf-8 path")])
        .env("RUST_LOG", "error")
        .spawn()
        .expect("start the server");
    wait_for_healthz(port);

    let search = start_search(port);
    // Signal only once the engine really has the request, so a slow runner
    // cannot turn "in flight" into "not started yet".
    accepted
        .recv_timeout(Duration::from_secs(5))
        .expect("the engine received the search");
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(status.success());

    let answer = search.join().expect("search thread");
    assert!(
        answer.starts_with("HTTP/1.1 200"),
        "the in-flight request must finish: {answer:.200}"
    );
    let exit = wait_for_exit(&mut child, Duration::from_secs(6));
    assert_eq!(exit.code(), Some(0));
    engine.join().expect("engine thread");
}

#[cfg(unix)]
#[test]
fn sigterm_stops_after_the_shutdown_grace() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().expect("temp dir");
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("pick a port");
        listener.local_addr().expect("local addr").port()
    };
    let settings = dir.path().join("settings.toml");
    let (engine_url, accepted, engine) = spawn_delayed_engine(Duration::from_secs(10));
    shutdown_settings(&settings, &engine_url, port, 1);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustxng"))
        .args(["--config", settings.to_str().expect("utf-8 path")])
        .env("RUST_LOG", "error")
        .spawn()
        .expect("start the server");
    wait_for_healthz(port);

    let _search = start_search(port);
    accepted
        .recv_timeout(Duration::from_secs(5))
        .expect("the engine received the search");
    let started = Instant::now();
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(status.success());

    let exit = wait_for_exit(&mut child, Duration::from_secs(6));
    let elapsed = started.elapsed();
    assert_eq!(exit.code(), Some(0));
    assert!(
        elapsed < Duration::from_secs(5),
        "the watchdog must end the drain before the 10s engine answer: {elapsed:?}"
    );
    // The client connection is closed by the stopped server; the engine thread
    // finishes its sleep on its own and is not part of the server process.
    engine.join().expect("engine thread");
}
