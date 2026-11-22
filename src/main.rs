//! Binary entry point: `rustxng [--profile NAME] [--config FILE] [--bind ADDRESS] [--port PORT]`.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use rustxng::aggregator::Searcher;
use rustxng::settings::{Profile, Settings};
use rustxng::version::version_string;
use rustxng::web::{self, AppState};

const HELP: &str = "rustxng - a Rust rewrite of the SearXNG metasearch engine

USAGE:
    rustxng [OPTIONS]
    rustxng --check-engines [--profile NAME] [--query TEXT] [--timeout SECS] [--json]
    rustxng mcp [--profile NAME] [--config FILE]
                                   Serve MCP tools on stdin/stdout

OPTIONS:
    --profile <NAME>       Built-in provider set (default: standard)
                             standard  keyless providers that allow automated use
                             full      standard + search engines whose terms
                                       forbid automated access (private use only;
                                       read their terms first)
                             clean     documented public APIs and own crawl only
    -c, --config <FILE>    Settings file laid over the profile
                           (default: $SEARXNG_SETTINGS_PATH)
    -b, --bind <ADDRESS>   Address to listen on (default: server.bind_address)
    -p, --port <PORT>      Port to listen on (default: server.port)
    --check-config        Validate the settings, build the engines and print
                          whether this instance can search (no socket is opened)
    --check-engines       Ask every enabled engine once and print its health
                          (diagnostic; the exit code stays 0 unless the profile
                          cannot be built). The probe does not use the cache
    --query <TEXT>        Query sent by --check-engines (default: rust)
    --timeout <SECS>      Per-engine deadline for --check-engines (default: 5)
    --json                Print the --check-engines report as JSON
    --check-update        Check the latest official release
    --update              Download, verify and install the latest release
    -v, --version          Print the version
    -h, --help             Print this help

ENVIRONMENT:
    RUSTXNG_PROFILE, SEARXNG_SETTINGS_PATH, SEARXNG_BIND_ADDRESS, SEARXNG_PORT, SEARXNG_SECRET,
    SEARXNG_BASE_URL, SEARXNG_DEBUG, SEARXNG_ENABLE_METRICS, RUST_LOG
";

struct Args {
    mcp: bool,
    check_config: bool,
    check_engines: bool,
    config: Option<PathBuf>,
    profile: Option<Profile>,
    bind: Option<String>,
    port: Option<u16>,
    update: Option<bool>,
    query: Option<String>,
    timeout: Option<f64>,
    json: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        mcp: false,
        check_config: false,
        check_engines: false,
        config: None,
        profile: None,
        bind: None,
        port: None,
        update: None,
        query: None,
        timeout: None,
        json: false,
    };
    let mut argv = std::env::args().skip(1).peekable();
    if argv.peek().map(String::as_str) == Some("mcp") {
        argv.next();
        args.mcp = true;
    }
    while let Some(argument) = argv.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--check-config" => args.check_config = true,
            "--check-engines" => args.check_engines = true,
            "--json" => args.json = true,
            "--query" => args.query = Some(argv.next().ok_or("--query needs text")?),
            "--timeout" => {
                let value = argv.next().ok_or("--timeout needs seconds")?;
                args.timeout = Some(value.parse().map_err(|_| "--timeout needs seconds")?);
            }
            "-v" | "--version" => {
                println!("{}", version_string());
                return Ok(None);
            }
            "--update" | "--check-update" => {
                if args.update.is_some() {
                    return Err("choose only one update action".into());
                }
                args.update = Some(argument == "--update");
            }
            "-c" | "--config" => {
                args.config = Some(PathBuf::from(
                    argv.next().ok_or("--config needs a file path")?,
                ));
            }
            "--profile" => {
                args.profile = Some(Profile::parse(
                    &argv.next().ok_or("--profile needs a name")?,
                )?);
            }
            other if other.starts_with("--profile=") => {
                args.profile = Some(Profile::parse(&other["--profile=".len()..])?);
            }
            "-b" | "--bind" => args.bind = Some(argv.next().ok_or("--bind needs an address")?),
            "-p" | "--port" => {
                let value = argv.next().ok_or("--port needs a number")?;
                args.port = Some(value.parse().map_err(|_| "--port needs a number")?);
            }
            other if other.starts_with("--config=") => {
                args.config = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if other.starts_with("--port=") => {
                args.port = Some(
                    other["--port=".len()..]
                        .parse()
                        .map_err(|_| "--port needs a number")?,
                );
            }
            other if other.starts_with("--query=") => {
                args.query = Some(other["--query=".len()..].to_string());
            }
            other if other.starts_with("--timeout=") => {
                args.timeout = Some(
                    other["--timeout=".len()..]
                        .parse()
                        .map_err(|_| "--timeout needs seconds")?,
                );
            }
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }
    Ok(Some(args))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    if args.mcp && (args.update.is_some() || args.bind.is_some() || args.port.is_some()) {
        eprintln!("error: mcp only accepts --profile and --config");
        return ExitCode::from(2);
    }
    if args.check_config && args.mcp {
        eprintln!("error: --check-config cannot be combined with mcp");
        return ExitCode::from(2);
    }
    if args.check_engines && (args.mcp || args.check_config || args.update.is_some()) {
        eprintln!(
            "error: --check-engines only accepts --profile, --config, --query, --timeout and --json"
        );
        return ExitCode::from(2);
    }
    if args.check_engines && (args.bind.is_some() || args.port.is_some()) {
        eprintln!("error: --check-engines does not open a socket; drop --bind/--port");
        return ExitCode::from(2);
    }
    if !args.check_engines && (args.query.is_some() || args.timeout.is_some() || args.json) {
        eprintln!("error: --query, --timeout and --json need --check-engines");
        return ExitCode::from(2);
    }
    if let Some(timeout) = args.timeout {
        if !timeout.is_finite() || timeout <= 0.0 {
            eprintln!("error: --timeout needs a positive number of seconds");
            return ExitCode::from(2);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // Default to info: debug logs name engines and carry error text that
                // can include query URLs, which is not something a default install
                // should write to journals. Opt in with RUST_LOG when debugging.
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    if let Some(install) = args.update {
        return match rustxng::update::run(install).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let profile = match args
        .profile
        .map(Ok)
        .or_else(|| Profile::from_env().transpose())
    {
        Some(Ok(profile)) => profile,
        Some(Err(error)) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
        None => Profile::Standard,
    };
    let settings_path = args.config.or_else(Settings::path_from_env);
    let mut settings = match Settings::load_profile(settings_path.as_deref(), profile) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A probe never listens, so it must not fail on an unusable listener
    // address: the health of the engines is what it reports.
    let address = if args.check_engines {
        None
    } else {
        match apply_listener(&mut settings, args.bind, args.port) {
            Ok(address) => Some(address),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        }
    };

    let warnings = settings.warnings();
    let searcher = match Searcher::new(settings) {
        Ok(searcher) => searcher,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    for warning in &warnings {
        tracing::warn!("{}", warning);
    }
    if args.check_engines {
        print_profile_notice(profile);
        let options = rustxng::probe::ProbeOptions {
            query: args
                .query
                .unwrap_or_else(|| rustxng::probe::DEFAULT_QUERY.to_string()),
            timeout: std::time::Duration::from_secs_f64(
                args.timeout
                    .unwrap_or(rustxng::probe::DEFAULT_TIMEOUT.as_secs_f64()),
            ),
            concurrency: rustxng::probe::DEFAULT_CONCURRENCY,
        };
        let summary = rustxng::probe::run(&searcher, &options).await;
        // A diagnostic must keep exit code 0 even when its reader goes away
        // (for example `--check-engines | head`), so write errors are ignored.
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        if args.json {
            let _ = writeln!(out, "{}", summary.to_json(profile.name()));
        } else {
            for report in &summary.engines {
                if writeln!(out, "{}", report.line()).is_err() {
                    break;
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    if args.check_config {
        let index_open = searcher.local_index.is_some() || searcher.wiki_index.is_some();
        let readiness = rustxng::diagnostics::Readiness::evaluate(
            &searcher.settings,
            searcher.registry.len(),
            Some(index_open),
        );
        // No search has run yet, so only the circuit policy can be reported
        // here. The live per-engine phases are on `/stats` and `/metrics`.
        let circuit = &searcher.settings.search.circuit;
        let report = serde_json::json!({
            "profile": profile.name(),
            "ready": readiness.ready,
            "engines": readiness.engines,
            "local_index": readiness.local_index,
            "reasons": readiness.reasons,
            "warnings": warnings,
            "circuit": {
                "enabled": circuit.enabled,
                "window": circuit.window,
                "failure_ratio": circuit.failure_ratio,
                "minimum_samples": circuit.minimum_samples,
                "open_seconds": circuit.open_seconds,
                "max_open_seconds": circuit.max_open_seconds,
            },
        });
        println!("{report}");
        return ExitCode::SUCCESS;
    }
    print_profile_notice(profile);
    if args.mcp {
        return match rustxng::mcp::serve_stdio(searcher).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
    tracing::info!(
        profile = profile.name(),
        engines = searcher.registry.len(),
        "{}",
        version_string()
    );

    let shutdown_grace = settings_shutdown_grace(&searcher);
    let state = AppState::new(searcher);
    // Track the last request so an idle sidecar can exit on its own.
    let idle_seconds = idle_timeout_seconds(&state);
    let activity = std::sync::Arc::new(AtomicU64::new(unix_now()));
    let touched = activity.clone();
    let app = web::router(state)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let touched = touched.clone();
                async move {
                    touched.store(unix_now(), Ordering::Relaxed);
                    next.run(request).await
                }
            },
        ));

    let address = address.expect("the listen address is resolved outside --check-engines");
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("error: could not listen on {address}: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("listening on http://{address}");

    // Stop is decided once and shared: the server drains, and a watchdog ends
    // the drain when shutdown_grace elapses so a container or service manager
    // never has to kill the process. Dropping the runtime afterwards stops the
    // remaining tasks and any renderer/PDF child (both spawn kill_on_drop).
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_reason(activity, idle_seconds).await;
        let _ = stop_tx.send(true);
    });
    let mut drain_rx = stop_rx.clone();
    let graceful = async move {
        while drain_rx.changed().await.is_ok() {
            if *drain_rx.borrow() {
                return;
            }
        }
        std::future::pending::<()>().await;
    };
    let mut watchdog_rx = stop_rx.clone();
    let watchdog = async move {
        while watchdog_rx.changed().await.is_ok() {
            if *watchdog_rx.borrow() {
                break;
            }
        }
        tokio::time::sleep(shutdown_grace).await;
    };
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful);
    tokio::select! {
        result = serve => {
            if let Err(error) = result {
                eprintln!("error: server failed: {error}");
                return ExitCode::FAILURE;
            }
        }
        () = watchdog => {
            tracing::warn!(
                grace = ?shutdown_grace,
                "shutdown grace elapsed; stopping with requests still running"
            );
        }
    }
    ExitCode::SUCCESS
}

/// Apply the `--bind`/`--port` overrides to `settings.server` and return the
/// listen address.
///
/// The overrides are written back so everything derived from the settings
/// (notably the generated `Settings::base_url` used for feeds, OpenSearch and
/// proxied links) points at the port actually listened on. An explicitly
/// configured `server.base_url` is left untouched: it names the public URL,
/// which may differ from the local listener (reverse proxy, container port
/// mapping).
fn apply_listener(
    settings: &mut Settings,
    bind: Option<String>,
    port: Option<u16>,
) -> Result<SocketAddr, String> {
    if let Some(bind) = bind {
        settings.server.bind_address = bind;
    }
    if let Some(port) = port {
        settings.server.port = port;
    }
    let (bind, port) = (&settings.server.bind_address, settings.server.port);
    format!("{bind}:{port}")
        .parse()
        .map_err(|error| format!("invalid listen address {bind}:{port}: {error}"))
}

fn settings_shutdown_grace(searcher: &Searcher) -> std::time::Duration {
    searcher.settings.server.shutdown_grace()
}

/// Seconds since the epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The idle timeout the settings or RUSTXNG_IDLE_TIMEOUT ask for.
fn idle_timeout_seconds(state: &AppState) -> u64 {
    if let Ok(value) = std::env::var("RUSTXNG_IDLE_TIMEOUT") {
        if let Ok(seconds) = value.trim().parse() {
            return seconds;
        }
    }
    state.searcher.settings.server.idle_timeout
}

/// Resolve when the process should stop: a signal, or an idle timeout.
async fn shutdown_reason(activity: std::sync::Arc<AtomicU64>, idle_seconds: u64) {
    if idle_seconds == 0 {
        shutdown_signal().await;
        return;
    }
    tokio::select! {
        () = shutdown_signal() => {}
        () = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let idle = unix_now().saturating_sub(activity.load(Ordering::Relaxed));
                if idle >= idle_seconds {
                    tracing::info!(idle, "idle timeout reached, shutting down");
                    return;
                }
            }
        } => {}
    }
}

/// Resolve on SIGINT (Ctrl-C) or, on Unix, SIGTERM.
///
/// Containers and service managers send SIGTERM: without watching it, `docker
/// stop` or `systemctl stop` would kill the process after the grace period
/// instead of letting in-flight requests finish.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot watch SIGTERM, only Ctrl-C is handled");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
}

/// A one-time startup notice for the selected profile. `None` means no extra
/// notice: Standard ships only reviewed keyless entries and Clean makes no
/// external provider requests.
fn profile_notice(profile: Profile) -> Option<&'static str> {
    match profile {
        Profile::Full => Some(
            "NOTICE: the `full` profile enables HTML providers whose terms may forbid \
             automated access, in addition to the standard set. Review each provider's \
             conditions in docs/PROVIDERS.md before sending queries; personal use does \
             not override provider terms.",
        ),
        Profile::Standard | Profile::Clean => None,
    }
}

/// Print the profile notice once, to stderr so MCP stdio transport stays clean.
fn print_profile_notice(profile: Profile) {
    if let Some(notice) = profile_notice(profile) {
        eprintln!("{notice}");
        tracing::warn!("{}", notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_on(bind: &str, port: u16, base_url: Option<&str>) -> Settings {
        let mut settings = Settings::default();
        settings.server.bind_address = bind.into();
        settings.server.port = port;
        settings.server.base_url = base_url.map(str::to_string);
        settings
    }

    #[test]
    fn cli_port_reaches_the_generated_base_url() {
        let mut settings = settings_on("127.0.0.1", 29483, None);
        let address = apply_listener(&mut settings, None, Some(9000)).unwrap();
        assert_eq!(address, "127.0.0.1:9000".parse::<SocketAddr>().unwrap());
        assert_eq!(settings.base_url(), "http://127.0.0.1:9000");
    }

    #[test]
    fn cli_bind_and_port_reach_the_generated_base_url() {
        let mut settings = settings_on("127.0.0.1", 29483, None);
        let address = apply_listener(&mut settings, Some("0.0.0.0".into()), Some(8123)).unwrap();
        assert_eq!(address, "0.0.0.0:8123".parse::<SocketAddr>().unwrap());
        // A wildcard bind is not a reachable host; the generated URL uses localhost.
        assert_eq!(settings.base_url(), "http://localhost:8123");
    }

    #[test]
    fn explicit_public_base_url_survives_listener_overrides() {
        let public = "https://search.example.org/";
        let mut settings = settings_on("127.0.0.1", 29483, Some(public));
        let address = apply_listener(&mut settings, Some("0.0.0.0".into()), Some(9000)).unwrap();
        assert_eq!(address, "0.0.0.0:9000".parse::<SocketAddr>().unwrap());
        assert_eq!(settings.server.base_url.as_deref(), Some(public));
        assert_eq!(settings.base_url(), public);
    }

    #[test]
    fn without_overrides_the_configured_listener_is_used() {
        let mut settings = settings_on("127.0.0.1", 29483, None);
        let address = apply_listener(&mut settings, None, None).unwrap();
        assert_eq!(address, "127.0.0.1:29483".parse::<SocketAddr>().unwrap());
        assert_eq!(settings.base_url(), "http://127.0.0.1:29483");
    }

    #[test]
    fn invalid_bind_address_is_reported() {
        let mut settings = settings_on("127.0.0.1", 29483, None);
        let error = apply_listener(&mut settings, Some("not an ip".into()), None).unwrap_err();
        assert!(error.contains("not an ip:29483"), "{error}");
    }

    #[test]
    fn full_profile_notice_warns_about_provider_terms() {
        let notice = profile_notice(Profile::Full).expect("full prints a notice");
        assert!(notice.contains("HTML providers"), "{notice}");
        assert!(notice.contains("docs/PROVIDERS.md"), "{notice}");
        assert!(notice.contains("terms"), "{notice}");
        assert!(profile_notice(Profile::Standard).is_none());
        assert!(profile_notice(Profile::Clean).is_none());
    }
}
