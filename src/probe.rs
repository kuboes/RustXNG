//! Engine health probe for `rustxng --check-engines` (U5).
//!
//! The probe asks every enabled engine once with the same query and classifies
//! what happened: an answer, an empty answer, a block page, a broken endpoint,
//! a timeout, or an engine that still needs its key / base URL. It is a
//! diagnostic, not a search: responses are fetched without a cache, so nothing
//! is written to the response cache, and no engine health state or metric is
//! recorded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use serde::Serialize;

use crate::aggregator::Searcher;
use crate::engine::{EngineHandle, EngineItem, EngineRegistry};
use crate::network::NetworkError;
use crate::query::Query;
use crate::settings::Settings;

/// Query used when `--query` is not given.
pub const DEFAULT_QUERY: &str = "rust";
/// Per-engine deadline when `--timeout` is not given.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Probes in flight at once.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// The health of one engine for the probe query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    /// The engine answered with at least one result.
    Ok,
    /// The engine answered with HTTP 2xx but returned no result.
    Empty,
    /// HTTP 401/403/429 or a CAPTCHA / challenge body.
    Blocked,
    /// A connection/DNS error, a parse failure or another HTTP error.
    Broken,
    /// No answer within the per-engine deadline.
    Timeout,
    /// The engine still needs an api key or a base URL.
    NeedsConfig,
}

impl ProbeStatus {
    /// Every status, in report order.
    pub const ALL: [ProbeStatus; 6] = [
        Self::Ok,
        Self::Empty,
        Self::Blocked,
        Self::Broken,
        Self::Timeout,
        Self::NeedsConfig,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
            Self::Blocked => "blocked",
            Self::Broken => "broken",
            Self::Timeout => "timeout",
            Self::NeedsConfig => "needs_config",
        }
    }
}

/// Options for one sweep.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Query sent to every engine.
    pub query: String,
    /// Upper bound for one engine, including request construction and parsing.
    pub timeout: Duration,
    /// Probes in flight at once.
    pub concurrency: usize,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            query: DEFAULT_QUERY.to_string(),
            timeout: DEFAULT_TIMEOUT,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

/// The outcome for one engine.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub engine: String,
    pub status: ProbeStatus,
    pub results: usize,
    pub latency_ms: u64,
    pub error: Option<String>,
}

impl ProbeReport {
    /// One output line for the human readable report.
    pub fn line(&self) -> String {
        let mut line = format!(
            "{:<12} {:<28} {:>4} results {:>6} ms",
            self.status.as_str(),
            self.engine,
            self.results,
            self.latency_ms
        );
        if let Some(error) = &self.error {
            line.push_str("  ");
            line.push_str(error);
        }
        line
    }

    /// A report for an engine that could not even be built.
    fn from_error(engine: String, error: &str) -> Self {
        let (status, results, error) = outcome_from_error(error);
        Self {
            engine,
            status,
            results,
            latency_ms: 0,
            error,
        }
    }
}

/// The result of one sweep.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeSummary {
    pub query: String,
    /// One entry per enabled engine, sorted by name.
    pub engines: Vec<ProbeReport>,
}

impl ProbeSummary {
    /// Number of engines per status, with zero entries for statuses that did
    /// not occur so the JSON shape is stable.
    pub fn totals(&self) -> BTreeMap<&'static str, usize> {
        let mut totals: BTreeMap<&'static str, usize> = ProbeStatus::ALL
            .into_iter()
            .map(|status| (status.as_str(), 0))
            .collect();
        for report in &self.engines {
            if let Some(count) = totals.get_mut(report.status.as_str()) {
                *count += 1;
            }
        }
        totals
    }

    /// The JSON summary printed by `--json`.
    pub fn to_json(&self, profile: &str) -> serde_json::Value {
        serde_json::json!({
            "profile": profile,
            "query": self.query,
            "total": self.engines.len(),
            "totals": self.totals(),
            "engines": &self.engines,
        })
    }
}

/// Probe every enabled engine once.
///
/// Engines that could not be built at startup are reported too: a missing api
/// key or base URL is `needs_config`, anything else is `broken`. Nothing is
/// persisted; the caller only has to print the summary.
pub async fn run(searcher: &Searcher, options: &ProbeOptions) -> ProbeSummary {
    let query = Arc::new(query_for(
        &searcher.settings,
        &searcher.registry,
        &options.query,
    ));
    let failed: Vec<(String, String)> = searcher.registry.failed().to_vec();

    let mut reports: Vec<ProbeReport> = stream::iter(
        searcher
            .registry
            .iter()
            .cloned()
            .map(|engine| probe_engine(searcher, engine, Arc::clone(&query), options)),
    )
    .buffer_unordered(options.concurrency.max(1))
    .collect()
    .await;

    reports.extend(
        failed
            .into_iter()
            .map(|(engine, error)| ProbeReport::from_error(engine, &error)),
    );
    reports.sort_by_key(|report| report.engine.to_lowercase());
    ProbeSummary {
        query: options.query.clone(),
        engines: reports,
    }
}

/// The query every engine sees: parsed once, with the UI language as locale so
/// engines that pick a region behave like a normal search.
fn query_for(settings: &Settings, registry: &EngineRegistry, text: &str) -> Query {
    let mut query = Query::parse(text, registry, &[]);
    query.locale = settings.ui.default_locale.clone();
    query
}

async fn probe_engine(
    searcher: &Searcher,
    engine: EngineHandle,
    query: Arc<Query>,
    options: &ProbeOptions,
) -> ProbeReport {
    let name = engine.name().to_string();
    let started = Instant::now();
    let deadline = engine.timeout().min(options.timeout);
    let outcome = tokio::time::timeout(
        options.timeout,
        probe_once(searcher, &engine, &query, deadline),
    )
    .await;
    let latency_ms = millis(started.elapsed());
    let (status, results, error) = match outcome {
        Ok(outcome) => outcome,
        Err(_) => (
            ProbeStatus::Timeout,
            0,
            Some(format!(
                "no answer within {}s",
                options.timeout.as_secs_f64()
            )),
        ),
    };
    ProbeReport {
        engine: name,
        status,
        results,
        latency_ms,
        error,
    }
}

/// Probe one engine, returning (status, result count, short error).
async fn probe_once(
    searcher: &Searcher,
    engine: &EngineHandle,
    query: &Query,
    deadline: Duration,
) -> (ProbeStatus, usize, Option<String>) {
    if engine.is_offline() {
        let handle = Arc::clone(engine);
        let query = query.clone();
        let joined = tokio::time::timeout(
            deadline,
            tokio::task::spawn_blocking(move || handle.search_offline(&query, 1)),
        )
        .await;
        return match joined {
            Ok(Ok(Ok(items))) => outcome_from_items(items),
            Ok(Ok(Err(error))) => outcome_from_error(&error.to_string()),
            Ok(Err(_)) => (
                ProbeStatus::Broken,
                0,
                Some("offline engine task failed".into()),
            ),
            Err(_) => (
                ProbeStatus::Timeout,
                0,
                Some("offline engine timed out".into()),
            ),
        };
    }
    // Engines that only answer their own syntax (a translation pair, a
    // currency conversion, ...) are not unhealthy when this query is not for
    // them; the aggregator skips them the same way.
    if !engine.is_applicable(query) {
        return (
            ProbeStatus::Empty,
            0,
            Some("not applicable to this query".into()),
        );
    }
    let request = match engine.build_request(query, 1) {
        Ok(request) => request,
        Err(error) => return outcome_from_error(&error.to_string()),
    };
    match searcher
        .network
        .fetch(request, deadline, None, engine.proxy())
        .await
    {
        Ok(response) => {
            let body = body_snippet(&response.body);
            match engine.parse(&response, query) {
                Ok(items) => {
                    let results = items
                        .iter()
                        .filter(|item| matches!(item, EngineItem::Result(_)))
                        .count();
                    let status = classify(Some(response.status), &body, None, results);
                    (status, results, None)
                }
                Err(error) => {
                    let text = short_error(&error.to_string());
                    let status = classify(Some(response.status), &body, Some(&text), 0);
                    (status, 0, Some(text))
                }
            }
        }
        Err(error) => {
            let (status, body) = match &error {
                NetworkError::Status { status, body, .. } => (Some(*status), body.as_str()),
                _ => (None, ""),
            };
            let text = short_error(&error.to_string());
            let status = classify(status, body, Some(&text), 0);
            (status, 0, Some(text))
        }
    }
}

fn outcome_from_items(items: Vec<EngineItem>) -> (ProbeStatus, usize, Option<String>) {
    let results = items
        .iter()
        .filter(|item| matches!(item, EngineItem::Result(_)))
        .count();
    (classify(Some(200), "", None, results), results, None)
}

fn outcome_from_error(error: &str) -> (ProbeStatus, usize, Option<String>) {
    let text = short_error(error);
    (classify(None, "", Some(&text), 0), 0, Some(text))
}

/// Classify one probe outcome from what the request saw.
///
/// `status` is the HTTP status when a response arrived, `body` a short snippet of
/// the response body, `error` the short error text (request, parse or engine
/// construction) and `results` the number of parsed result items.
pub fn classify(
    status: Option<u16>,
    body: &str,
    error: Option<&str>,
    results: usize,
) -> ProbeStatus {
    let error = error.unwrap_or_default().to_ascii_lowercase();
    if is_timeout(&error) {
        return ProbeStatus::Timeout;
    }
    if needs_config(&error) {
        return ProbeStatus::NeedsConfig;
    }
    if is_blocked(&error) || is_captcha(body) {
        return ProbeStatus::Blocked;
    }
    if !error.is_empty() {
        return ProbeStatus::Broken;
    }
    match status {
        Some(401 | 403 | 429) => ProbeStatus::Blocked,
        Some(200..=299) if results == 0 => ProbeStatus::Empty,
        Some(200..=299) => ProbeStatus::Ok,
        _ => ProbeStatus::Broken,
    }
}

fn is_timeout(error: &str) -> bool {
    ["timed out", "timeout", "deadline"]
        .iter()
        .any(|needle| error.contains(needle))
}

/// Whether the engine is only built because the operator has not configured
/// the endpoint or the credentials it needs, so the failure says nothing about
/// the provider's health.
///
/// An engine may report the missing option in its own words (`requires the
/// latitude and longitude options`), so engine-construction errors are matched
/// by their "requires ... option(s)" shape as well as by the option names.
fn needs_config(error: &str) -> bool {
    ["api_key", "api key", "base_url", "client_id", "credentials"]
        .iter()
        .any(|needle| error.contains(needle))
        || (error.contains("requires ")
            && (error.contains(" option")
                || error.contains(" setting")
                || error.contains(" instance")))
}

fn is_blocked(error: &str) -> bool {
    [
        "captcha",
        "access denied",
        "too many requests",
        "rate limit",
        "forbidden",
        "http status 401",
        "http status 403",
        "http status 429",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn is_captcha(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    [
        "captcha",
        "are you a robot",
        "unusual traffic",
        "verify you are human",
        "cf-challenge",
        "challenge-platform",
    ]
    .iter()
    .any(|needle| body.contains(needle))
}

/// First part of a body, enough to spot a challenge page.
fn body_snippet(body: &[u8]) -> String {
    String::from_utf8_lossy(&body[..body.len().min(4096)]).to_string()
}

/// Collapse whitespace, redact credentials and shorten an error for one line.
fn short_error(text: &str) -> String {
    let text = redact(text);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const LIMIT: usize = 160;
    if collapsed.chars().count() <= LIMIT {
        collapsed
    } else {
        let mut short: String = collapsed.chars().take(LIMIT - 3).collect();
        short.push_str("...");
        short
    }
}

/// Replace the value of common secret query parameters with `***`.
///
/// Request errors can contain the engine URL, and some engines put their key
/// in the query string; the diagnostic must not print it.
fn redact(text: &str) -> String {
    const SECRETS: [&str; 7] = [
        "api_key=",
        "apikey=",
        "access_token=",
        "client_id=",
        "token=",
        "key=",
        "secret=",
    ];
    let lowered = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    loop {
        let next = SECRETS
            .iter()
            .filter_map(|secret| {
                lowered[cursor..]
                    .find(secret)
                    .map(|at| (cursor + at, *secret))
            })
            .min_by_key(|(at, _)| *at);
        let Some((at, secret)) = next else {
            break;
        };
        let value_start = at + secret.len();
        let value_end = text[value_start..]
            .find(|character: char| character == '&' || character.is_whitespace())
            .map(|offset| value_start + offset)
            .unwrap_or(text.len());
        output.push_str(&text[cursor..value_start]);
        output.push_str("***");
        cursor = value_end;
    }
    output.push_str(&text[cursor..]);
    output
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_rate_limit_statuses_are_blocked() {
        for status in [401, 403, 429] {
            assert_eq!(
                classify(Some(status), "", None, 0),
                ProbeStatus::Blocked,
                "HTTP {status}"
            );
        }
    }

    #[test]
    fn challenge_bodies_are_blocked_even_on_http_200() {
        assert_eq!(
            classify(Some(200), "<html>please solve the captcha</html>", None, 0),
            ProbeStatus::Blocked
        );
        assert_eq!(
            classify(Some(200), "unusual traffic from your network", None, 0),
            ProbeStatus::Blocked
        );
        assert_eq!(
            classify(None, "", Some("captcha required"), 0),
            ProbeStatus::Blocked
        );
    }

    #[test]
    fn connection_and_dns_errors_are_broken() {
        for error in [
            "transport error: dns error: failed to lookup address information",
            "transport error: connection refused",
            "request could not be built: relative URL without a base",
            "engine request failed: connection reset by peer",
        ] {
            assert_eq!(
                classify(None, "", Some(error), 0),
                ProbeStatus::Broken,
                "{error}"
            );
        }
    }

    #[test]
    fn parse_failures_are_broken() {
        assert_eq!(
            classify(
                Some(200),
                "<html>not the expected shape</html>",
                Some("engine response could not be parsed: invalid JSON response"),
                0
            ),
            ProbeStatus::Broken
        );
    }

    #[test]
    fn timeouts_are_timeouts() {
        assert_eq!(
            classify(None, "", Some("request timed out"), 0),
            ProbeStatus::Timeout
        );
        assert_eq!(
            classify(None, "", Some("deadline has elapsed"), 0),
            ProbeStatus::Timeout
        );
    }

    #[test]
    fn missing_key_or_base_url_is_needs_config() {
        for error in [
            "deepl requires an api_key",
            "deepseek requires an api_key or DEEPSEEK_API_KEY",
            "discourse requires a base_url setting",
            "missing required config `base_url`",
            "giphy: an api_key is required",
            "engine request failed: open_meteo requires the latitude and longitude options",
            "anaconda requires a self-hosted instance",
        ] {
            assert_eq!(
                classify(None, "", Some(error), 0),
                ProbeStatus::NeedsConfig,
                "{error}"
            );
        }
    }

    #[test]
    fn a_two_hundred_answer_is_ok_with_results_and_empty_without() {
        assert_eq!(
            classify(Some(200), r#"{"results":[]}"#, None, 0),
            ProbeStatus::Empty
        );
        assert_eq!(
            classify(Some(200), r#"{"results":[...]}"#, None, 3),
            ProbeStatus::Ok
        );
    }

    #[test]
    fn server_errors_are_broken() {
        for status in [400, 404, 500, 503] {
            assert_eq!(
                classify(Some(status), "", None, 0),
                ProbeStatus::Broken,
                "HTTP {status}"
            );
        }
    }

    #[test]
    fn a_403_with_an_access_denied_message_is_blocked() {
        assert_eq!(
            classify(None, "", Some("access denied"), 0),
            ProbeStatus::Blocked
        );
        assert_eq!(
            classify(None, "", Some("too many requests"), 0),
            ProbeStatus::Blocked
        );
    }

    #[test]
    fn errors_are_shortened_and_credentials_are_redacted() {
        let long = format!(
            "transport error: https://example.invalid/search?q=rust&key=secret-value&x=1 {}",
            "x".repeat(300)
        );
        let short = short_error(&long);
        assert!(short.len() <= 160, "{short}");
        assert!(!short.contains("secret-value"), "{short}");
        assert!(short.contains("key=***"), "{short}");
        assert!(short.contains("https://example.invalid/search"), "{short}");
    }
}
