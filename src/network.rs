//! HTTP layer, ported from `searx/network/`.
//!
//! Responsibilities kept from upstream: rotating user agents, per request
//! timeout, proxies, a redirect limit, retries on transport errors and a
//! response cache.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use reqwest::dns::Resolve;
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::cache::{Cache, CachedResponse};
use crate::engine::{EngineRequest, EngineResponse, FailureKind, Method};
use crate::settings::Settings;

const USER_AGENT_OS: &[&str] = &[
    "Windows NT 10.0; Win64; x64",
    "X11; Linux x86_64",
    "Macintosh; Intel Mac OS X 10.15",
    "X11; Ubuntu; Linux x86_64",
];
const USER_AGENT_VERSIONS: &[&str] = &["157.0", "156.0", "155.0", "154.0"];
const USER_AGENT_TEMPLATE: &str =
    "Mozilla/5.0 ({os}; rv:{version}) Gecko/20100101 Firefox/{version}";

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("request timed out")]
    Timeout,
    #[error("response exceeds the {0} byte decoded body limit")]
    TooLarge(usize),
    #[error("http status {status}")]
    Status {
        status: u16,
        body: String,
        response: Box<EngineResponse>,
    },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("request could not be built: {0}")]
    Build(String),
}

impl NetworkError {
    /// Map a network error onto the framework failure kinds.
    pub fn failure_kind(&self) -> FailureKind {
        match self {
            Self::Timeout => FailureKind::Timeout,
            Self::TooLarge(_) => FailureKind::Other,
            Self::Build(_) => FailureKind::Other,
            Self::Transport(_) => FailureKind::Other,
            Self::Status { status, body, .. } => classify_status(*status, body),
        }
    }
}

/// Classify an HTTP status (with a body excerpt) the way upstream does.
pub fn classify_status(status: u16, body: &str) -> FailureKind {
    let lowered = body.to_lowercase();
    let captcha = lowered.contains("captcha")
        || lowered.contains("anomaly")
        || lowered.contains("unusual traffic")
        || lowered.contains("are you a robot");
    match status {
        403 if captcha => FailureKind::Captcha,
        401..=403 => FailureKind::AccessDenied,
        429 => FailureKind::TooManyRequests,
        500..=599 => FailureKind::Http,
        _ => FailureKind::Http,
    }
}

/// The HTTP client used by every engine.
pub struct Network {
    client: reqwest::Client,
    /// Direct-only client for third-party media; no engine host exemptions.
    media_client: reqwest::Client,
    media_proxy_blocked: bool,
    /// Clients for engines that need their own proxy, keyed by proxy URL.
    proxied: std::sync::Mutex<std::collections::HashMap<String, reqwest::Client>>,
    /// Settings kept so a proxied client can be rebuilt on demand.
    outgoing: crate::settings::Outgoing,
    user_agents: Vec<String>,
    suffix: String,
    retries: u32,
    counter: AtomicUsize,
    /// Fixed, honest User-Agent used instead of the rotating pool.
    identified: Option<String>,
    /// The honest User-Agent sent to hosts that publish an API policy, even
    /// when the instance as a whole does not identify itself.
    api_agent: String,
    /// Minimum pause between two requests to one host.
    host_spacing: Duration,
    /// Per-domain exceptions to `host_spacing` (domain, pause).
    host_spacing_overrides: Vec<(String, Duration)>,
    /// Earliest start of the next request per host.
    host_slots: std::sync::Mutex<std::collections::HashMap<String, tokio::time::Instant>>,
    /// Concurrent media reads (`fetch_image`).
    media_slots: Arc<tokio::sync::Semaphore>,
    /// Resolver shared by the engine clients: one lookup, non-global
    /// addresses are dropped from the answer (an answer with no global
    /// address fails), unless the name is a configured engine endpoint.
    resolver: Arc<dyn Resolve>,
    /// Hosts of the enabled engine endpoints (base_url and search_url).
    engine_hosts: Arc<std::collections::BTreeSet<String>>,
}

/// Decoded body bytes accepted from one engine response.
///
/// An engine answer is parsed whole, so an unbounded body is operator-visible
/// memory: one hostile or compromised upstream could inflate the process by
/// the size of its reply. 8 MiB is far above any real search answer.
const ENGINE_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Whether two URLs are the same origin (scheme, host, effective port).
fn is_same_origin(previous: Option<&reqwest::Url>, target: &reqwest::Url) -> bool {
    previous.is_some_and(|from| {
        from.scheme() == target.scheme()
            && from
                .host_str()
                .zip(target.host_str())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            && from.port_or_known_default() == target.port_or_known_default()
    })
}

/// Whether an engine redirect may follow from `previous` to `target`.
///
/// Same-origin hops are routine canonicalisation and keep a private endpoint
/// working. Everything else has to be public; a proxy hides the real
/// destination from the local resolver, and another configured engine is a
/// trusted destination but never a redirect target.
fn redirect_allowed(
    previous: Option<&reqwest::Url>,
    target: &reqwest::Url,
    through_proxy: bool,
    engine_hosts: &std::collections::BTreeSet<String>,
) -> bool {
    if is_same_origin(previous, target) {
        return true;
    }
    if through_proxy {
        return false;
    }
    let host = target
        .host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase());
    if host.is_some_and(|host| engine_hosts.contains(&host)) {
        return false;
    }
    crate::fetch::is_fetchable(target)
}

/// Hand the shared resolver to reqwest, whose builder wants a sized type.
struct SharedResolver(Arc<dyn Resolve>);

impl Resolve for SharedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.0.resolve(name)
    }
}

/// The hosts of the engine endpoints the operator enabled.
///
/// Those names are trusted configuration: a self-hosted engine may answer
/// with a private address. Only enabled engines qualify, and only the host
/// is kept. Redirects, result URLs, media and MCP reads never consult this
/// list; a redirect that crosses origins to one of these hosts is refused.
fn configured_engine_hosts(settings: &Settings) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for engine in settings.enabled_engines() {
        for template in [engine.base_url.as_deref(), engine.search_url.as_deref()]
            .into_iter()
            .flatten()
        {
            // A template holds placeholders such as {query}; replacing them
            // keeps the authority intact and lets one parse cover both shapes.
            let expanded = template
                .replace("{query}", "x")
                .replace("{pageno}", "1")
                .replace("{language}", "en")
                .replace("{locale}", "en")
                .replace("{time_range}", "")
                .replace("{safesearch}", "0");
            let Ok(url) = reqwest::Url::parse(&expanded) else {
                continue;
            };
            if let Some(host) = url.host_str() {
                let host = host.trim_end_matches('.').to_ascii_lowercase();
                if !host.is_empty() && !hosts.iter().any(|known| known == &host) {
                    hosts.push(host);
                }
            }
        }
    }
    hosts
}

fn engine_redirect_policy(
    max_redirects: usize,
    through_proxy: bool,
    engine_hosts: Arc<std::collections::BTreeSet<String>>,
) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= max_redirects {
            return attempt.error("too many redirects");
        }
        if !redirect_allowed(
            attempt.previous().last(),
            attempt.url(),
            through_proxy,
            &engine_hosts,
        ) {
            return attempt.error("redirect target is not an allowed destination");
        }
        attempt.follow()
    })
}

/// Concurrent media reads allowed in one process.
///
/// Every media response is buffered in memory (up to 8 MiB for an image), so
/// an unbounded fan-out from `/image_proxy` or `/favicon_proxy` is a memory and
/// upstream-load amplifier. Four keeps the worst case near 32 MiB.
const MAX_CONCURRENT_MEDIA: usize = 4;

/// Build a client from the outgoing settings, optionally through another proxy.
fn build_client(
    outgoing: &crate::settings::Outgoing,
    extra_proxy: Option<reqwest::Proxy>,
    redirects: reqwest::redirect::Policy,
    resolver: Option<Arc<dyn Resolve>>,
) -> Result<reqwest::Client, NetworkError> {
    let mut builder = reqwest::Client::builder()
        .redirect(redirects)
        .timeout(Duration::from_secs_f64(
            outgoing.max_request_timeout.max(1.0),
        ))
        .connect_timeout(Duration::from_secs_f64(
            (outgoing.request_timeout / 2.0).clamp(1.0, 5.0),
        ))
        .pool_max_idle_per_host(outgoing.pool_maxsize)
        .danger_accept_invalid_certs(!outgoing.verify)
        .user_agent(default_user_agent());
    if let Some(path) = &outgoing.ca_bundle {
        let pem = std::fs::read(path)
            .map_err(|e| NetworkError::Build(format!("cannot read CA bundle {path}: {e}")))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| NetworkError::Build(format!("invalid CA bundle: {e}")))?;
        if certificates.is_empty() {
            return Err(NetworkError::Build(
                "CA bundle contains no certificates".into(),
            ));
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    if !outgoing.enable_http2 {
        builder = builder.http1_only();
    }
    if let Some(resolver) = resolver {
        builder = builder.dns_resolver(Arc::new(SharedResolver(resolver)));
    }
    if let Some(proxy) = extra_proxy {
        builder = builder.proxy(proxy);
    } else {
        for (scheme, address) in &outgoing.proxies {
            // `proxies` is keyed per scheme. Registering every entry with
            // `Proxy::all` made the first key (BTreeMap order) capture every
            // request, so an intended `https` route silently used `http`.
            let scheme = scheme.trim().to_ascii_lowercase();
            let proxy = match scheme.as_str() {
                "all" => reqwest::Proxy::all(address),
                "http" => reqwest::Proxy::http(address),
                "https" => reqwest::Proxy::https(address),
                other => {
                    return Err(NetworkError::Build(format!(
                        "unsupported proxy key {other:?}: use all, http or https"
                    )));
                }
            };
            let proxy = proxy.map_err(|error| NetworkError::Build(error.to_string()))?;
            tracing::debug!(scheme, "configured proxy");
            builder = builder.proxy(proxy);
        }
    }
    builder
        .build()
        .map_err(|error| NetworkError::Build(error.to_string()))
}

/// A normal (non-public-only) client for a trusted API endpoint.
/// It honours the outgoing proxy, TLS and CA settings, but never follows
/// redirects: the configured endpoint is fixed and a redirect must not make
/// the local process contact another destination.
pub(crate) fn api_client(
    outgoing: &crate::settings::Outgoing,
) -> Result<reqwest::Client, NetworkError> {
    build_client(outgoing, None, reqwest::redirect::Policy::none(), None)
}

impl Network {
    #[cfg(test)]
    pub(crate) fn with_test_media_client(mut self, client: reqwest::Client) -> Self {
        self.media_client = client;
        self.media_proxy_blocked = false;
        self
    }

    pub fn from_settings(settings: &Settings) -> Result<Self, NetworkError> {
        let outgoing = &settings.outgoing;
        let hosts = configured_engine_hosts(settings);
        let resolver = crate::fetch::validated_resolver(hosts.clone());
        let engine_hosts = Arc::new(hosts.into_iter().collect::<std::collections::BTreeSet<_>>());
        let through_proxy = !outgoing.proxies.is_empty();
        let client = build_client(
            outgoing,
            None,
            engine_redirect_policy(outgoing.max_redirects, through_proxy, engine_hosts.clone()),
            Some(resolver.clone()),
        )?;
        let (media_client, media_proxy_blocked) =
            crate::fetch::public_client(outgoing, &[]).map_err(NetworkError::Build)?;
        Ok(Self {
            client,
            resolver,
            engine_hosts,
            media_client,
            media_proxy_blocked,
            proxied: std::sync::Mutex::new(std::collections::HashMap::new()),
            outgoing: outgoing.clone(),
            user_agents: build_user_agents(),
            suffix: outgoing.useragent_suffix.clone(),
            retries: outgoing.retries,
            counter: AtomicUsize::new(0),
            api_agent: identified_user_agent(&outgoing.contact),
            identified: outgoing
                .identified_ua
                .then(|| identified_user_agent(&outgoing.contact)),
            host_spacing: Duration::from_millis(outgoing.host_spacing_ms),
            host_spacing_overrides: outgoing
                .host_spacing_overrides
                .iter()
                .map(|(domain, millis)| {
                    (
                        domain.trim().trim_start_matches('.').to_lowercase(),
                        Duration::from_millis(*millis),
                    )
                })
                .collect(),
            host_slots: std::sync::Mutex::new(std::collections::HashMap::new()),
            media_slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_MEDIA)),
        })
    }

    /// The pause between two requests to `host`: the most specific override
    /// (exact host or a parent domain), else the general spacing.
    fn spacing_for(&self, host: &str) -> Duration {
        self.host_spacing_overrides
            .iter()
            .filter(|(domain, _)| {
                host == domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            })
            .max_by_key(|(domain, _)| domain.len())
            .map_or(self.host_spacing, |(_, spacing)| *spacing)
    }

    /// Reserve this host's next request slot and wait for it.
    ///
    /// Returns `Timeout` when the slot starts after the request's own budget,
    /// so a busy host is skipped instead of being asked early.
    async fn wait_for_host_slot(&self, url: &str, budget: Duration) -> Result<(), NetworkError> {
        if self.host_spacing.is_zero() && self.host_spacing_overrides.is_empty() {
            return Ok(());
        }
        let Some(host) = reqwest::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
        else {
            return Ok(());
        };
        let spacing = self.spacing_for(&host);
        if spacing.is_zero() {
            return Ok(());
        }
        let now = tokio::time::Instant::now();
        let slot = {
            let Ok(mut slots) = self.host_slots.lock() else {
                return Ok(());
            };
            let next = slots.get(&host).copied().unwrap_or(now).max(now);
            if next.saturating_duration_since(now) >= budget {
                return Err(NetworkError::Timeout);
            }
            slots.insert(host, next + spacing);
            next
        };
        tokio::time::sleep_until(slot).await;
        Ok(())
    }

    /// The client for one request, honouring a per-engine proxy.
    ///
    /// Engines share the default client; a per-engine proxy gets its own so a
    /// blocked engine can be routed elsewhere without moving the rest.
    fn client_for(&self, proxy: Option<&str>) -> Result<reqwest::Client, NetworkError> {
        let Some(proxy) = proxy.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(self.client.clone());
        };
        if let Ok(cache) = self.proxied.lock() {
            if let Some(client) = cache.get(proxy) {
                return Ok(client.clone());
            }
        }
        let configured =
            reqwest::Proxy::all(proxy).map_err(|error| NetworkError::Build(error.to_string()))?;
        let client = build_client(
            &self.outgoing,
            Some(configured),
            engine_redirect_policy(self.outgoing.max_redirects, true, self.engine_hosts.clone()),
            Some(self.resolver.clone()),
        )?;
        if let Ok(mut cache) = self.proxied.lock() {
            cache.insert(proxy.to_string(), client.clone());
        }
        Ok(client)
    }

    /// A different user agent on every call, like upstream's rotation.
    pub fn user_agent(&self) -> String {
        if let Some(identified) = &self.identified {
            return identified.clone();
        }
        if self.user_agents.is_empty() {
            return default_user_agent();
        }
        let index = self.counter.fetch_add(1, Ordering::Relaxed) % self.user_agents.len();
        format!("{} {}", self.user_agents[index], self.suffix)
            .trim()
            .to_string()
    }

    fn default_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static(
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("en-US,en;q=0.7"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("dnt"),
            HeaderValue::from_static("1"),
        );
        if let Ok(value) = HeaderValue::from_str(&self.user_agent()) {
            headers.insert(reqwest::header::USER_AGENT, value);
        }
        headers
    }

    /// Fetch a request, using the cache when one is given.
    pub async fn fetch(
        &self,
        request: EngineRequest,
        timeout: Duration,
        cache: Option<&Cache>,
        proxy: Option<&str>,
    ) -> Result<EngineResponse, NetworkError> {
        self.fetch_inner(
            request,
            timeout,
            cache,
            proxy,
            Some(ENGINE_MAX_BODY_BYTES),
            false,
        )
        .await
    }

    /// Fetch an uncached image with a body limit.
    pub(crate) async fn fetch_image(
        &self,
        request: EngineRequest,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<EngineResponse, NetworkError> {
        self.fetch_inner(request, timeout, None, None, Some(max_bytes), true)
            .await
    }

    async fn fetch_inner(
        &self,
        request: EngineRequest,
        timeout: Duration,
        cache: Option<&Cache>,
        proxy: Option<&str>,
        max_bytes: Option<usize>,
        media: bool,
    ) -> Result<EngineResponse, NetworkError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let (client, media_slot) = if media {
            if self.media_proxy_blocked {
                return Err(NetworkError::Build(
                    "media reads require direct connections; configured proxies cannot validate destinations".into(),
                ));
            }
            let url = reqwest::Url::parse(&request.url)
                .map_err(|error| NetworkError::Build(error.to_string()))?;
            if !crate::fetch::is_fetchable(&url) {
                return Err(NetworkError::Build(
                    "only public http(s) media can be fetched".into(),
                ));
            }
            // Bound the concurrent buffered bodies; the permit is released
            // when this request finishes or its deadline expires.
            let slot = tokio::time::timeout_at(deadline, self.media_slots.clone().acquire_owned())
                .await
                .map_err(|_| NetworkError::Timeout)?
                .map_err(|_| NetworkError::Build("media fetch slots are closed".into()))?;
            (self.media_client.clone(), Some(slot))
        } else {
            (self.client_for(proxy)?, None)
        };
        let _media_slot = media_slot;
        let cache_key = cache
            .map(|_| request_cache_key(&request, proxy))
            .transpose()?;
        // The fast path needs no request lock. On a miss, only one caller for
        // this cache and full request identity contacts the origin at a time.
        let (_request_guard, mut stale) =
            if let (Some(cache), Some(key)) = (cache, cache_key.as_deref()) {
                if let Some(cached) = tokio::time::timeout_at(deadline, cache.get(key))
                    .await
                    .map_err(|_| NetworkError::Timeout)?
                {
                    return Ok(cached_response(cached));
                }
                let guard = tokio::time::timeout_at(deadline, cache.request_lock(key).lock_owned())
                    .await
                    .map_err(|_| NetworkError::Timeout)?;
                // A previous caller may have populated the cache while we waited.
                if let Some(cached) = tokio::time::timeout_at(deadline, cache.get(key))
                    .await
                    .map_err(|_| NetworkError::Timeout)?
                {
                    debug!(url = %request.url, "cache hit");
                    return Ok(cached_response(cached));
                }
                // An expired entry is still stored: its validators let the
                // origin answer 304 instead of resending the whole body.
                let stale = tokio::time::timeout_at(deadline, cache.get_with_stale(key))
                    .await
                    .map_err(|_| NetworkError::Timeout)?;
                (Some(guard), stale)
            } else {
                (None, None)
            };
        // Only GET requests are revalidated; POST results are refetched.
        let revalidation = if request.method == Method::Get {
            stale.as_ref().and_then(Revalidation::from_stale)
        } else {
            None
        };

        let mut attempt = 0;
        loop {
            attempt += 1;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(NetworkError::Timeout);
            }
            let outcome = match tokio::time::timeout_at(
                deadline,
                self.send(
                    client.clone(),
                    &request,
                    remaining,
                    max_bytes,
                    revalidation.as_ref(),
                ),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => return Err(NetworkError::Timeout),
            };
            match outcome {
                Ok(response) => {
                    if response.status == 304 && revalidation.is_some() {
                        // The origin confirms the stale body is unchanged:
                        // extend its TTL and reuse it without a download.
                        if let Some(stale) = stale.take() {
                            if let (Some(cache), Some(key)) = (cache, cache_key.as_deref()) {
                                if tokio::time::timeout_at(deadline, cache.refresh(key))
                                    .await
                                    .is_err()
                                {
                                    debug!(url = %request.url, "cache refresh timed out");
                                }
                            }
                            debug!(url = %request.url, "304 not modified, reusing cached response");
                            return Ok(cached_response(stale));
                        }
                    }
                    if let (Some(cache), Some(key)) = (cache, cache_key.as_deref()) {
                        if response.status == 200
                            && !response.body.is_empty()
                            && tokio::time::timeout_at(
                                deadline,
                                cache.set(
                                    key,
                                    CachedResponse {
                                        status: response.status,
                                        final_url: response.final_url.clone(),
                                        headers: response.headers.clone(),
                                        body: std::sync::Arc::new(response.body.clone()),
                                        etag: validator(&response.headers, "etag"),
                                        last_modified: validator(
                                            &response.headers,
                                            "last-modified",
                                        ),
                                    },
                                ),
                            )
                            .await
                            .is_err()
                        {
                            debug!(url = %request.url, "cache write timed out");
                        }
                    }
                    // A 429 usually clears within a second. Retrying once keeps
                    // a good engine (Yahoo! JAPAN, Bing) alive instead of
                    // suspending it for minutes.
                    if response.status == 429 && attempt <= self.retries {
                        // Retry-After を尊重する（RFC 9110）。無ければ従来どおり 700ms。
                        // 上流の制限を回避するためではなく、指示された時間だけ待つ。
                        let backoff = parse_retry_after(&response.headers)
                            .unwrap_or(Duration::from_millis(700));
                        if tokio::time::Instant::now() + backoff < deadline {
                            debug!(url = %request.url, ?backoff, "rate limited, retrying after Retry-After pause");
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                    }
                    if response.status >= 400 {
                        return Err(NetworkError::Status {
                            status: response.status,
                            body: String::from_utf8_lossy(
                                &response.body[..response.body.len().min(4096)],
                            )
                            .to_string(),
                            response: Box::new(response),
                        });
                    }
                    return Ok(response);
                }
                Err(error) if attempt <= self.retries && is_retryable(&error) => {
                    // never retry a timeout: the engine deadline already elapsed
                    if matches!(error, NetworkError::Timeout) {
                        return Err(error);
                    }
                    let backoff = Duration::from_millis(250 * attempt as u64);
                    if tokio::time::Instant::now() + backoff >= deadline {
                        return Err(error);
                    }
                    debug!(url = %request.url, %error, "retrying request");
                    tokio::time::sleep(backoff).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn send(
        &self,
        client: reqwest::Client,
        request: &EngineRequest,
        timeout: Duration,
        max_bytes: Option<usize>,
        revalidation: Option<&Revalidation>,
    ) -> Result<EngineResponse, NetworkError> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
        };
        self.wait_for_host_slot(&request.url, timeout).await?;
        let mut headers = self.default_headers();
        // Hosts that publish an API policy ask clients to name themselves, so
        // they never see the browser-like rotation.
        if is_documented_api_host(&request.url) {
            if let Ok(value) = HeaderValue::from_str(&self.api_agent) {
                headers.insert(reqwest::header::USER_AGENT, value);
            }
        }
        for (name, value) in request.headers.iter() {
            headers.insert(name.clone(), value.clone());
        }
        if !request.cookies.is_empty() {
            let cookie = request
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                headers.insert(reqwest::header::COOKIE, value);
            }
        }

        if let Some(revalidation) = revalidation {
            if let Some(etag) = &revalidation.etag {
                if let Ok(value) = HeaderValue::from_str(etag) {
                    headers.insert(reqwest::header::IF_NONE_MATCH, value);
                }
            }
            if let Some(last_modified) = &revalidation.last_modified {
                if let Ok(value) = HeaderValue::from_str(last_modified) {
                    headers.insert(reqwest::header::IF_MODIFIED_SINCE, value);
                }
            }
        }

        let mut builder = client.request(method, &request.url).headers(headers);
        if let Some(json) = &request.json {
            builder = builder.json(json);
        } else if request.method == Method::Post {
            builder = builder.form(&request.form);
        }

        let send = async {
            let mut response = builder.send().await.map_err(|error| {
                if error.is_timeout() {
                    NetworkError::Timeout
                } else {
                    NetworkError::Transport(error.to_string())
                }
            })?;
            let status = response.status().as_u16();
            let final_url = response.url().to_string();
            let mut headers = BTreeMap::new();
            for (name, value) in response.headers().iter() {
                if let Ok(value) = value.to_str() {
                    headers.insert(name.as_str().to_lowercase(), value.to_string());
                }
            }
            let body = if let Some(limit) = max_bytes {
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|error| NetworkError::Transport(error.to_string()))?
                {
                    if chunk.len() > limit - body.len() {
                        return Err(NetworkError::TooLarge(limit));
                    }
                    body.extend_from_slice(&chunk);
                }
                body
            } else {
                response
                    .bytes()
                    .await
                    .map_err(|error| NetworkError::Transport(error.to_string()))?
                    .to_vec()
            };
            Ok(EngineResponse {
                status,
                final_url,
                headers,
                body,
            })
        };

        match tokio::time::timeout(timeout, send).await {
            Ok(result) => result,
            Err(_) => Err(NetworkError::Timeout),
        }
    }
}

fn cached_response(cached: CachedResponse) -> EngineResponse {
    EngineResponse {
        status: cached.status,
        final_url: cached.final_url,
        headers: cached.headers,
        body: cached.body.as_ref().clone(),
    }
}

/// Validators from a stale cache entry, sent as conditional GET headers.
struct Revalidation {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Revalidation {
    fn from_stale(stale: &CachedResponse) -> Option<Self> {
        let etag = stale.etag.clone().filter(|value| !value.is_empty());
        let last_modified = stale
            .last_modified
            .clone()
            .filter(|value| !value.is_empty());
        if etag.is_none() && last_modified.is_none() {
            None
        } else {
            Some(Self {
                etag,
                last_modified,
            })
        }
    }
}

/// A cache validator from a response header, when present and non-empty.
fn validator(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `Retry-After`（RFC 9110 §10.2.3）を待機時間に変換する。
///
/// 秒数形式（`"120"`）に対応する。HTTP-date 形式は手軽に解釈できないため
/// パース不能扱いとし、呼び出し側のフォールバック（700ms）に任せる。
/// 異常に大きい値は 60 秒に頭打ちにする（サーバー設定ミスや悪意ある値で
/// 起動を長時間止めない。deadline が尽きればそもそもリトライしない）。
///
/// これは「待つべき秒数」のヒントであり、上流への礼儀（制限を尊重し、
/// 規制を回避しない）を守るためのもの。リトライ回数は増やさない。
fn parse_retry_after(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let value = headers.get("retry-after")?.trim();
    let secs: u64 = value.parse().ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

fn request_cache_key(request: &EngineRequest, proxy: Option<&str>) -> Result<String, NetworkError> {
    let mut headers: Vec<_> = request
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_bytes()))
        .collect();
    headers.sort_by_key(|(name, _)| *name);
    let identity = serde_json::to_vec(&(
        request.method,
        &request.url,
        headers,
        &request.cookies,
        &request.form,
        &request.json,
        proxy.map(str::trim),
    ))
    .map_err(|error| NetworkError::Build(error.to_string()))?;
    use std::fmt::Write;
    let mut key = String::with_capacity(64);
    for byte in Sha256::digest(identity) {
        write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(key)
}

fn is_retryable(error: &NetworkError) -> bool {
    match error {
        NetworkError::Transport(_) => true,
        // a timeout already consumed the whole engine budget
        NetworkError::Timeout => false,
        NetworkError::Status { status, .. } => *status == 429 || *status >= 500,
        NetworkError::Build(_) | NetworkError::TooLarge(_) => false,
    }
}

/// Documented API hosts where RustXNG uses its identified User-Agent.
const DOCUMENTED_API_SUFFIXES: &[&str] = &[
    "wikipedia.org",
    "wikidata.org",
    "wikimedia.org",
    "wikibooks.org",
    "wikinews.org",
    "wikiquote.org",
    "wikisource.org",
    "wikiversity.org",
    "wikivoyage.org",
    "wiktionary.org",
    "api.github.com",
    "crates.io",
    "pypi.org",
    "registry.npmjs.org",
    "api.npms.io",
    "api.crossref.org",
    "api.openalex.org",
    "api.semanticscholar.org",
    "api.stackexchange.com",
    "export.arxiv.org",
    "api.openverse.org",
    "api.openverse.engineering",
    "nominatim.openstreetmap.org",
    "photon.komoot.io",
    "api.open-meteo.com",
    "geocoding-api.open-meteo.com",
    "api.mwmbl.org",
    "api2.marginalia-search.com",
    "qiita.com",
    "hn.algolia.com",
    "www.ebi.ac.uk",
];

/// Whether the URL points at a host with a published API policy.
pub fn is_documented_api_host(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
    else {
        return false;
    };
    DOCUMENTED_API_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// The User-Agent of an instance that identifies itself (`identified_ua`).
pub fn identified_user_agent(contact: &str) -> String {
    let base = format!(
        "rustxng/{} (+https://github.com/RustXNG/RustXNG",
        env!("CARGO_PKG_VERSION")
    );
    match contact.trim() {
        "" => format!("{base})"),
        contact => format!("{base}; {contact})"),
    }
}

/// The default user agent, mirroring upstream's `searxng_useragent`.
pub fn default_user_agent() -> String {
    build_user_agents()
        .into_iter()
        .next()
        .unwrap_or_else(|| "rustxng".to_string())
}

/// Build the user agent pool like upstream's `data/useragents.json`.
fn build_user_agents() -> Vec<String> {
    let mut agents = Vec::new();
    for os in USER_AGENT_OS {
        for version in USER_AGENT_VERSIONS {
            agents.push(
                USER_AGENT_TEMPLATE
                    .replace("{os}", os)
                    .replace("{version}", version),
            );
        }
    }
    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_engine_proxy_does_not_contact_origin() {
        use axum::Router;
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::clone(&calls);
        let app = Router::new().fallback(move || {
            requests.fetch_add(1, Ordering::Relaxed);
            async { "origin" }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut network = Network::from_settings(&Settings::defaults()).unwrap();
        network.client = reqwest::Client::builder().no_proxy().build().unwrap();

        let result = network
            .fetch(
                EngineRequest::get(url),
                Duration::from_secs(2),
                None,
                Some("http://[::1"),
            )
            .await;
        server.abort();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(matches!(result, Err(NetworkError::Build(_))));
    }

    #[tokio::test]
    async fn engine_proxy_overrides_global_proxy() {
        use axum::Router;

        let mut proxies = Vec::new();
        let mut servers = Vec::new();
        for name in ["global", "engine"] {
            let app = Router::new().fallback(move || async move { name });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            proxies.push(format!("http://{}", listener.local_addr().unwrap()));
            servers.push(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap()
            }));
        }
        let mut settings = Settings::defaults();
        settings
            .outgoing
            .proxies
            .insert("http".into(), proxies[0].clone());
        let network = Network::from_settings(&settings).unwrap();
        let mut responses = Vec::new();
        for proxy in [None, Some(proxies[1].as_str()), Some(proxies[1].as_str())] {
            responses.push(
                network
                    .fetch(
                        EngineRequest::get("http://example.invalid/"),
                        Duration::from_secs(2),
                        None,
                        proxy,
                    )
                    .await,
            );
        }
        for server in servers {
            server.abort();
        }
        let bodies: Vec<_> = responses
            .into_iter()
            .map(|response| response.unwrap().text().to_string())
            .collect();
        assert_eq!(bodies, ["global", "engine", "engine"]);
    }

    #[tokio::test]
    async fn cached_responses_respect_request_cookies_headers_and_forms() {
        use axum::{Router, routing::post};
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::clone(&calls);
        let app = Router::new().route(
            "/",
            post(move |headers: HeaderMap, body: String| {
                requests.fetch_add(1, Ordering::Relaxed);
                async move {
                    format!(
                        "{}|{}|{}",
                        headers[reqwest::header::COOKIE].to_str().unwrap(),
                        headers[reqwest::header::ACCEPT_LANGUAGE].to_str().unwrap(),
                        body
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::default();

        for (safe, language, form, expected) in [
            ("off", "ja", vec![], "safesearch=off|ja|"),
            ("strict", "ja", vec![], "safesearch=strict|ja|"),
            ("strict", "en", vec![], "safesearch=strict|en|"),
            (
                "strict",
                "en",
                vec![("q", "cats&safe=0"), ("safe", "2")],
                "safesearch=strict|en|q=cats%26safe%3D0&safe=2",
            ),
            (
                "strict",
                "en",
                vec![("q", "cats"), ("safe", "0&safe=2")],
                "safesearch=strict|en|q=cats&safe=0%26safe%3D2",
            ),
        ] {
            let request = EngineRequest::post(&url)
                .cookie("safesearch", safe)
                .header("Accept-Language", language)
                .header("Accept", "text/plain")
                .form(
                    form.into_iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                );
            let response = network
                .fetch(request, Duration::from_secs(2), Some(&cache), None)
                .await
                .unwrap();
            assert_eq!(response.text(), expected);
        }
        let repeated = EngineRequest::post(&url)
            .header("Accept", "text/plain")
            .header("Accept-Language", "en")
            .cookie("safesearch", "strict");
        let response = network
            .fetch(repeated, Duration::from_secs(2), Some(&cache), None)
            .await
            .unwrap();
        assert_eq!(response.text(), "safesearch=strict|en|");
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        server.abort();
    }

    /// (If-None-Match, If-Modified-Since) as seen by a test origin.
    type ConditionalHeaders = (Option<String>, Option<String>);

    /// Conditional request headers observed by a test origin.
    #[derive(Clone, Default)]
    struct Observed(std::sync::Arc<std::sync::Mutex<Vec<ConditionalHeaders>>>);

    impl Observed {
        fn record(&self, headers: &HeaderMap) {
            let value = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            };
            self.0
                .lock()
                .unwrap()
                .push((value("if-none-match"), value("if-modified-since")));
        }

        fn at(&self, index: usize) -> ConditionalHeaders {
            self.0.lock().unwrap()[index].clone()
        }
    }

    #[tokio::test]
    async fn not_modified_reuses_the_stale_body_and_extends_the_ttl() {
        use axum::response::IntoResponse;
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Observed::default();
        let counter = Arc::clone(&hits);
        let seen = observed.clone();
        let app = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let counter = Arc::clone(&counter);
                let seen = seen.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    seen.record(&headers);
                    if headers.contains_key("if-none-match") {
                        axum::http::StatusCode::NOT_MODIFIED.into_response()
                    } else {
                        (
                            [
                                (axum::http::header::ETAG, "\"v1\""),
                                (
                                    axum::http::header::LAST_MODIFIED,
                                    "Thu, 01 Jan 2026 00:00:00 GMT",
                                ),
                            ],
                            "content-v1",
                        )
                            .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::memory(Duration::from_millis(250), 8 * 1024 * 1024);

        let first = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.text(), "content-v1");
        assert_eq!(observed.at(0), (None, None));

        // Let the entry expire so the next fetch revalidates.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(second.text(), "content-v1");
        assert_eq!(
            observed.at(1),
            (
                Some("\"v1\"".into()),
                Some("Thu, 01 Jan 2026 00:00:00 GMT".into())
            )
        );

        // The 304 extended the TTL, so this one is a plain cache hit.
        let third = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(third.text(), "content-v1");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn entries_without_validators_are_refetched_in_full() {
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Observed::default();
        let counter = Arc::clone(&hits);
        let seen = observed.clone();
        let app = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let counter = Arc::clone(&counter);
                let seen = seen.clone();
                async move {
                    let hit = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    seen.record(&headers);
                    format!("content-v{hit}")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::memory(Duration::from_millis(250), 8 * 1024 * 1024);

        let first = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.text(), "content-v1");
        assert_eq!(observed.at(0), (None, None));

        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(second.text(), "content-v2");
        // Without validators there is nothing to revalidate with.
        assert_eq!(observed.at(1), (None, None));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn last_modified_alone_sends_if_modified_since() {
        use axum::response::IntoResponse;
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Observed::default();
        let counter = Arc::clone(&hits);
        let seen = observed.clone();
        let app = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let counter = Arc::clone(&counter);
                let seen = seen.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    seen.record(&headers);
                    if headers.contains_key("if-modified-since") {
                        axum::http::StatusCode::NOT_MODIFIED.into_response()
                    } else {
                        (
                            [(
                                axum::http::header::LAST_MODIFIED,
                                "Thu, 01 Jan 2026 00:00:00 GMT",
                            )],
                            "content-v1",
                        )
                            .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::memory(Duration::from_millis(250), 8 * 1024 * 1024);

        let first = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.text(), "content-v1");

        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(second.text(), "content-v1");
        assert_eq!(
            observed.at(1),
            (None, Some("Thu, 01 Jan 2026 00:00:00 GMT".into()))
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn both_validators_are_sent_and_new_content_replaces_the_cache() {
        use axum::response::IntoResponse;
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Observed::default();
        let counter = Arc::clone(&hits);
        let seen = observed.clone();
        let app = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let counter = Arc::clone(&counter);
                let seen = seen.clone();
                async move {
                    let hit = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    seen.record(&headers);
                    let if_none_match = headers.get("if-none-match").and_then(|v| v.to_str().ok());
                    if if_none_match == Some("\"v2\"") {
                        axum::http::StatusCode::NOT_MODIFIED.into_response()
                    } else if hit == 1 {
                        (
                            [
                                (axum::http::header::ETAG, "\"v1\""),
                                (
                                    axum::http::header::LAST_MODIFIED,
                                    "Thu, 01 Jan 2026 00:00:00 GMT",
                                ),
                            ],
                            "content-v1",
                        )
                            .into_response()
                    } else {
                        (
                            [
                                (axum::http::header::ETAG, "\"v2\""),
                                (
                                    axum::http::header::LAST_MODIFIED,
                                    "Fri, 02 Jan 2026 00:00:00 GMT",
                                ),
                            ],
                            "content-v2",
                        )
                            .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::memory(Duration::from_millis(250), 8 * 1024 * 1024);

        let first = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.text(), "content-v1");
        assert_eq!(observed.at(0), (None, None));

        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        // The content changed: both validators go out and the new body wins.
        assert_eq!(second.text(), "content-v2");
        assert_eq!(
            observed.at(1),
            (
                Some("\"v1\"".into()),
                Some("Thu, 01 Jan 2026 00:00:00 GMT".into())
            )
        );

        // The replacement kept its own validators for the next revalidation.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let third = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(third.text(), "content-v2");
        assert_eq!(
            observed.at(2),
            (
                Some("\"v2\"".into()),
                Some("Fri, 02 Jan 2026 00:00:00 GMT".into())
            )
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn a_fully_cleared_cache_fetches_without_conditional_headers() {
        use axum::response::IntoResponse;
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Observed::default();
        let counter = Arc::clone(&hits);
        let seen = observed.clone();
        let app = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let counter = Arc::clone(&counter);
                let seen = seen.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    seen.record(&headers);
                    ([(axum::http::header::ETAG, "\"v1\"")], "content-v1").into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let cache = Cache::memory(Duration::from_millis(250), 8 * 1024 * 1024);

        let first = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.text(), "content-v1");
        assert_eq!(observed.at(0), (None, None));

        // Nothing is left to revalidate after the entry is gone: the fetch
        // behaves exactly like the pre-conditional-GET path.
        cache.clear().await;
        let second = network
            .fetch(
                EngineRequest::get(&url),
                Duration::from_secs(2),
                Some(&cache),
                None,
            )
            .await
            .unwrap();
        assert_eq!(second.text(), "content-v1");
        assert_eq!(observed.at(1), (None, None));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[test]
    fn invalid_ca_bundle_fails_instead_of_disabling_verification() {
        let mut outgoing = crate::settings::Outgoing::default();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not a certificate").unwrap();
        outgoing.ca_bundle = Some(file.path().to_string_lossy().into_owned());
        assert!(build_client(&outgoing, None, reqwest::redirect::Policy::none(), None).is_err());
        outgoing.ca_bundle = Some("/nonexistent/rustxng-ca.pem".into());
        assert!(build_client(&outgoing, None, reqwest::redirect::Policy::none(), None).is_err());
        assert!(outgoing.verify);
    }

    /// The media budget is a fixed number of slots, and every attempt must
    /// give its slot back even when the fetch fails or times out.
    #[tokio::test]
    async fn media_fetches_release_their_slot() {
        let network = Network::from_settings(&crate::settings::Settings::defaults()).unwrap();
        assert_eq!(
            network.media_slots.available_permits(),
            MAX_CONCURRENT_MEDIA,
            "the media budget must start full"
        );
        for _ in 0..(MAX_CONCURRENT_MEDIA + 2) {
            let request = EngineRequest::get("https://media.invalid/x.png");
            let outcome = network
                .fetch_image(request, Duration::from_millis(50), 1024)
                .await;
            assert!(outcome.is_err(), "an unresolvable host cannot succeed");
            assert_eq!(
                network.media_slots.available_permits(),
                MAX_CONCURRENT_MEDIA,
                "a failed media read must not leak its slot"
            );
        }
    }

    /// Only `MAX_CONCURRENT_MEDIA` media reads can run at once.
    #[tokio::test]
    async fn media_reads_are_bounded_by_the_slot_count() {
        let network = Network::from_settings(&crate::settings::Settings::defaults()).unwrap();
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_MEDIA {
            held.push(
                network
                    .media_slots
                    .clone()
                    .try_acquire_owned()
                    .expect("the budget starts full"),
            );
        }
        assert!(
            network.media_slots.clone().try_acquire_owned().is_err(),
            "the budget must not grow past MAX_CONCURRENT_MEDIA"
        );
    }

    #[test]
    fn proxy_keys_select_the_scheme_they_name() {
        let mut outgoing = crate::settings::Outgoing::default();
        outgoing
            .proxies
            .insert("https".into(), "socks5h://127.0.0.1:9050".into());
        assert!(
            build_client(&outgoing, None, reqwest::redirect::Policy::none(), None).is_ok(),
            "an https-only route must build"
        );

        outgoing.proxies.clear();
        outgoing
            .proxies
            .insert("fpt".into(), "socks5h://127.0.0.1:9050".into());
        // A typo used to be accepted silently and then applied to every
        // scheme, which changed the egress route without any signal.
        let error = match build_client(&outgoing, None, reqwest::redirect::Policy::none(), None) {
            Ok(_) => panic!("an unknown proxy key is a configuration error"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("unsupported proxy key"),
            "{error}"
        );
    }

    #[test]
    fn user_agents_are_diverse_and_plausible() {
        let network = Network {
            client: reqwest::Client::new(),
            media_client: reqwest::Client::new(),
            media_proxy_blocked: false,
            proxied: std::sync::Mutex::new(std::collections::HashMap::new()),
            outgoing: crate::settings::Outgoing::default(),
            user_agents: build_user_agents(),
            suffix: String::new(),
            retries: 0,
            counter: AtomicUsize::new(0),
            api_agent: identified_user_agent(""),
            identified: None,
            host_spacing: Duration::ZERO,
            host_spacing_overrides: Vec::new(),
            host_slots: std::sync::Mutex::new(std::collections::HashMap::new()),
            media_slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_MEDIA)),
            resolver: crate::fetch::validated_resolver(Vec::new()),
            engine_hosts: Arc::new(std::collections::BTreeSet::new()),
        };
        let first = network.user_agent();
        let second = network.user_agent();
        assert!(first.starts_with("Mozilla/5.0"));
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn media_does_not_inherit_private_engine_endpoints() {
        use axum::{Router, routing::get};
        use std::sync::Arc;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { "engine" }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut settings = Settings::defaults();
        let mut engine = crate::settings::EngineConfig::new("private", "mwmbl", "", &["general"]);
        engine.base_url = Some(url.clone());
        settings.engines = vec![engine];
        let network = Network::from_settings(&settings).unwrap();
        assert!(matches!(
            network
                .fetch_image(EngineRequest::get(&url), Duration::from_secs(2), 32)
                .await,
            Err(NetworkError::Build(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(
            network
                .fetch(EngineRequest::get(&url), Duration::from_secs(2), None, None)
                .await
                .unwrap()
                .text(),
            "engine"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn media_redirects_cannot_contact_private_targets() {
        use axum::{Router, routing::get};
        use std::sync::Arc;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let target = Router::new().fallback(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async { "private" }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_port = listener.local_addr().unwrap().port();
        let private_server =
            tokio::spawn(async move { axum::serve(listener, target).await.unwrap() });
        let app = Router::new()
            .route(
                "/literal",
                get(move || async move {
                    axum::response::Redirect::temporary(&format!("http://127.0.0.1:{target_port}/"))
                }),
            )
            .route(
                "/name",
                get(move || async move {
                    axum::response::Redirect::temporary(&format!("http://localhost:{target_port}/"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = crate::fetch::public_client(
            &crate::settings::Outgoing::default(),
            &[("example.com".into(), address)],
        )
        .unwrap()
        .0;
        let mut network = Network::from_settings(&Settings::defaults())
            .unwrap()
            .with_test_media_client(client);
        network.retries = 0;
        for path in ["literal", "name"] {
            let url = format!("http://example.com:{}/{path}", address.port());
            assert!(
                network
                    .fetch_image(EngineRequest::get(url), Duration::from_secs(2), 32)
                    .await
                    .is_err()
            );
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        server.abort();
        private_server.abort();
    }

    #[tokio::test]
    async fn media_size_limit_stops_before_end_of_stream() {
        use axum::{Router, body::Body, routing::get};
        use futures::StreamExt;
        let app = Router::new().route(
            "/",
            get(|| async {
                Body::from_stream(
                    futures::stream::once(async {
                        Ok::<_, std::convert::Infallible>(vec![b'x'; 33])
                    })
                    .chain(futures::stream::pending()),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = crate::fetch::public_client(
            &crate::settings::Outgoing::default(),
            &[("example.com".into(), address)],
        )
        .unwrap()
        .0;
        let network = Network::from_settings(&Settings::defaults())
            .unwrap()
            .with_test_media_client(client);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            network.fetch_image(
                EngineRequest::get(format!("http://example.com:{}/", address.port())),
                Duration::from_secs(8),
                32,
            ),
        )
        .await
        .expect("must reject without waiting for EOF");
        assert!(matches!(result, Err(NetworkError::TooLarge(32))));
        server.abort();
    }

    #[tokio::test]
    async fn image_limits_apply_to_decoded_bytes_and_accept_the_exact_limit() {
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let app = Router::new()
            .route(
                "/exact",
                get(|| async {
                    (
                        [(reqwest::header::CONTENT_TYPE, "image/png")],
                        vec![b'x'; 32],
                    )
                }),
            )
            .route(
                "/compressed",
                get(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async {
                        // Compressed 33 bytes; decoded limit is 32.
                        (
                            [
                                (reqwest::header::CONTENT_TYPE, "image/png"),
                                (reqwest::header::CONTENT_ENCODING, "gzip"),
                            ],
                            vec![
                                31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 168, 32, 0, 0, 172, 211, 191,
                                111, 33, 0, 0, 0,
                            ],
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = crate::fetch::public_client(
            &crate::settings::Outgoing::default(),
            &[("example.com".into(), address)],
        )
        .unwrap()
        .0;
        let mut network = Network::from_settings(&Settings::defaults())
            .unwrap()
            .with_test_media_client(client);
        network.retries = 2;
        let response = network
            .fetch_image(
                EngineRequest::get(format!("http://example.com:{}/exact", address.port())),
                Duration::from_secs(2),
                32,
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.header("content-type"), Some("image/png"));
        assert_eq!(response.body, [b'x'; 32]);
        let error = network
            .fetch_image(
                EngineRequest::get(format!("http://example.com:{}/compressed", address.port())),
                Duration::from_secs(2),
                32,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, NetworkError::TooLarge(32)), "{error}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn captcha_pages_are_recognised() {
        assert_eq!(
            classify_status(403, "please solve the captcha"),
            FailureKind::Captcha
        );
        assert_eq!(classify_status(403, "denied"), FailureKind::AccessDenied);
        assert_eq!(classify_status(429, ""), FailureKind::TooManyRequests);
        assert_eq!(classify_status(500, ""), FailureKind::Http);
    }

    fn network_with(configure: impl FnOnce(&mut crate::settings::Outgoing)) -> Network {
        let mut settings = Settings::default();
        configure(&mut settings.outgoing);
        Network::from_settings(&settings).expect("network")
    }

    #[test]
    fn documented_api_hosts_are_recognised_exactly() {
        assert!(is_documented_api_host(
            "https://en.wikipedia.org/w/api.php?x=1"
        ));
        assert!(is_documented_api_host("https://crates.io/api/v1/crates"));
        assert!(is_documented_api_host(
            "https://api.github.com/search/repositories"
        ));
        assert!(is_documented_api_host(
            "https://api2.marginalia-search.com/search"
        ));
        assert!(is_documented_api_host("https://qiita.com/api/v2/items"));
        assert!(!is_documented_api_host("https://github.com/search"));
        assert!(!is_documented_api_host("https://evilcrates.io/"));
        assert!(!is_documented_api_host("https://www.bing.com/search"));
    }

    #[test]
    fn an_identified_instance_always_sends_the_same_honest_user_agent() {
        let network = network_with(|outgoing| {
            outgoing.identified_ua = true;
            outgoing.contact = "ops@example.org".into();
        });
        let first = network.user_agent();
        assert_eq!(first, network.user_agent());
        assert!(first.starts_with("rustxng/"), "{first}");
        assert!(first.contains("ops@example.org"));
        assert!(!first.contains("Mozilla"));
        // The default stays the rotating pool.
        assert!(network_with(|_| {}).user_agent().starts_with("Mozilla/5.0"));
    }

    #[test]
    fn a_domain_can_have_its_own_spacing() {
        let network = network_with(|outgoing| {
            outgoing.host_spacing_ms = 1100;
            outgoing
                .host_spacing_overrides
                .insert("wikipedia.org".into(), 200);
            outgoing
                .host_spacing_overrides
                .insert("ja.wikipedia.org".into(), 0);
        });
        assert_eq!(
            network.spacing_for("en.wikipedia.org"),
            Duration::from_millis(200)
        );
        assert_eq!(network.spacing_for("ja.wikipedia.org"), Duration::ZERO);
        assert_eq!(
            network.spacing_for("crates.io"),
            Duration::from_millis(1100)
        );
        assert_eq!(
            network.spacing_for("notwikipedia.org"),
            Duration::from_millis(1100)
        );
    }

    #[tokio::test]
    async fn requests_to_one_host_are_spaced_and_a_busy_host_is_skipped() {
        let network = network_with(|outgoing| outgoing.host_spacing_ms = 200);
        let budget = Duration::from_secs(5);
        let started = std::time::Instant::now();
        network
            .wait_for_host_slot("https://api.example.org/a", budget)
            .await
            .unwrap();
        // A different host does not wait.
        network
            .wait_for_host_slot("https://other.example.org/a", budget)
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        network
            .wait_for_host_slot("https://api.example.org/b", budget)
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(190));
        // The next slot is 200 ms away: a 50 ms budget gives up instead.
        let result = network
            .wait_for_host_slot("https://api.example.org/c", Duration::from_millis(50))
            .await;
        assert!(matches!(result, Err(NetworkError::Timeout)));
    }
    // -------------------------------------------------------------------------
    // Retry-After（429 の待機時間の指示。RFC 9110 §10.2.3）
    // -------------------------------------------------------------------------

    #[test]
    fn retry_after_seconds_is_honoured() {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_string(), "3".to_string());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(3)));
    }

    #[test]
    fn retry_after_is_capped_at_a_minute() {
        let mut headers = BTreeMap::new();
        // サーバー設定ミスや悪意ある値で起動を長時間止めない。
        headers.insert("retry-after".to_string(), "86400".to_string());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(60)));
    }

    #[test]
    fn retry_after_http_date_falls_back() {
        let mut headers = BTreeMap::new();
        // HTTP-date 形式は解釈しない。呼び出し側の 700ms フォールバックに任せる。
        headers.insert(
            "retry-after".to_string(),
            "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn retry_after_missing_is_none() {
        assert_eq!(parse_retry_after(&BTreeMap::new()), None);
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_string(), "".to_string());
        assert_eq!(parse_retry_after(&headers), None);
    }

    /// 429 + Retry-After: 0 なら即リトライして成功する（待機なしで 1 回だけやり直す）。
    #[tokio::test]
    async fn rate_limited_request_retries_once() {
        use axum::response::IntoResponse;
        use axum::{Router, routing::get};
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let app = Router::new().route(
            "/",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            [(axum::http::header::RETRY_AFTER, "0")],
                        )
                            .into_response()
                    } else {
                        "ok".into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let response = network
            .fetch(
                EngineRequest::get(format!("http://{addr}/")),
                Duration::from_secs(5),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "429 のあと 1 回だけリトライする"
        );
        server.abort();
    }

    #[test]
    fn engine_redirects_may_not_leave_for_a_private_destination() {
        let engine = reqwest::Url::parse("https://engine.example/search?q=x").unwrap();
        for (target, expected) in [
            ("https://engine.example/search?q=y", true),
            ("https://engine.example:443/other", true),
            ("https://cdn.example/answer.json", true),
            ("http://engine.example/other", true),
            // A public host on another port is a destination the resolver can
            // validate, so it is allowed; only private ports are not exempt.
            ("https://engine.example:8443/other", true),
            ("https://127.0.0.1/secret", false),
            ("https://169.254.169.254/latest/meta-data/", false),
            ("http://10.1.2.3/admin", false),
            ("http://localhost:8080/", false),
            ("http://[::1]/", false),
            ("file:///etc/passwd", false),
        ] {
            let target = reqwest::Url::parse(target).unwrap();
            assert_eq!(
                allowed_by_default(Some(&engine), &target),
                expected,
                "{target}"
            );
        }
        // A configured private endpoint keeps working while the redirect stays
        // on its own origin, but a hop onto another loopback port is another
        // service and is refused.
        let private = reqwest::Url::parse("http://127.0.0.1:8080/api").unwrap();
        assert!(allowed_by_default(
            Some(&private),
            &reqwest::Url::parse("http://127.0.0.1:8080/api/v2").unwrap()
        ));
        assert!(!allowed_by_default(
            Some(&private),
            &reqwest::Url::parse("http://127.0.0.1:9090/admin").unwrap()
        ));
    }

    #[tokio::test]
    async fn engine_redirects_to_private_targets_are_refused() {
        use axum::Router;
        use axum::response::Redirect;

        // The engine answers 302 onto a host that is not its own. The client
        // has to refuse the hop before it connects anywhere.
        let app =
            Router::new().fallback(|| async { Redirect::temporary("http://10.255.255.1/secret") });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let refused = network
            .fetch(
                EngineRequest::get(format!("http://{addr}/search")),
                Duration::from_secs(5),
                None,
                None,
            )
            .await;
        server.abort();
        let message = match refused {
            Err(NetworkError::Transport(message)) => message,
            other => panic!("the redirect has to be refused: {other:?}"),
        };
        assert!(message.contains("redirect"), "{message}");
    }

    #[tokio::test]
    async fn engine_bodies_are_cut_at_the_engine_limit() {
        use axum::Router;

        let app = Router::new().fallback(|| async { "A".repeat(ENGINE_MAX_BODY_BYTES + 1) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let network = Network::from_settings(&Settings::defaults()).unwrap();
        let result = network
            .fetch(
                EngineRequest::get(format!("http://{addr}/search")),
                Duration::from_secs(10),
                None,
                None,
            )
            .await;
        server.abort();
        assert!(
            matches!(result, Err(NetworkError::TooLarge(ENGINE_MAX_BODY_BYTES))),
            "{result:?}"
        );
    }
    #[test]
    fn only_enabled_engine_hosts_are_exempt() {
        let mut settings = crate::settings::Settings::defaults();
        let mut primary =
            crate::settings::EngineConfig::new("self hosted", "json_engine", "sh", &["general"]);
        primary.base_url = Some("https://Search.Internal./api".into());
        let mut templated =
            crate::settings::EngineConfig::new("templated", "json_engine", "tp", &["general"]);
        templated.search_url = Some("http://engine.lan:8080/search?q={query}&p={pageno}".into());
        let mut disabled =
            crate::settings::EngineConfig::new("disabled", "json_engine", "dis", &["general"]);
        disabled.base_url = Some("http://off.internal/".into());
        disabled.disabled = Some(true);
        settings.engines = vec![primary, templated, disabled];

        let hosts = configured_engine_hosts(&settings);
        assert!(hosts.contains(&"search.internal".to_string()), "{hosts:?}");
        assert!(hosts.contains(&"engine.lan".to_string()), "{hosts:?}");
        assert!(
            !hosts.contains(&"off.internal".to_string()),
            "disabled engines are not exempt: {hosts:?}"
        );
    }

    #[test]
    fn redirects_crossing_origins_are_restricted() {
        use std::collections::BTreeSet;
        let engine = reqwest::Url::parse("https://engine.example/search").unwrap();
        let same = reqwest::Url::parse("https://engine.example/next").unwrap();
        let public = reqwest::Url::parse("https://other.example/next").unwrap();
        let trusted = reqwest::Url::parse("http://engine.lan/search").unwrap();
        let mut hosts = BTreeSet::new();
        hosts.insert("engine.lan".to_string());

        assert!(redirect_allowed(Some(&engine), &same, false, &hosts));
        assert!(redirect_allowed(Some(&engine), &public, false, &hosts));
        assert!(
            !redirect_allowed(Some(&engine), &public, true, &hosts),
            "a proxy hides the destination from the resolver"
        );
        assert!(
            !redirect_allowed(Some(&engine), &trusted, false, &hosts),
            "another configured engine is not a redirect target"
        );
    }

    /// The redirect rule with no proxy and no configured engine hosts.
    fn allowed_by_default(previous: Option<&reqwest::Url>, target: &reqwest::Url) -> bool {
        redirect_allowed(previous, target, false, &std::collections::BTreeSet::new())
    }
}
