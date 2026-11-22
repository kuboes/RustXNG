//! In-memory HTTP response cache, ported from `searx/cache.py`.
//!
//! Upstream configuration equivalents are `cache.ttl` (default 300 s) and
//! `cache.max_bytes` (default 8 MB); the cache is a bounded LRU with a TTL.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Cached response payload (a clone of the parts engines look at).
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: std::sync::Arc<Vec<u8>>,
    /// `ETag` validator from the origin, kept for conditional revalidation.
    pub etag: Option<String>,
    /// `Last-Modified` validator from the origin, kept for revalidation.
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub hit: u64,
    pub miss: u64,
    pub entries: usize,
    pub bytes: usize,
    pub error: u64,
}

struct Entry {
    stored: Instant,
    response: CachedResponse,
    size: usize,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    order: VecDeque<String>,
    bytes: usize,
}

/// A TTL + LRU cache for engine responses.
pub struct ResponseCache {
    inner: Mutex<Inner>,
    ttl: Duration,
    max_bytes: usize,
    hit: AtomicU64,
    miss: AtomicU64,
    error: AtomicU64,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(300), 8 * 1024 * 1024)
    }
}

impl ResponseCache {
    pub fn new(ttl: Duration, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            ttl,
            max_bytes,
            hit: AtomicU64::new(0),
            miss: AtomicU64::new(0),
            error: AtomicU64::new(0),
        }
    }

    /// The cache key of a request: method + URL + body.
    pub fn key(method: &str, url: &str, body: &str) -> String {
        format!("{method} {url} {body}")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Return a fresh entry. An expired entry counts as a miss but stays
    /// stored so `get_with_stale` can revalidate it with the origin.
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let inner = self.lock();
        match inner.entries.get(key) {
            Some(entry) if entry.stored.elapsed() <= self.ttl => {
                self.hit.fetch_add(1, Ordering::Relaxed);
                Some(entry.response.clone())
            }
            _ => {
                self.miss.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Return the entry even when its TTL elapsed, for conditional
    /// revalidation. Counters stay untouched; callers decide how a stale
    /// lookup is counted.
    pub fn get_with_stale(&self, key: &str) -> Option<CachedResponse> {
        let inner = self.lock();
        inner.entries.get(key).map(|entry| entry.response.clone())
    }

    /// Extend the TTL of an existing entry after a 304 revalidation.
    /// Returns whether the entry was still stored.
    pub fn refresh(&self, key: &str) -> bool {
        let mut inner = self.lock();
        match inner.entries.get_mut(key) {
            Some(entry) => {
                entry.stored = Instant::now();
                true
            }
            None => false,
        }
    }

    pub fn set(&self, key: &str, response: CachedResponse) {
        let size = response.body.len() + key.len() + 128;
        if size > self.max_bytes {
            self.error.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut inner = self.lock();
        self.remove_locked(&mut inner, key);
        inner.bytes += size;
        inner.entries.insert(
            key.to_string(),
            Entry {
                stored: Instant::now(),
                response,
                size,
            },
        );
        inner.order.push_back(key.to_string());
        self.evict_locked(&mut inner);
    }

    fn remove_locked(&self, inner: &mut Inner, key: &str) {
        if let Some(entry) = inner.entries.remove(key) {
            inner.bytes = inner.bytes.saturating_sub(entry.size);
            if let Some(position) = inner.order.iter().position(|candidate| candidate == key) {
                inner.order.remove(position);
            }
        }
    }

    fn evict_locked(&self, inner: &mut Inner) {
        while inner.bytes > self.max_bytes {
            let Some(key) = inner.order.pop_front() else {
                break;
            };
            if let Some(entry) = inner.entries.remove(&key) {
                inner.bytes = inner.bytes.saturating_sub(entry.size);
            }
        }
    }

    /// Drop every entry (used by tests and `/stats`).
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.order.clear();
        inner.bytes = 0;
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            hit: self.hit.load(Ordering::Relaxed),
            miss: self.miss.load(Ordering::Relaxed),
            entries: inner.entries.len(),
            bytes: inner.bytes,
            error: self.error.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &str) -> CachedResponse {
        CachedResponse {
            status: 200,
            final_url: "https://example.com/".into(),
            headers: Default::default(),
            body: std::sync::Arc::new(body.as_bytes().to_vec()),
            etag: None,
            last_modified: None,
        }
    }

    #[test]
    fn stored_responses_are_returned() {
        let cache = ResponseCache::default();
        cache.set("GET https://example.com/ ", response("hello"));
        let cached = cache.get("GET https://example.com/ ").expect("cached");
        assert_eq!(cached.body.len(), 5);
        assert_eq!(cache.stats().hit, 1);
    }

    #[test]
    fn missing_keys_count_as_misses() {
        let cache = ResponseCache::default();
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.stats().miss, 1);
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let cache = ResponseCache::new(Duration::from_millis(0), 1024);
        cache.set("key", response("x"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(cache.get("key").is_none());
    }

    #[test]
    fn the_cache_stays_within_its_size_budget() {
        let cache = ResponseCache::new(Duration::from_secs(60), 200);
        for index in 0..50 {
            cache.set(&format!("key-{index}"), response(&"x".repeat(64)));
        }
        assert!(cache.stats().bytes <= 200);
    }

    #[test]
    fn oversized_responses_are_not_cached() {
        let cache = ResponseCache::new(Duration::from_secs(60), 16);
        cache.set("key", response(&"x".repeat(100)));
        assert!(cache.get("key").is_none());
    }

    #[test]
    fn expired_entries_are_kept_for_revalidation() {
        let cache = ResponseCache::new(Duration::from_millis(0), 1024);
        cache.set("key", response("x"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(cache.get("key").is_none());
        let stale = cache.get_with_stale("key").expect("stale entry");
        assert_eq!(stale.body.as_slice(), b"x");
    }

    #[test]
    fn refresh_extends_the_ttl_of_an_entry() {
        let cache = ResponseCache::new(Duration::from_millis(200), 1024);
        cache.set("key", response("x"));
        std::thread::sleep(Duration::from_millis(250));
        assert!(cache.get("key").is_none());
        assert!(cache.refresh("key"));
        assert!(cache.get("key").is_some());
        assert!(!cache.refresh("missing"));
    }
}

/// A cache backend: in-process memory or an external Valkey / Redis server.
pub enum CacheBackend {
    Memory(ResponseCache),
    Valkey(ValkeyCache),
}

/// Cache facade used by the network layer and the HTTP handlers.
pub struct Cache {
    backend: CacheBackend,
    requests: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory(ResponseCache::default()),
            requests: Mutex::default(),
        }
    }
}

impl Cache {
    pub fn memory(ttl: Duration, max_bytes: usize) -> Self {
        Self {
            backend: CacheBackend::Memory(ResponseCache::new(ttl, max_bytes)),
            requests: Mutex::default(),
        }
    }

    /// A Valkey / Redis backed cache (upstream valkey.url). The connection is
    /// opened on first use so the searcher can be built synchronously.
    pub fn valkey(url: &str, ttl: Duration) -> Self {
        Self {
            backend: CacheBackend::Valkey(ValkeyCache::new(url, ttl)),
            requests: Mutex::default(),
        }
    }

    /// Coordinate cache misses without serializing unrelated requests. Weak
    /// entries do not keep completed requests alive; prune them on the next miss.
    /// Dropping a cancelled fetch also releases its lock for the next caller.
    pub(crate) fn request_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        requests.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = requests.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        requests.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub fn key(method: &str, url: &str, body: &str) -> String {
        ResponseCache::key(method, url, body)
    }

    pub async fn get(&self, key: &str) -> Option<CachedResponse> {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.get(key),
            CacheBackend::Valkey(cache) => cache.get(key).await,
        }
    }

    pub async fn set(&self, key: &str, response: CachedResponse) {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.set(key, response),
            CacheBackend::Valkey(cache) => cache.set(key, response).await,
        }
    }

    /// Like `get`, but an expired entry is also returned so its validators
    /// can be revalidated with a conditional request.
    pub async fn get_with_stale(&self, key: &str) -> Option<CachedResponse> {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.get_with_stale(key),
            CacheBackend::Valkey(cache) => cache.get_with_stale(key).await,
        }
    }

    /// Extend the TTL of an existing entry after a 304 revalidation.
    /// Returns whether the entry was still stored.
    pub async fn refresh(&self, key: &str) -> bool {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.refresh(key),
            CacheBackend::Valkey(cache) => cache.refresh(key).await,
        }
    }

    pub async fn clear(&self) {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.clear(),
            CacheBackend::Valkey(cache) => cache.clear().await,
        }
    }

    pub fn stats(&self) -> CacheStats {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.stats(),
            CacheBackend::Valkey(cache) => cache.stats(),
        }
    }
}

/// Namespace of the response-cache keys in Valkey / Redis.
///
/// Entries written before this prefix existed keep their own TTL and are not
/// shared with a namespaced instance.
const VALKEY_KEY_PREFIX: &str = "rustxng:cache:";

/// A cache stored in Valkey / Redis, like the upstream valkey cache backend.
pub struct ValkeyCache {
    url: String,
    connection: tokio::sync::Mutex<Option<ValkeyConnection>>,
    ttl: Duration,
    hit: AtomicU64,
    miss: AtomicU64,
    error: AtomicU64,
    entries: AtomicU64,
}

impl ValkeyCache {
    pub fn new(url: &str, ttl: Duration) -> Self {
        Self {
            url: url.to_string(),
            connection: tokio::sync::Mutex::new(None),
            ttl,
            hit: AtomicU64::new(0),
            miss: AtomicU64::new(0),
            error: AtomicU64::new(0),
            entries: AtomicU64::new(0),
        }
    }

    async fn raw_command(&self, args: &[&str]) -> Result<ValkeyReply, String> {
        let mut pooled = self.connection.lock().await;
        // Cancellation must discard the connection.
        let mut connection = match pooled.take() {
            Some(connection) => connection,
            None => ValkeyConnection::connect(&self.url).await?,
        };
        let response = connection.command(args).await;
        if response.is_ok() {
            *pooled = Some(connection);
        }
        response
    }

    async fn command(&self, args: &[&str]) -> Result<Option<String>, String> {
        Ok(flatten_reply(self.raw_command(args).await?))
    }

    /// Prefix of every key this backend owns. `clear` scans only this
    /// namespace, so a shared Valkey database keeps the entries of other
    /// applications and of older RustXNG versions intact.
    fn namespaced(key: &str) -> String {
        format!("{VALKEY_KEY_PREFIX}{key}")
    }

    pub async fn get(&self, key: &str) -> Option<CachedResponse> {
        let key = Self::namespaced(key);
        match self.command(&["GET", &key]).await {
            Ok(Some(payload)) => match decode_response(&payload) {
                Some(response) => {
                    self.hit.fetch_add(1, Ordering::Relaxed);
                    Some(response)
                }
                None => {
                    self.error.fetch_add(1, Ordering::Relaxed);
                    self.miss.fetch_add(1, Ordering::Relaxed);
                    None
                }
            },
            Ok(None) => {
                self.miss.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(_) => {
                self.error.fetch_add(1, Ordering::Relaxed);
                self.miss.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub async fn set(&self, key: &str, response: CachedResponse) {
        let Some(payload) = encode_response(&response) else {
            self.error.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let ttl = self.ttl.as_secs().max(1).to_string();
        let key = Self::namespaced(key);
        match self.command(&["SET", &key, &payload, "EX", &ttl]).await {
            Ok(_) => {
                self.entries.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.error.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Return the entry even when its TTL elapsed. Valkey enforces the TTL
    /// server side and drops expired values, so this matches `get` without
    /// touching the hit/miss counters.
    pub async fn get_with_stale(&self, key: &str) -> Option<CachedResponse> {
        let key = Self::namespaced(key);
        match self.command(&["GET", &key]).await {
            Ok(Some(payload)) => decode_response(&payload),
            _ => None,
        }
    }

    /// Extend the TTL of an existing entry after a 304 revalidation.
    pub async fn refresh(&self, key: &str) -> bool {
        let ttl = self.ttl.as_secs().max(1).to_string();
        let key = Self::namespaced(key);
        matches!(
            self.command(&["EXPIRE", &key, &ttl]).await,
            Ok(Some(reply)) if reply == "1"
        )
    }

    /// Drop every entry of this namespace.
    ///
    /// Uses `SCAN` instead of `KEYS`: a large shared database must not be
    /// blocked server side by one administrative call.
    pub async fn clear(&self) {
        let pattern = format!("{VALKEY_KEY_PREFIX}*");
        let mut cursor = "0".to_string();
        loop {
            let reply = match self
                .raw_command(&["SCAN", &cursor, "MATCH", &pattern, "COUNT", "256"])
                .await
            {
                Ok(reply) => reply,
                Err(_) => return,
            };
            let ValkeyReply::Array(items) = reply else {
                return;
            };
            let mut items = items.into_iter();
            cursor = match items.next() {
                Some(ValkeyReply::Bulk(Some(value))) | Some(ValkeyReply::Simple(value)) => value,
                _ => return,
            };
            let keys: Vec<String> = match items.next() {
                Some(ValkeyReply::Array(keys)) => keys
                    .into_iter()
                    .filter_map(|item| match item {
                        ValkeyReply::Bulk(Some(value)) | ValkeyReply::Simple(value) => Some(value),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            if !keys.is_empty() {
                let mut args = vec!["DEL"];
                args.extend(keys.iter().map(String::as_str));
                if self.raw_command(&args).await.is_err() {
                    return;
                }
            }
            if cursor == "0" {
                return;
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hit: self.hit.load(Ordering::Relaxed),
            miss: self.miss.load(Ordering::Relaxed),
            entries: self.entries.load(Ordering::Relaxed) as usize,
            bytes: 0,
            error: self.error.load(Ordering::Relaxed),
        }
    }
}

/// Encode a cached response as JSON with a base64 body.
fn encode_response(response: &CachedResponse) -> Option<String> {
    use base64::Engine as _;
    let document = serde_json::json!({
        "status": response.status,
        "final_url": response.final_url,
        "headers": response.headers,
        "body": base64::engine::general_purpose::STANDARD.encode(response.body.as_slice()),
        // Optional validators. Older payloads omit both keys and older
        // decoders ignore them, so the format stays compatible in both
        // directions.
        "etag": response.etag,
        "last_modified": response.last_modified,
    });
    serde_json::to_string(&document).ok()
}

/// Decode the payload written by encode_response.
fn decode_response(payload: &str) -> Option<CachedResponse> {
    use base64::Engine as _;
    let document: serde_json::Value = serde_json::from_str(payload).ok()?;
    let body = document.get("body").and_then(|value| value.as_str())?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    let headers = document
        .get("headers")
        .and_then(|value| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(CachedResponse {
        status: document.get("status").and_then(|value| value.as_u64())? as u16,
        final_url: document
            .get("final_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        headers,
        body: std::sync::Arc::new(body),
        etag: document
            .get("etag")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        last_modified: document
            .get("last_modified")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

/// Upper bound on one RESP bulk argument this process reads.
///
/// A peer names the length in the header, so without a bound a single line
/// such as `$999999999999\r\n` makes the reader reserve that many bytes
/// before a payload ever arrives. Cached documents are capped well below this.
pub(crate) const MAX_BULK_LENGTH: usize = 64 * 1024 * 1024;

/// Upper bound on the RESP array nesting a reply may ask for.
pub(crate) const MAX_REPLY_DEPTH: usize = 8;

/// One parsed RESP length or count header, checked against `limit`.
///
/// The header is untrusted: it is a decimal integer the peer chooses. A value
/// that does not parse, is negative, or is larger than `limit` fails the read
/// instead of being turned into an allocation.
pub(crate) fn bounded_resp_length(header: &str, limit: usize) -> Result<usize, String> {
    let value: i64 = header
        .parse()
        .map_err(|_| format!("resp: {header:?} is not a length"))?;
    if value < 0 {
        return Err(format!("resp: negative length {value}"));
    }
    let length = usize::try_from(value).map_err(|_| format!("resp: length {value} overflows"))?;
    if length > limit {
        return Err(format!(
            "resp: length {length} exceeds the {limit} byte limit"
        ));
    }
    Ok(length)
}

/// A tiny RESP client over a Tokio stream.
struct ValkeyConnection {
    reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl ValkeyConnection {
    async fn connect(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("redis://")
            .or_else(|| url.strip_prefix("valkey://"))
            .ok_or_else(|| format!("unsupported cache url: {url}"))?;
        let (authority, database) = match rest.split_once('/') {
            Some((authority, database)) => (authority, database.parse::<i64>().unwrap_or(0)),
            None => (rest, 0),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse::<u16>().unwrap_or(6379)),
            None => (authority.to_string(), 6379),
        };
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| format!("cache: cannot connect to {host}:{port}: {error}"))?;
        let (reader, writer) = stream.into_split();
        let mut connection = Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
        };
        if database != 0 {
            let database = database.to_string();
            connection.command(&["SELECT", &database]).await?;
        }
        Ok(connection)
    }

    /// Run one command and return its raw reply.
    async fn command(&mut self, args: &[&str]) -> Result<ValkeyReply, String> {
        use tokio::io::AsyncWriteExt as _;
        let mut request = format!("*{}\r\n", args.len());
        for argument in args {
            request.push_str(&format!("${}\r\n{}\r\n", argument.len(), argument));
        }
        self.writer
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        self.writer
            .flush()
            .await
            .map_err(|error| error.to_string())?;
        self.read_reply(0).await
    }

    fn read_reply(
        &mut self,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ValkeyReply, String>> + Send + '_>>
    {
        Box::pin(self.read_reply_inner(depth))
    }

    async fn read_reply_inner(&mut self, depth: usize) -> Result<ValkeyReply, String> {
        use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};
        if depth > MAX_REPLY_DEPTH {
            return Err(format!("resp: reply nested deeper than {MAX_REPLY_DEPTH}"));
        }
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if line.is_empty() {
            return Err("cache: connection closed".into());
        }
        let marker = line.chars().next().unwrap_or(' ');
        let rest = line[1..].trim_end().to_string();
        Ok(match marker {
            '+' => ValkeyReply::Simple(rest),
            '-' => return Err(format!("cache: {rest}")),
            ':' => ValkeyReply::Integer(rest.parse().unwrap_or(0)),
            '$' => {
                let Some(length) = resp_bulk_length(&rest)? else {
                    return Ok(ValkeyReply::Nil);
                };
                let mut buffer = vec![0u8; length + 2];
                self.reader
                    .read_exact(&mut buffer)
                    .await
                    .map_err(|error| error.to_string())?;
                buffer.truncate(length);
                ValkeyReply::Bulk(Some(String::from_utf8_lossy(&buffer).to_string()))
            }
            '*' => {
                let count = resp_element_count(&rest)?;
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(self.read_reply(depth + 1).await?);
                }
                ValkeyReply::Array(items)
            }
            _ => ValkeyReply::Nil,
        })
    }
}

/// The byte length of one bulk reply, or `None` for the RESP nil bulk.
fn resp_bulk_length(header: &str) -> Result<Option<usize>, String> {
    if header.starts_with('-') {
        return Ok(None);
    }
    bounded_resp_length(header, MAX_BULK_LENGTH).map(Some)
}

/// The element count of one array reply header.
fn resp_element_count(header: &str) -> Result<usize, String> {
    // A negative count is the RESP nil array, which carries no elements.
    if header.starts_with('-') {
        return Ok(0);
    }
    // One element is at least three bytes on the wire, so the bulk limit also
    // caps how many elements a reply may claim.
    bounded_resp_length(header, MAX_BULK_LENGTH / 3)
}

enum ValkeyReply {
    Simple(String),
    Bulk(Option<String>),
    Integer(i64),
    Array(Vec<ValkeyReply>),
    Nil,
}

/// Collapse a reply to the textual form the cache treats as a value.
///
/// Multibulk replies (the `*` array form) join their scalar items with a
/// newline, which is what the key listing helpers expect.
fn flatten_reply(reply: ValkeyReply) -> Option<String> {
    match reply {
        ValkeyReply::Bulk(value) => value,
        ValkeyReply::Simple(value) => Some(value),
        ValkeyReply::Integer(value) => Some(value.to_string()),
        ValkeyReply::Nil => None,
        ValkeyReply::Array(items) => Some(
            items
                .into_iter()
                .filter_map(|item| match item {
                    ValkeyReply::Bulk(Some(value)) | ValkeyReply::Simple(value) => Some(value),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

#[cfg(test)]
mod valkey_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    async fn read_command(reader: &mut tokio::io::BufReader<tokio::net::TcpStream>) -> Vec<String> {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let count: usize = line[1..].trim().parse().unwrap();
        let mut command = Vec::new();
        for _ in 0..count {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let length: usize = line[1..].trim().parse().unwrap();
            let mut argument = vec![0; length + 2];
            reader.read_exact(&mut argument).await.unwrap();
            argument.truncate(length);
            command.push(String::from_utf8(argument).unwrap());
        }
        command
    }

    #[test]
    fn resp_lengths_are_bounded_before_anything_is_allocated() {
        assert_eq!(bounded_resp_length("12", 1024).unwrap(), 12);
        assert_eq!(bounded_resp_length("0", 1024).unwrap(), 0);
        // The reported abort: a header alone must not buy an allocation.
        assert!(bounded_resp_length("999999999999", 1024).is_err());
        assert!(bounded_resp_length("-5", 1024).is_err());
        assert!(bounded_resp_length("1e9", 1024).is_err());
        assert!(bounded_resp_length("", 1024).is_err());
        assert!(bounded_resp_length("9223372036854775808", usize::MAX).is_err());
    }

    #[tokio::test]
    async fn an_oversized_bulk_header_fails_the_read_instead_of_allocating() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut stream).await,
                ["GET", "rustxng:cache:key"]
            );
            stream
                .get_mut()
                .write_all(
                    b"$999999999999
",
                )
                .await
                .unwrap();
            // The client must hang up instead of waiting for the payload.
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest).await;
        });
        let cache = Cache::valkey(&url, Duration::from_secs(60));
        assert!(cache.get("key").await.is_none());
        server.await.unwrap();
        assert_eq!(cache.stats().error, 1);
    }

    #[tokio::test]
    async fn a_deeply_nested_reply_fails_the_read() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut stream).await,
                ["GET", "rustxng:cache:key"]
            );
            let mut nested = String::new();
            for _ in 0..MAX_REPLY_DEPTH + 2 {
                nested.push_str(
                    "*1
",
                );
            }
            stream.get_mut().write_all(nested.as_bytes()).await.unwrap();
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest).await;
        });
        let cache = Cache::valkey(&url, Duration::from_secs(60));
        assert!(cache.get("key").await.is_none());
        server.await.unwrap();
        assert_eq!(cache.stats().error, 1);
    }

    #[tokio::test]
    async fn cancelled_commands_do_not_reuse_pending_replies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let (requested, received) = tokio::sync::oneshot::channel();
        let (resume, cancelled) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut first = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut first).await,
                ["GET", "rustxng:cache:first"]
            );
            requested.send(()).unwrap();
            cancelled.await.unwrap();
            let payload =
                r#"{"status":200,"final_url":"https://first.invalid/","body":"Zmlyc3Q="}"#;
            let _ = first
                .get_mut()
                .write_all(format!("${}\r\n{payload}\r\n", payload.len()).as_bytes())
                .await;

            let (stream, _) = listener.accept().await.unwrap();
            let mut second = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut second).await,
                ["GET", "rustxng:cache:second"]
            );
            let payload =
                r#"{"status":200,"final_url":"https://second.invalid/","body":"c2Vjb25k"}"#;
            second
                .get_mut()
                .write_all(format!("${}\r\n{payload}\r\n", payload.len()).as_bytes())
                .await
                .unwrap();
        });
        let cache = std::sync::Arc::new(Cache::valkey(&url, Duration::from_secs(60)));
        let first_cache = std::sync::Arc::clone(&cache);
        let first = tokio::spawn(async move { first_cache.get("first").await });
        received.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        resume.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), cache.get("second"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.body.as_slice(), b"second");
        assert_eq!(response.final_url, "https://second.invalid/");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cache_reads_and_writes_respect_the_fetch_deadline() {
        use crate::engine::EngineRequest;
        use crate::network::{Network, NetworkError};
        use axum::{Router, routing::get};

        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", http.local_addr().unwrap());
        let http_server = tokio::spawn(async move {
            axum::serve(
                http,
                Router::new().route("/", get(|| async { "fresh response" })),
            )
            .await
            .unwrap();
        });
        let network = Network::from_settings(&crate::settings::Settings::defaults()).unwrap();
        let mut outcomes = Vec::new();
        for stall_write in [false, true] {
            let redis = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let cache = Cache::valkey(
                &format!("redis://{}", redis.local_addr().unwrap()),
                Duration::from_secs(60),
            );
            let server = tokio::spawn(async move {
                let (stream, _) = redis.accept().await.unwrap();
                let mut stream = tokio::io::BufReader::new(stream);
                assert_eq!(read_command(&mut stream).await[0], "GET");
                if stall_write {
                    // GET (fast path), GET (under the request lock) and GET
                    // (stale lookup) all miss before the origin response is
                    // written back with SET.
                    for _ in 0..2 {
                        stream.get_mut().write_all(b"$-1\r\n").await.unwrap();
                        assert_eq!(read_command(&mut stream).await[0], "GET");
                    }
                    stream.get_mut().write_all(b"$-1\r\n").await.unwrap();
                    assert_eq!(read_command(&mut stream).await[0], "SET");
                }
                std::future::pending::<()>().await;
            });
            let response = tokio::time::timeout(
                Duration::from_secs(2),
                network.fetch(
                    EngineRequest::get(&url),
                    Duration::from_millis(100),
                    Some(&cache),
                    None,
                ),
            )
            .await;
            outcomes.push((stall_write, response));
            server.abort();
        }
        http_server.abort();
        assert!(
            outcomes.iter().all(|(_, response)| response.is_ok()),
            "cache exceeded request budget: {outcomes:?}"
        );
        for (stall_write, response) in outcomes {
            if stall_write {
                assert_eq!(response.unwrap().unwrap().text(), "fresh response");
            } else {
                assert!(matches!(response.unwrap(), Err(NetworkError::Timeout)));
            }
        }
    }

    #[test]
    fn responses_round_trip_through_the_valkey_payload() {
        let response = CachedResponse {
            status: 200,
            final_url: "https://example.test/".into(),
            headers: [("content-type".to_string(), "text/html".to_string())]
                .into_iter()
                .collect(),
            body: std::sync::Arc::new(b"hello".to_vec()),
            etag: Some("\"v1\"".into()),
            last_modified: Some("Thu, 01 Jan 2026 00:00:00 GMT".into()),
        };
        let payload = encode_response(&response).expect("encode");
        let decoded = decode_response(&payload).expect("decode");
        assert_eq!(decoded.status, 200);
        assert_eq!(decoded.final_url, "https://example.test/");
        assert_eq!(decoded.body.as_slice(), b"hello");
        assert_eq!(
            decoded.headers.get("content-type").map(String::as_str),
            Some("text/html")
        );
        assert_eq!(decoded.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            decoded.last_modified.as_deref(),
            Some("Thu, 01 Jan 2026 00:00:00 GMT")
        );
    }

    #[test]
    fn payloads_without_validators_decode_for_compatibility() {
        // A payload written before the validators were stored.
        let payload = r#"{"status":200,"final_url":"https://example.test/","body":"aGVsbG8="}"#;
        let decoded = decode_response(payload).expect("decode");
        assert_eq!(decoded.body.as_slice(), b"hello");
        assert!(decoded.etag.is_none());
        assert!(decoded.last_modified.is_none());
    }

    #[tokio::test]
    async fn valkey_get_with_stale_returns_the_entry_without_counting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut stream).await,
                ["GET", "rustxng:cache:key"]
            );
            let payload = r#"{"status":200,"final_url":"https://example.test/","body":"aGVsbG8=","etag":"\"v1\""}"#;
            stream
                .get_mut()
                .write_all(format!("${}\r\n{payload}\r\n", payload.len()).as_bytes())
                .await
                .unwrap();
        });
        let cache = Cache::valkey(&url, Duration::from_secs(60));
        let stale = cache.get_with_stale("key").await.expect("entry");
        assert_eq!(stale.body.as_slice(), b"hello");
        assert_eq!(stale.etag.as_deref(), Some("\"v1\""));
        assert_eq!(cache.stats().hit, 0);
        assert_eq!(cache.stats().miss, 0);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn clear_scans_the_whole_namespace_and_deletes_each_page() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            // First page: the server returns a non-zero cursor, so the
            // client has to keep scanning.
            assert_eq!(
                read_command(&mut stream).await,
                ["SCAN", "0", "MATCH", "rustxng:cache:*", "COUNT", "256"]
            );
            stream
                .get_mut()
                .write_all(b"*2\r\n$1\r\n7\r\n*1\r\n$15\r\nrustxng:cache:a\r\n")
                .await
                .unwrap();
            assert_eq!(read_command(&mut stream).await, ["DEL", "rustxng:cache:a"]);
            stream.get_mut().write_all(b":1\r\n").await.unwrap();
            assert_eq!(
                read_command(&mut stream).await,
                ["SCAN", "7", "MATCH", "rustxng:cache:*", "COUNT", "256"]
            );
            stream
                .get_mut()
                .write_all(b"*2\r\n$1\r\n0\r\n*1\r\n$15\r\nrustxng:cache:b\r\n")
                .await
                .unwrap();
            assert_eq!(read_command(&mut stream).await, ["DEL", "rustxng:cache:b"]);
            stream.get_mut().write_all(b":1\r\n").await.unwrap();
        });
        let cache = Cache::valkey(&url, Duration::from_secs(60));
        tokio::time::timeout(Duration::from_secs(2), cache.clear())
            .await
            .expect("clear must finish once the cursor returns to zero");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn refresh_extends_the_valkey_ttl() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            assert_eq!(
                read_command(&mut stream).await,
                ["EXPIRE", "rustxng:cache:key", "60"]
            );
            stream.get_mut().write_all(b":1\r\n").await.unwrap();
            assert_eq!(
                read_command(&mut stream).await,
                ["EXPIRE", "rustxng:cache:missing", "60"]
            );
            stream.get_mut().write_all(b":0\r\n").await.unwrap();
        });
        let cache = Cache::valkey(&url, Duration::from_secs(60));
        assert!(cache.refresh("key").await);
        assert!(!cache.refresh("missing").await);
        server.await.unwrap();
    }
}
