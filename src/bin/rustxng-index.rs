//! Crawl a seed list into the local index, and query it from the terminal.
//!
//!   rustxng-index crawl --seeds seeds.txt --out ./index-data --max-pages 2000
//!       [--sitemaps] [--within-seed-path] [--skip-indexed] [--max-per-host N] [--workers N]
//!       [--since RFC3339] [--prune-missing [--scope PREFIX] [--dry-run]]
//!   rustxng-index crawl --plan crawl-plan.toml --out ./index-data [--due]
//!   rustxng-index search --index ./index-data "tokio runtime"
//!   rustxng-index stats --index ./index-data
//!   rustxng-index schedule --index ./index-data
//!
//! Requests stay on the seed sites, obey robots and page directives, and share
//! per-host pacing. Downloads, pending URLs, and sitemap traversal are bounded.
//!
//! A recrawl replaces the documents it visits and keeps the rest. The deletion
//! pass (`--prune-missing`) and the freshness cutoff (`--since`) are opt-in and
//! only run after a complete pass over the scope.
//!
//! A plan file declares the sites and how often each is crawled. The next due
//! time, the last attempt and the consecutive failures are stored with the
//! index, so an external batch scheduler decides when to start the command and
//! the command decides which sites are due.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustxng::crawl::{self, Directives};
use rustxng::localindex::schedule::{self, CrawlPlan, PlannedSite, Schedule};
use rustxng::localindex::{LocalIndex, Page, has_cjk};
use rustxng::robots::Robots;
use scraper::{Html, Selector};

/// Skip files that are not pages.
const SKIP_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico", ".css", ".js", ".json", ".zip",
    ".gz", ".tar", ".pdf", ".mp4", ".mp3", ".woff", ".woff2", ".ttf", ".xml", ".rss", ".txt",
];

/// Decoded body bytes read from one page.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Most redirects followed for one request.
const MAX_REDIRECTS: usize = 5;
const ROBOTS_MAX_BYTES: usize = 512 * 1024;
const SITEMAP_MAX_BYTES: usize = 10 * 1024 * 1024;
const SITEMAP_MAX_LOCATIONS: usize = 50_000;
/// Longest pause after a 429/503 without a usable `Retry-After`.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(30);
/// Longest `Retry-After` we are willing to honour.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

fn usage() {
    println!(
        "usage: rustxng-index <crawl|search|stats|schedule> [options]

crawl
  --seeds FILE            seed URLs, one per line (required without --plan)
  --plan FILE             TOML plan of sites and their crawl intervals;
                          without --due every site is crawled and rescheduled
  --due                   with --plan: crawl only the sites whose turn it is
  --out DIR, --index DIR  index directory (default index-data)
  --max-pages N           stop after N pages (default 500)
  --max-depth N           follow links N levels deep (default 2)
  --max-per-host N        pages taken from one host (default 2000)
  --max-queue N           pending URLs kept (default 10000)
  --max-sitemaps N        sitemap documents read per origin (default 64)
  --delay-ms N            pause between requests to one host (default 1000)
  --workers N             download threads (default 4)
  --contact TEXT          mail address or URL announced in the user agent
  --sitemaps              also read the hosts' sitemaps
  --within-seed-path      stay under the seeds' directories
  --skip-indexed          do not download sitemap URLs the index already holds
  --since RFC3339         refetch only documents indexed before this instant
                          (needs --sitemaps)
  --prune-missing         delete documents in scope that this run never visited
  --scope PREFIX          URL prefix covered by --prune-missing (repeatable)
  --dry-run               report what --prune-missing would delete

search
  --index DIR             index directory (default index-data)
  --limit N               results to print (default 10)
  QUERY...                words to look for

stats
  --index DIR             index directory (default index-data)

schedule
  --index DIR             index directory (default index-data)

--help                  show this text"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().cloned() else {
        usage();
        std::process::exit(2);
    };
    if matches!(command.as_str(), "--help" | "-h" | "help") {
        usage();
        return;
    }
    let options = Options::parse(&args[1..]);
    let result = match command.as_str() {
        "crawl" => crawl(&options),
        "search" => search(&options),
        "stats" => stats(&options),
        "schedule" => report_schedule(&options),
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// Long form flags, kept tiny so the binary has no extra dependencies.
#[derive(Clone)]
struct Options {
    seeds: Option<PathBuf>,
    /// TOML plan of sites and intervals, instead of one seed list.
    plan: Option<PathBuf>,
    /// With a plan: crawl only the sites whose interval has elapsed.
    due: bool,
    index: PathBuf,
    max_pages: usize,
    max_depth: usize,
    /// Pause between two requests to one host (a host's `Crawl-delay` wins when
    /// it is longer).
    delay_ms: u64,
    workers: usize,
    use_sitemaps: bool,
    /// Stay under the directory of the seed URLs (per host).
    within_seed_path: bool,
    /// Do not download pages the index already holds.
    skip_indexed: bool,
    /// Most pages taken from one host, so no single site fills the index.
    max_per_host: usize,
    max_sitemaps: usize,
    max_queue: usize,
    /// Mail address or URL where the crawled sites can reach us.
    contact: String,
    limit: usize,
    query: Vec<String>,
    /// Delete documents in scope that this run never visited. Never the
    /// default: a partial crawl makes absence look like a deletion.
    prune_missing: bool,
    /// Report what `--prune-missing` would delete without touching the index.
    dry_run: bool,
    /// URL prefixes the deletion pass covers instead of the seed scope.
    scope: Vec<String>,
    /// RFC 3339 instant: only documents indexed before it are refetched.
    since: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut options = Self {
            seeds: None,
            plan: None,
            due: false,
            index: PathBuf::from("index-data"),
            max_pages: 500,
            max_depth: 2,
            delay_ms: 1000,
            workers: 4,
            use_sitemaps: false,
            within_seed_path: false,
            skip_indexed: false,
            max_per_host: 2000,
            max_sitemaps: 64,
            max_queue: 10_000,
            contact: String::new(),
            limit: 10,
            query: Vec::new(),
            prune_missing: false,
            dry_run: false,
            scope: Vec::new(),
            since: None,
        };
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            let mut next = || {
                index += 1;
                args.get(index).cloned()
            };
            match arg.as_str() {
                "--seeds" => options.seeds = next().map(PathBuf::from),
                "--plan" => options.plan = next().map(PathBuf::from),
                "--due" => options.due = true,
                "--index" | "--out" => {
                    if let Some(value) = next() {
                        options.index = PathBuf::from(value);
                    }
                }
                "--max-pages" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.max_pages = value;
                    }
                }
                "--max-depth" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.max_depth = value;
                    }
                }
                "--delay-ms" => {
                    if let Some(value) = next().and_then(|v| v.parse::<u64>().ok()) {
                        options.delay_ms = value;
                    }
                }
                "--max-per-host" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.max_per_host = value.max(1);
                    }
                }
                "--max-sitemaps" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.max_sitemaps = value;
                    }
                }
                "--max-queue" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.max_queue = value.max(1);
                    }
                }
                "--contact" => {
                    if let Some(value) = next() {
                        options.contact = value;
                    }
                }
                "--sitemaps" => options.use_sitemaps = true,
                "--within-seed-path" => options.within_seed_path = true,
                "--skip-indexed" => options.skip_indexed = true,
                "--prune-missing" => options.prune_missing = true,
                "--dry-run" => options.dry_run = true,
                "--scope" => {
                    if let Some(value) = next() {
                        options.scope.push(value);
                    }
                }
                "--since" => options.since = next(),
                "--workers" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.workers = value.clamp(1, 32);
                    }
                }
                "--limit" => {
                    if let Some(value) = next().and_then(|v| v.parse::<usize>().ok()) {
                        options.limit = value;
                    }
                }
                other if !other.starts_with("--") => options.query.push(other.to_string()),
                _ => {}
            }
            index += 1;
        }
        options
    }
}

fn search(options: &Options) -> Result<(), String> {
    let index = LocalIndex::open(&options.index)?;
    let query = options.query.join(" ");
    if query.is_empty() {
        return Err("no query given".to_string());
    }
    let started = Instant::now();
    let hits = index.search(&query, options.limit)?;
    let elapsed = started.elapsed();
    println!(
        "{} hits in {:.1} ms (index has {} documents)",
        hits.len(),
        elapsed.as_secs_f64() * 1000.0,
        index.document_count()
    );
    for (rank, hit) in hits.iter().enumerate() {
        println!("{:>3}. {:.3}  {}", rank + 1, hit.score, hit.url);
        if !hit.title.is_empty() {
            println!("      {}", hit.title);
        }
    }
    Ok(())
}

fn stats(options: &Options) -> Result<(), String> {
    let index = LocalIndex::open(&options.index)?;
    println!("documents: {}", index.document_count());
    Ok(())
}

/// Print the stored crawl schedule without crawling anything.
fn report_schedule(options: &Options) -> Result<(), String> {
    let schedule = Schedule::load(&options.index)?;
    if schedule.sites().next().is_none() {
        println!(
            "schedule: empty ({} has no crawl schedule)",
            options.index.display()
        );
        return Ok(());
    }
    for line in schedule_lines(&schedule, schedule::unix_now()) {
        println!("{line}");
    }
    Ok(())
}

/// One line per site: interval, last attempt, last success, consecutive
/// failures and the next due time.
fn schedule_lines(schedule: &Schedule, now: u64) -> Vec<String> {
    schedule
        .sites()
        .map(|(name, state)| {
            let last_attempt = state
                .last_attempt
                .map(format_time)
                .unwrap_or_else(|| "never".to_string());
            let last_success = state
                .last_success
                .map(format_time)
                .unwrap_or_else(|| "never".to_string());
            let due = if state.next_due <= now {
                "due now"
            } else {
                "waiting"
            };
            format!(
                "{name}: interval {}s, last attempt {last_attempt}, last success {last_success}, failures {}, next {} ({due})",
                state.interval_seconds,
                state.consecutive_failures,
                format_time(state.next_due)
            )
        })
        .collect()
}

/// The title of a page, falling back to the first heading.
fn page_title(html: &Html) -> String {
    for selector in ["title", "h1"] {
        if let Ok(parsed) = Selector::parse(selector) {
            if let Some(element) = html.select(&parsed).next() {
                let text = element.text().collect::<Vec<_>>().join(" ");
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    String::new()
}

/// Absolute, normalised http(s) links of the page that may be followed.
fn links(html: &Html, base: &url::Url) -> Vec<String> {
    let Ok(selector) = Selector::parse("a[href]") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for element in html.select(&selector) {
        if crawl::rel_forbids_following(element.value().attr("rel").unwrap_or("")) {
            continue;
        }
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Ok(resolved) = base.join(href) else {
            continue;
        };
        let path = resolved.path().to_lowercase();
        if SKIP_EXTENSIONS
            .iter()
            .any(|extension| path.ends_with(extension))
        {
            continue;
        }
        if let Some(clean) = crawl::normalize_url(resolved.as_str()) {
            found.push(clean);
        }
    }
    found
}

/// `host` or `host:port`: the unit robots.txt and politeness apply to.
fn authority(url: &url::Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// `www.example.com` and `example.com` count as the same site.
fn bare_host(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

fn build_client(contact: &str) -> Result<reqwest::blocking::Client, String> {
    rustxng::fetch::public_blocking_client(&crawl::user_agent(contact))
}

fn same_site(source: &url::Url, target: &url::Url) -> bool {
    source.host_str().map(bare_host) == target.host_str().map(bare_host)
        && source.port() == target.port()
        && (source.scheme() == target.scheme()
            || (source.scheme() == "http" && target.scheme() == "https"))
}

/// Read at most `limit` bytes of a response body.
fn read_capped(response: reqwest::blocking::Response, limit: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .ok()?;
    (body.len() <= limit).then_some(body)
}

/// What a worker hands back to the writer.
struct Fetched {
    /// The canonical address the page is indexed under.
    url: String,
    aliases: Vec<String>,
    title: String,
    body: String,
    lang: String,
    links: Vec<String>,
    depth: usize,
}

/// The outcome of asking for one page.
enum Fetch {
    Page(Fetched),
    /// Not indexed, but its links may still be worth following.
    Links(Vec<String>),
    Remove {
        urls: Vec<String>,
        links: Vec<String>,
    },
    /// The host asked us to slow down.
    Backoff,
    Skipped,
}

enum IndexUpdate {
    Page(Fetched),
    Remove(Vec<String>),
}

struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Resource {
    Robots,
    Sitemap,
    Page,
}

#[derive(Debug)]
enum FetchError {
    RobotsBlocked,
    Backoff,
    Failed,
}

/// Counters printed at the end of a crawl.
#[derive(Default)]
struct Counters {
    robots_blocked: AtomicUsize,
    noindex: AtomicUsize,
    duplicates: AtomicUsize,
    failed: AtomicUsize,
    backoffs: AtomicUsize,
    host_capped: AtomicUsize,
    queue_capped: AtomicUsize,
}

/// The `host[:port]` a URL belongs to.
fn host_key(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| authority(&parsed))
        .unwrap_or_default()
}

/// Pending URLs grouped by host.
///
/// One FIFO queue lets a crowded host (a documentation site with thousands of
/// links) fill the front, so every worker ends up waiting for that host's
/// politeness delay while the other hosts starve. Grouping by host and always
/// serving a host that is ready keeps all workers busy.
#[derive(Default)]
struct Frontier {
    hosts: HashMap<String, VecDeque<(String, usize)>>,
    /// Hosts with pending URLs, in round-robin order.
    order: VecDeque<String>,
    len: usize,
    scheduled: HashSet<String>,
}

impl Frontier {
    fn push(&mut self, url: String, depth: usize) {
        if !self.scheduled.insert(url.clone()) {
            return;
        }
        let host = host_key(&url);
        let pending = self.hosts.entry(host.clone()).or_default();
        if pending.is_empty() {
            self.order.push_back(host);
        }
        pending.push_back((url, depth));
        self.len += 1;
    }

    /// Take a URL of the first host whose next slot has come; when every host
    /// is still cooling down, the one that becomes ready first.
    fn pop(&mut self, slots: &HashMap<String, Instant>) -> Option<(String, usize)> {
        let now = Instant::now();
        let position = self
            .order
            .iter()
            .position(|host| slots.get(host).is_none_or(|slot| *slot <= now))
            .or_else(|| {
                (0..self.order.len()).min_by_key(|index| slots.get(&self.order[*index]).copied())
            })?;
        let host = self.order.remove(position)?;
        let pending = self.hosts.get_mut(&host)?;
        let item = pending.pop_front();
        if pending.is_empty() {
            self.hosts.remove(&host);
        } else {
            self.order.push_back(host);
        }
        self.len -= usize::from(item.is_some());
        item
    }
}

/// Shared crawl state. Workers pull URLs, the main thread writes documents.
struct Crawl {
    queue: Mutex<Frontier>,
    robots: Mutex<HashMap<String, Arc<OnceLock<Arc<Robots>>>>>,
    /// Earliest time the next request to a host may start.
    next_slot: Mutex<HashMap<String, Instant>>,
    wake: Condvar,
    stopped: AtomicBool,
    /// Pages claimed per host, against `max_per_host`.
    per_host: Mutex<HashMap<String, usize>>,
    in_flight: AtomicUsize,
    counters: Counters,
    /// With `--within-seed-path`: the path prefixes each host may be crawled
    /// under (the directories of its seeds). `None` crawls whole hosts.
    scope: Option<HashMap<String, Vec<String>>>,
    /// URLs taken from sitemaps: the only ones `--skip-indexed` may skip,
    /// because their links are not needed to discover the rest of the site.
    from_sitemap: Mutex<HashSet<String>>,
    /// Every URL this run claimed. A deletion pass compares this with the
    /// index: a document it never saw was not visited, not necessarily gone.
    seen: Mutex<HashSet<String>>,
    max_queue: usize,
}

impl Crawl {
    fn new() -> Self {
        Self {
            queue: Mutex::new(Frontier::default()),
            robots: Mutex::new(HashMap::new()),
            next_slot: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            per_host: Mutex::new(HashMap::new()),
            in_flight: AtomicUsize::new(0),
            counters: Counters::default(),
            scope: None,
            from_sitemap: Mutex::new(HashSet::new()),
            seen: Mutex::new(HashSet::new()),
            max_queue: 10_000,
        }
    }

    /// Remember that this run visited `url`.
    fn mark_seen(&self, url: &str) {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(url.to_string());
    }

    /// Give back a per-host slot claimed for a page that was not fetched.
    fn refund_host(&self, url: &str) {
        if let Ok(mut counts) = self.per_host.lock() {
            if let Some(count) = counts.get_mut(&host_key(url)) {
                *count = count.saturating_sub(1);
            }
        }
    }

    /// Whether `url` lies under one of its host's seed directories.
    fn in_scope(&self, url: &str) -> bool {
        let Some(scope) = &self.scope else {
            return true;
        };
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        scope
            .get(bare_host(&host_key(url)))
            .is_some_and(|prefixes| {
                prefixes
                    .iter()
                    .any(|prefix| parsed.path().starts_with(prefix.as_str()))
            })
    }

    fn claim(&self, max_per_host: usize) -> Option<(String, usize)> {
        let mut queue = self.queue.lock().ok()?;
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return None;
            }
            let popped = {
                let slots = self.next_slot.lock().ok()?;
                queue.pop(&slots)
            };
            let (url, depth) = popped?;
            let within_cap = self
                .per_host
                .lock()
                .map(|mut counts| {
                    let count = counts.entry(host_key(&url)).or_insert(0);
                    *count += 1;
                    *count <= max_per_host
                })
                .unwrap_or(true);
            if within_cap {
                self.in_flight.fetch_add(1, Ordering::SeqCst);
                return Some((url, depth));
            }
            self.counters.host_capped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Wait until this host may be asked again, so several workers still keep
    /// one request per delay per host.
    fn wait_for_host(&self, host: &str, delay: Duration) -> bool {
        let mut slots = self.next_slot.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return false;
            }
            let now = Instant::now();
            let slot = slots.entry(host.to_string()).or_insert(now);
            if *slot <= now {
                let Some(next) = now.checked_add(delay) else {
                    return false;
                };
                *slot = next;
                return true;
            }
            let pause = slot.saturating_duration_since(now);
            (slots, _) = self
                .wake
                .wait_timeout(slots, pause)
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    fn stop(&self) {
        let _slots = self.next_slot.lock().unwrap_or_else(|p| p.into_inner());
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    /// Keep every worker away from this host for a while.
    fn back_off(&self, host: &str, pause: Duration) {
        if let Ok(mut slots) = self.next_slot.lock() {
            let until = Instant::now() + pause;
            let slot = slots.entry(host.to_string()).or_insert(until);
            if *slot < until {
                *slot = until;
            }
        }
    }

    /// The robots.txt policy of a host, fetched once.
    fn robots_for(
        &self,
        client: &reqwest::blocking::Client,
        url: &url::Url,
        delay: Duration,
    ) -> Arc<Robots> {
        let cell = {
            let mut policies = self.robots.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(
                policies
                    .entry(url.origin().ascii_serialization())
                    .or_default(),
            )
        };
        Arc::clone(cell.get_or_init(|| {
            let mut robots_url = url.clone();
            robots_url.set_path("/robots.txt");
            robots_url.set_query(None);
            robots_url.set_fragment(None);
            let policy = match self.request(client, robots_url, delay, Resource::Robots) {
                Ok(response) => {
                    let status = response.status().as_u16();
                    match read_capped(response, ROBOTS_MAX_BYTES) {
                        Some(body) => Robots::from_response(
                            status,
                            &String::from_utf8_lossy(&body),
                            crawl::PRODUCT,
                        ),
                        None => Robots::deny_all(),
                    }
                }
                Err(_) => Robots::deny_all(),
            };
            Arc::new(policy)
        }))
    }

    fn request(
        &self,
        client: &reqwest::blocking::Client,
        mut url: url::Url,
        delay: Duration,
        resource: Resource,
    ) -> Result<reqwest::blocking::Response, FetchError> {
        let original = url.clone();
        for hop in 0..=MAX_REDIRECTS {
            if !rustxng::fetch::is_fetchable(&url)
                || !same_site(&original, &url)
                || (resource == Resource::Page && !self.in_scope(url.as_str()))
            {
                return Err(FetchError::Failed);
            }
            let host = host_key(url.as_str());
            let spacing = if resource == Resource::Robots {
                delay
            } else {
                let robots = self.robots_for(client, &url, delay);
                let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
                if !robots.allowed(target) {
                    return Err(FetchError::RobotsBlocked);
                }
                robots.crawl_delay().map_or(delay, |own| own.max(delay))
            };
            if !self.wait_for_host(&host, spacing) {
                return Err(FetchError::Failed);
            }
            let response = client
                .get(url.clone())
                .send()
                .map_err(|_| FetchError::Failed)?;
            if matches!(response.status().as_u16(), 429 | 503) {
                let pause = retry_after(&response);
                self.back_off(&host, pause);
                return Err(FetchError::Backoff);
            }
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if hop == MAX_REDIRECTS {
                return Err(FetchError::Failed);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::Failed)?;
            let next = url.join(location).map_err(|_| FetchError::Failed)?;
            let next = crawl::normalize_url(next.as_str()).ok_or(FetchError::Failed)?;
            url = url::Url::parse(&next).map_err(|_| FetchError::Failed)?;
        }
        Err(FetchError::Failed)
    }
}

fn sitemap_urls(
    client: &reqwest::blocking::Client,
    state: &Crawl,
    seed: &url::Url,
    options: &Options,
) -> Vec<String> {
    let delay = Duration::from_millis(options.delay_ms);
    let limit = options.max_per_host.min(options.max_pages);
    let mut pending = VecDeque::new();
    let mut scheduled = HashSet::new();
    let mut pages = HashSet::new();
    let mut out = Vec::new();
    let robots = state.robots_for(client, seed, delay);
    for location in robots.sitemaps() {
        let Some(location) = crawl::normalize_url(location) else {
            continue;
        };
        let Ok(url) = url::Url::parse(&location) else {
            continue;
        };
        if scheduled.len() < options.max_sitemaps
            && same_site(seed, &url)
            && rustxng::fetch::is_fetchable(&url)
            && scheduled.insert(location)
        {
            pending.push_back((url, 0));
        }
    }
    while let Some((url, depth)) = pending.pop_front() {
        if out.len() >= limit {
            break;
        }
        let Ok(response) = state.request(client, url, delay, Resource::Sitemap) else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Some(bytes) = read_capped(response, SITEMAP_MAX_BYTES) else {
            continue;
        };
        let Ok((is_index, urls)) = crawl::sitemap_locations(&bytes, SITEMAP_MAX_LOCATIONS) else {
            continue;
        };
        for location in urls {
            let Ok(url) = url::Url::parse(&location) else {
                continue;
            };
            if !same_site(seed, &url) || !rustxng::fetch::is_fetchable(&url) {
                continue;
            }
            if is_index {
                if depth < 2 && scheduled.len() < options.max_sitemaps && scheduled.insert(location)
                {
                    pending.push_back((url, depth + 1));
                }
            } else if state.in_scope(&location) && pages.insert(location.clone()) {
                out.push(location);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out
}

/// The directory of a seed URL's path: `/3/` for `/3/`, `/doc/` for
/// `/doc/index.html`, `/` for `/docs.html`.
fn seed_directory(url: &str) -> String {
    let path = url::Url::parse(url)
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|_| "/".to_string());
    match path.rfind('/') {
        Some(end) => path[..=end].to_string(),
        None => "/".to_string(),
    }
}

/// What one crawl pass did. The scheduler uses it to decide whether the site
/// itself failed, which is what advances the backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CrawlSummary {
    /// Pages this pass wrote to the index.
    indexed: usize,
    /// URLs this pass claimed, however the fetch ended.
    visited: usize,
    /// Requests that failed outright.
    failed: usize,
    /// Hosts that asked us to slow down.
    backoffs: usize,
}

impl CrawlSummary {
    /// Whether the site could not be crawled at all.
    ///
    /// A single failed page is normal and stays a page counter. Only a pass
    /// that wrote nothing while recording failures or back-offs means the site
    /// itself is in trouble, and only that advances the backoff.
    fn is_site_failure(self) -> bool {
        self.visited > 0 && self.indexed == 0 && (self.failed > 0 || self.backoffs > 0)
    }
}

fn crawl(options: &Options) -> Result<(), String> {
    let client = build_client(&options.contact)?;
    if options.plan.is_some() {
        crawl_plan(options, client)
    } else {
        crawl_with_client(options, client).map(|_| ())
    }
}

fn crawl_with_client(
    options: &Options,
    client: reqwest::blocking::Client,
) -> Result<CrawlSummary, String> {
    if options.due {
        return Err("--due needs --plan".to_string());
    }
    // Check the flags before the seed file, as this path always did: a broken
    // command line must not depend on whether the file exists.
    validate_crawl(options)?;
    since_cutoff(options)?;
    let seeds_path = options.seeds.as_ref().ok_or("crawl needs --seeds <file>")?;
    let seeds = read_seeds(seeds_path)?;
    crawl_seeds(options, client, &seeds)
}

/// Seeds of one crawl: blank lines and comments are skipped, URLs are
/// normalised, and a seed that is not a public HTTP(S) URL is refused before
/// anything is requested.
fn read_seeds(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("read {path:?}: {error}"))?;
    let mut seeds = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(url) = crawl::normalize_url(line) {
            let parsed = url::Url::parse(&url).map_err(|error| error.to_string())?;
            if !rustxng::fetch::is_fetchable(&parsed) {
                return Err(format!("seed is not a public HTTP(S) URL: {url}"));
            }
            seeds.push(url);
        }
    }
    if seeds.is_empty() {
        return Err("no seeds".to_string());
    }
    Ok(seeds)
}

/// Flags that shape every pass, checked before the first request.
fn validate_crawl(options: &Options) -> Result<(), String> {
    if options.max_pages == 0 {
        return Err("--max-pages must be positive".into());
    }
    if options.dry_run && !options.prune_missing {
        return Err("--dry-run needs --prune-missing".into());
    }
    if !options.scope.is_empty() && !options.prune_missing {
        return Err("--scope only applies with --prune-missing".into());
    }
    if options.prune_missing && options.skip_indexed {
        return Err(
            "--prune-missing cannot be combined with --skip-indexed: skipped pages look missing"
                .into(),
        );
    }
    if options.prune_missing && options.since.is_some() {
        return Err(
            "--prune-missing cannot be combined with --since: documents that were not refetched look missing"
                .into(),
        );
    }
    for prefix in &options.scope {
        let parsed = url::Url::parse(prefix)
            .map_err(|error| format!("--scope is not a URL ({prefix}): {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("--scope must be an HTTP(S) URL: {prefix}"));
        }
    }
    Ok(())
}

/// The freshness cutoff from --since, in seconds since the Unix epoch.
fn since_cutoff(options: &Options) -> Result<Option<i64>, String> {
    let Some(text) = options.since.as_deref() else {
        return Ok(None);
    };
    if !options.use_sitemaps {
        return Err(
            "--since needs --sitemaps: only sitemap URLs are skipped, so link discovery keeps working"
                .into(),
        );
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(text)
        .map_err(|error| format!("--since is not an RFC 3339 instant ({text}): {error}"))?;
    Ok(Some(parsed.timestamp()))
}

/// Crawl one site's seeds. Every pass, whether it comes from --seeds or from
/// one site of a plan, goes through here, so the flags keep one meaning.
fn crawl_seeds(
    options: &Options,
    client: reqwest::blocking::Client,
    seed_urls: &[String],
) -> Result<CrawlSummary, String> {
    validate_crawl(options)?;
    let since = since_cutoff(options)?;
    let mut queue: VecDeque<(String, usize)> =
        seed_urls.iter().map(|url| (url.clone(), 0)).collect();
    if queue.is_empty() {
        return Err("no seeds".to_string());
    }
    let delay = Duration::from_millis(options.delay_ms);
    let mut state = Crawl::new();
    state.max_queue = options.max_queue;
    if options.within_seed_path {
        let mut scope: HashMap<String, Vec<String>> = HashMap::new();
        for (url, _) in &queue {
            scope
                .entry(bare_host(&host_key(url)).to_string())
                .or_default()
                .push(seed_directory(url));
        }
        state.scope = Some(scope);
    }
    let state = Arc::new(state);

    if options.use_sitemaps {
        let mut expanded = Vec::new();
        let mut asked = HashSet::new();
        for (url, _) in &queue {
            let Ok(parsed) = url::Url::parse(url) else {
                continue;
            };
            if !asked.insert(parsed.origin()) {
                continue;
            }
            let mut from_host = sitemap_urls(&client, &state, &parsed, options);
            from_host.truncate(options.max_pages.saturating_sub(expanded.len()));
            println!("sitemaps of {}: {} urls", host_key(url), from_host.len());
            expanded.extend(from_host);
            if expanded.len() >= options.max_pages {
                break;
            }
        }
        if let Ok(mut listed) = state.from_sitemap.lock() {
            listed.extend(expanded.iter().cloned());
        }
        queue.extend(expanded.into_iter().map(|url| (url, 0)));
    }
    for (url, depth) in queue {
        queue_links(&state, vec![url], depth);
    }

    let index = LocalIndex::open_for_write(&options.index)?;
    let crawl = state;
    let (sender, receiver) = std::sync::mpsc::sync_channel(options.workers);
    let max_depth = options.max_depth;
    let max_per_host = options.max_per_host;
    // `--skip-indexed` and `--since`: a resumed crawl does not download pages
    // it already holds or pages that are still fresh (their links are then not
    // followed; sitemaps cover that).
    let skip_indexed = options.skip_indexed;
    let fresh_since = since;
    let known = if options.skip_indexed {
        Some(Arc::new(LocalIndex::open(&options.index)?))
    } else if since.is_some() {
        // A first run has no index yet: every page is fetched.
        LocalIndex::open(&options.index).ok().map(Arc::new)
    } else {
        None
    };

    let mut workers = Vec::new();
    for _ in 0..options.workers {
        let crawl = Arc::clone(&crawl);
        let known = known.clone();
        let sender = sender.clone();
        let client = client.clone();
        workers.push(std::thread::spawn(move || {
            loop {
                if crawl.stopped.load(Ordering::Acquire) {
                    return;
                }
                let Some((url, depth)) = crawl.claim(max_per_host) else {
                    if crawl.in_flight.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                let _active = InFlight(&crawl.in_flight);
                // Every claimed URL counts as visited, however the fetch ends:
                // a deletion pass must never read a failure as a removal.
                crawl.mark_seen(&url);
                let listed = crawl
                    .from_sitemap
                    .lock()
                    .map(|listed| listed.contains(&url))
                    .unwrap_or(false);
                if listed {
                    let remembered = known.as_ref().is_some_and(|index| {
                        if skip_indexed {
                            index.has_url(&url)
                        } else {
                            fresh_since.is_some_and(|since| {
                                index
                                    .indexed_at(&url)
                                    .ok()
                                    .flatten()
                                    .is_some_and(|indexed_at| indexed_at as i64 >= since)
                            })
                        }
                    });
                    if remembered {
                        crawl.refund_host(&url);
                        continue;
                    }
                }
                let outcome = fetch(&client, &url, depth, max_depth, &crawl, delay);
                let update = match outcome {
                    Fetch::Page(mut page) => {
                        queue_links(&crawl, std::mem::take(&mut page.links), page.depth + 1);
                        Some(IndexUpdate::Page(page))
                    }
                    Fetch::Links(found) => {
                        queue_links(&crawl, found, depth + 1);
                        None
                    }
                    Fetch::Remove { urls, links } => {
                        queue_links(&crawl, links, depth + 1);
                        Some(IndexUpdate::Remove(urls))
                    }
                    Fetch::Backoff => {
                        crawl.counters.backoffs.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Fetch::Skipped => None,
                };
                if let Some(update) = update {
                    if sender.send(update).is_err() {
                        return;
                    }
                }
            }
        }));
    }
    drop(sender);

    let mut indexed = 0;
    let mut truncated = false;
    let writing = (|| -> Result<(), String> {
        let mut fingerprints = HashSet::new();
        let mut content_hashes = HashMap::new();
        for update in &receiver {
            let (page, removals) = match update {
                IndexUpdate::Page(mut page) => {
                    let mut aliases = std::mem::take(&mut page.aliases);
                    aliases.retain(|url| url != &page.url);
                    // The index stores a page under its canonical address,
                    // which a redirect or a canonical link can make different
                    // from the requested one. The deletion pass compares this
                    // set with the stored URLs, so both have to be in it.
                    crawl.mark_seen(&page.url);
                    for url in &aliases {
                        crawl.mark_seen(url);
                    }
                    (Some(page), aliases)
                }
                IndexUpdate::Remove(urls) => (None, urls),
            };
            for url in removals {
                if let Some(hash) = content_hashes.remove(&url) {
                    fingerprints.remove(&hash);
                }
                index.remove(&url)?;
            }
            let Some(page) = page else {
                continue;
            };
            if let Some(hash) = content_hashes.remove(&page.url) {
                fingerprints.remove(&hash);
            }
            let fingerprint = crawl::content_fingerprint(&page.body);
            if !fingerprints.insert(fingerprint) {
                index.remove(&page.url)?;
                crawl.counters.duplicates.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            content_hashes.insert(page.url.clone(), fingerprint);
            index.add(&Page {
                url: &page.url,
                title: &page.title,
                body: &page.body,
                lang: &page.lang,
                snippet: "",
            })?;
            indexed += 1;
            if indexed % 50 == 0 {
                index.commit()?;
                let queued = crawl.queue.lock().map(|q| q.len).unwrap_or(0);
                println!("indexed {indexed} pages (queue {queued})");
                let _ = std::io::stdout().flush();
            }
            if indexed >= options.max_pages {
                truncated = true;
                break;
            }
        }
        index.commit()
    })();
    crawl.stop();
    drop(receiver);
    let mut worker_failed = false;
    for worker in workers {
        worker_failed |= worker.join().is_err();
    }
    if worker_failed {
        return Err("crawler worker panicked".into());
    }
    writing?;

    // Deletion sync. Only a complete pass over the scope may delete: a run
    // that stopped at a limit leaves documents unvisited for a reason that is
    // not a removal.
    if options.prune_missing {
        let counters = &crawl.counters;
        let capped = truncated
            || counters.queue_capped.load(Ordering::Relaxed) > 0
            || counters.host_capped.load(Ordering::Relaxed) > 0;
        if capped {
            println!(
                "prune skipped: this run stopped at a limit (--max-pages, --max-per-host or --max-queue), so unvisited documents are not deletions"
            );
        } else {
            let seen = crawl
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            prune_missing(&index, options, seed_urls, &seen)?;
        }
    }

    let counters = &crawl.counters;
    println!(
        "crawled {indexed} pages, index now has {} documents",
        index.document_count()
    );
    println!(
        "skipped: {} by robots.txt, {} noindex, {} duplicate text, {} over the per-host cap, {} over the queue cap; {} failed, {} back-offs",
        counters.robots_blocked.load(Ordering::Relaxed),
        counters.noindex.load(Ordering::Relaxed),
        counters.duplicates.load(Ordering::Relaxed),
        counters.host_capped.load(Ordering::Relaxed),
        counters.queue_capped.load(Ordering::Relaxed),
        counters.failed.load(Ordering::Relaxed),
        counters.backoffs.load(Ordering::Relaxed),
    );
    let visited = crawl.seen.lock().map(|seen| seen.len()).unwrap_or_default();
    Ok(CrawlSummary {
        indexed,
        visited,
        failed: counters.failed.load(Ordering::Relaxed),
        backoffs: counters.backoffs.load(Ordering::Relaxed),
    })
}

/// Run the sites of a plan, each as its own pass.
///
/// The state file records the next due time, so an external scheduler can
/// start this command as often as it likes: with `--due` only the sites whose
/// interval has elapsed are crawled. Without `--due` every site is crawled and
/// rescheduled, which is the manual "refresh now" form.
fn crawl_plan(options: &Options, client: reqwest::blocking::Client) -> Result<(), String> {
    if options.seeds.is_some() {
        return Err("--plan lists the seeds; do not pass --seeds as well".to_string());
    }
    let plan_path = options
        .plan
        .as_ref()
        .ok_or("crawl needs --seeds <file> or --plan <file>")?;
    let text = std::fs::read_to_string(plan_path)
        .map_err(|error| format!("read {plan_path:?}: {error}"))?;
    let plan = CrawlPlan::parse(&text).map_err(|error| format!("{plan_path:?}: {error}"))?;
    if plan.is_empty() {
        return Err(format!("{plan_path:?} declares no sites"));
    }
    validate_crawl(options)?;
    if let Some(text) = options.since.as_deref() {
        // Check the instant and the flag once, not once per site, so a plan
        // that is not due today still reports a broken command line.
        chrono::DateTime::parse_from_rfc3339(text)
            .map_err(|error| format!("--since is not an RFC 3339 instant ({text}): {error}"))?;
        let sitemaps = |site: &PlannedSite| site.sitemaps.unwrap_or(options.use_sitemaps);
        if !plan.sites().iter().all(sitemaps) {
            return Err(
                "--since needs --sitemaps for every planned site: only sitemap URLs are skipped, so link discovery keeps working"
                    .into(),
            );
        }
    }
    let mut schedule = Schedule::load(&options.index)?;
    let now = schedule::unix_now();
    for site in plan.sites() {
        schedule.plan_site(&site.name, site.interval_seconds, now);
    }
    let due = plan
        .sites()
        .iter()
        .filter(|site| !options.due || schedule.is_due(&site.name, now))
        .count();
    if due > 0 {
        // Take the index's write lock once before any outcome is recorded. A
        // second crawler has to fail here instead of looking like a site
        // failure, which would push that site's next run out by an interval.
        drop(LocalIndex::open_for_write(&options.index)?);
    }
    println!(
        "plan: {} sites, {} due{}",
        plan.sites().len(),
        due,
        if options.due { "" } else { " (forced run)" }
    );
    let mut failed: Vec<String> = Vec::new();
    for site in plan.sites() {
        if options.due && !schedule.is_due(&site.name, now) {
            let next_due = schedule
                .get(&site.name)
                .map(|state| state.next_due)
                .unwrap_or(now);
            println!(
                "site {}: waiting until {}",
                site.name,
                format_time(next_due)
            );
            continue;
        }
        let mut site_options = options.clone();
        site_options.plan = None;
        site_options.due = false;
        site_options.seeds = None;
        site_options.use_sitemaps = site.sitemaps.unwrap_or(options.use_sitemaps);
        site_options.max_pages = site.max_pages.unwrap_or(options.max_pages);
        site_options.within_seed_path = site.within_seed_path.unwrap_or(options.within_seed_path);
        // A site's own scope is only meaningful for the deletion pass; a
        // run without --prune-missing ignores it instead of failing.
        if options.prune_missing && !site.scope.is_empty() {
            site_options.scope = site.scope.clone();
        }
        println!(
            "site {}: crawling {} seed(s), interval {}s",
            site.name,
            site.seeds.len(),
            site.interval_seconds
        );
        let started = schedule::unix_now();
        let outcome = crawl_seeds(&site_options, client.clone(), &site.seeds);
        let finished = schedule::unix_now();
        let error = match outcome {
            Ok(summary) if !summary.is_site_failure() => {
                schedule.record_success(&site.name, site.interval_seconds, started, finished);
                None
            }
            Ok(summary) => {
                schedule.record_failure(&site.name, site.interval_seconds, started, finished);
                Some(format!(
                    "nothing could be indexed ({} visited, {} failed, {} back-offs)",
                    summary.visited, summary.failed, summary.backoffs
                ))
            }
            Err(error) => {
                schedule.record_failure(&site.name, site.interval_seconds, started, finished);
                Some(error)
            }
        };
        // Save after every site: a crash or a kill mid-plan keeps the outcome
        // of the sites that already ran.
        schedule.save(&options.index)?;
        match error {
            Some(error) => {
                eprintln!("site {}: failed: {error}", site.name);
                failed.push(site.name.clone());
            }
            None => {
                let next_due = schedule
                    .get(&site.name)
                    .map(|state| state.next_due)
                    .unwrap_or(finished);
                println!(
                    "site {}: ok, next crawl at {}",
                    site.name,
                    format_time(next_due)
                );
            }
        }
    }
    schedule.save(&options.index)?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} of {} sites failed: {}",
            failed.len(),
            plan.sites().len(),
            failed.join(", ")
        ))
    }
}

/// A Unix timestamp as RFC 3339 UTC, for the schedule output.
fn format_time(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::UNIX_EPOCH + Duration::from_secs(seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Whether the deletion pass covers a URL: one of the `--scope` prefixes, or
/// else the hosts of the seeds (their directories with `--within-seed-path`).
fn prune_covers(url: &str, options: &Options, seeds: &[String]) -> bool {
    if !options.scope.is_empty() {
        return options
            .scope
            .iter()
            .any(|prefix| url.starts_with(prefix.as_str()));
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    seeds.iter().any(|seed| {
        let Ok(origin) = url::Url::parse(seed) else {
            return false;
        };
        same_site(&origin, &parsed)
            && (!options.within_seed_path
                || parsed.path().starts_with(seed_directory(seed).as_str()))
    })
}

/// Delete the documents in scope that this run never visited, printing each
/// URL it removes. With `--dry-run` the list is printed and nothing changes.
fn prune_missing(
    index: &LocalIndex,
    options: &Options,
    seeds: &[String],
    seen: &HashSet<String>,
) -> Result<(), String> {
    let mut missing: Vec<String> = index
        .urls()?
        .into_iter()
        .filter(|url| prune_covers(url, options, seeds) && !seen.contains(url))
        .collect();
    missing.sort();
    if missing.is_empty() {
        println!("prune: no documents missing");
        return Ok(());
    }
    let total = missing.len();
    if options.dry_run {
        println!("would prune {total} documents not seen in this run (dry run):");
        for url in &missing {
            println!("  {url}");
        }
        return Ok(());
    }
    println!("pruned {total} documents not seen in this run:");
    for url in &missing {
        index.remove(url)?;
        println!("  {url}");
    }
    index.commit()?;
    Ok(())
}

fn queue_links(crawl: &Crawl, links: Vec<String>, depth: usize) {
    if let Ok(mut queue) = crawl.queue.lock() {
        for link in links {
            if !crawl.in_scope(&link) || queue.scheduled.contains(&link) {
                continue;
            }
            if queue.len >= crawl.max_queue {
                crawl.counters.queue_capped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            queue.push(link, depth);
        }
    }
}

/// How long to stay away after a 429/503, from `Retry-After` when it is given
/// in seconds.
fn retry_after(response: &reqwest::blocking::Response) -> Duration {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BACKOFF)
        .min(MAX_BACKOFF)
}

/// The language of a page: its `lang` attribute, else a script heuristic.
fn page_language(html: &Html, title: &str, body: &str) -> String {
    if let Ok(selector) = Selector::parse("html[lang]") {
        if let Some(lang) = html
            .select(&selector)
            .next()
            .and_then(|element| element.value().attr("lang"))
        {
            let primary = lang.split(['-', '_']).next().unwrap_or("").to_lowercase();
            if (2..=3).contains(&primary.len()) {
                return primary;
            }
        }
    }
    if has_cjk(title) || has_cjk(body) {
        "ja".to_string()
    } else {
        "en".to_string()
    }
}

/// Fetch one page and extract what the index needs.
fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    depth: usize,
    max_depth: usize,
    crawl: &Crawl,
    delay: Duration,
) -> Fetch {
    let counters = &crawl.counters;
    let Ok(parsed) = url::Url::parse(url) else {
        counters.failed.fetch_add(1, Ordering::Relaxed);
        return Fetch::Skipped;
    };
    let response = match crawl.request(client, parsed, delay, Resource::Page) {
        Ok(response) => response,
        Err(FetchError::RobotsBlocked) => {
            counters.robots_blocked.fetch_add(1, Ordering::Relaxed);
            return Fetch::Skipped;
        }
        Err(FetchError::Backoff) => return Fetch::Backoff,
        Err(FetchError::Failed) => {
            counters.failed.fetch_add(1, Ordering::Relaxed);
            return Fetch::Skipped;
        }
    };
    let base = response.url().clone();
    let mut aliases = vec![url.to_string()];
    if base.as_str() != url {
        aliases.push(base.to_string());
    }
    if matches!(response.status().as_u16(), 404 | 410) {
        return Fetch::Remove {
            urls: aliases,
            links: Vec::new(),
        };
    }
    if !response.status().is_success() {
        counters.failed.fetch_add(1, Ordering::Relaxed);
        return Fetch::Skipped;
    }
    let mut directives = response
        .headers()
        .get_all("x-robots-tag")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(crawl::header_directives)
        .fold(Directives::default(), |all, one| Directives {
            noindex: all.noindex || one.noindex,
            nofollow: all.nofollow || one.nofollow,
        });
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    if !matches!(media_type, "text/html" | "application/xhtml+xml") {
        return if directives.noindex {
            Fetch::Remove {
                urls: aliases,
                links: Vec::new(),
            }
        } else {
            Fetch::Skipped
        };
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        counters.failed.fetch_add(1, Ordering::Relaxed);
        return if directives.noindex {
            Fetch::Remove {
                urls: aliases,
                links: Vec::new(),
            }
        } else {
            Fetch::Skipped
        };
    }
    let Some(bytes) = read_capped(response, MAX_BODY_BYTES) else {
        counters.failed.fetch_add(1, Ordering::Relaxed);
        return if directives.noindex {
            Fetch::Remove {
                urls: aliases,
                links: Vec::new(),
            }
        } else {
            Fetch::Skipped
        };
    };
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let html = Html::parse_document(&body);
    let meta = crawl::meta_directives(&html);
    directives.noindex |= meta.noindex;
    directives.nofollow |= meta.nofollow;

    let found = if depth < max_depth && !directives.nofollow {
        links(&html, &base)
            .into_iter()
            .filter(|link| {
                url::Url::parse(link)
                    .map(|target| {
                        same_site(&base, &target) && rustxng::fetch::is_fetchable(&target)
                    })
                    .unwrap_or(false)
            })
            .collect()
    } else {
        Vec::new()
    };
    if directives.noindex {
        counters.noindex.fetch_add(1, Ordering::Relaxed);
        return Fetch::Remove {
            urls: aliases,
            links: found,
        };
    }
    let title = page_title(&html);
    let text = rustxng::content::main_text_limited(&html, 20_000);
    if title.is_empty() && text.is_empty() {
        return Fetch::Links(found);
    }
    let canonical = crawl::canonical_url(&html, &base)
        .or_else(|| crawl::normalize_url(base.as_str()))
        .unwrap_or_else(|| base.to_string());
    let lang = page_language(&html, &title, &text);
    Fetch::Page(Fetched {
        url: canonical,
        aliases,
        title,
        body: text,
        lang,
        links: found,
        depth,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scope_is_the_seed_directory() {
        assert_eq!(seed_directory("https://docs.python.org/3/"), "/3/");
        assert_eq!(seed_directory("https://www.sqlite.org/docs.html"), "/");
        assert_eq!(seed_directory("https://go.dev/doc/index.html"), "/doc/");
        let mut crawl = Crawl::new();
        crawl.scope = Some(HashMap::from([(
            host_key("https://docs.python.org/3/"),
            vec!["/3/".to_string()],
        )]));
        assert!(crawl.in_scope("https://docs.python.org/3/library/re.html"));
        assert!(!crawl.in_scope("https://docs.python.org/ja/3/library/re.html"));
        assert!(!crawl.in_scope("https://example.org/3/"));
    }

    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::{SocketAddr, TcpListener, TcpStream};

    fn mock_client(address: SocketAddr, contact: &str) -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("example.test", address)
            .resolve("www.example.test", address)
            .user_agent(crawl::user_agent(contact))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    struct TestServer {
        address: SocketAddr,
        base: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn new(handler: impl Fn(&str, &str, &str) -> String + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let base = format!("http://example.test:{}", address.port());
            let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
            let log = Arc::clone(&requests);
            let origin = base.clone();
            let worker = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(&stream);
                    let mut request = String::new();
                    if reader.read_line(&mut request).unwrap() == 0 {
                        break;
                    }
                    let path = request.split_whitespace().nth(1).unwrap();
                    let mut host = String::new();
                    for header in reader.lines() {
                        let header = header.unwrap();
                        if header.is_empty() {
                            break;
                        }
                        if let Some(value) = header.to_ascii_lowercase().strip_prefix("host:") {
                            host = value.trim().into();
                        }
                    }
                    log.lock().unwrap().push((host.clone(), path.into()));
                    let _ = stream.write_all(handler(&host, path, &origin).as_bytes());
                }
            });
            Self {
                address,
                base,
                requests,
                worker: Some(worker),
            }
        }

        fn client(&self) -> reqwest::blocking::Client {
            mock_client(self.address, "")
        }

        fn url(&self, path: &str) -> url::Url {
            url::Url::parse(&format!("{}{path}", self.base)).unwrap()
        }

        fn paths(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|(_, path)| path.clone())
                .collect()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            drop(TcpStream::connect(self.address).unwrap());
            let result = self.worker.take().unwrap().join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }

    fn reply(status: u16, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn redirects_check_robots_before_reading_the_target() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", "User-agent: *\nDisallow: /private/*$\n"),
            "/start" => reply(302, "Location: /private/secret\r\n", ""),
            _ => panic!("forbidden request: {path}"),
        });
        let state = Crawl::new();
        assert!(matches!(
            state.request(
                &server.client(),
                server.url("/start"),
                Duration::ZERO,
                Resource::Page
            ),
            Err(FetchError::RobotsBlocked)
        ));
        assert_eq!(server.paths(), ["/robots.txt", "/start"]);
    }

    #[test]
    fn redirects_cannot_switch_ports_or_reach_literal_private_addresses() {
        let private = TcpListener::bind("127.0.0.1:0").unwrap();
        private.set_nonblocking(true).unwrap();
        let address = private.local_addr().unwrap();
        let server = TestServer::new(move |_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/port" => reply(
                302,
                &format!(
                    "Location: http://example.test:{}/secret\r\n",
                    address.port()
                ),
                "",
            ),
            "/private" => reply(302, &format!("Location: http://{address}/secret\r\n"), ""),
            _ => panic!("unexpected request: {path}"),
        });
        let state = Crawl::new();
        for path in ["/port", "/private"] {
            assert!(matches!(
                state.request(
                    &server.client(),
                    server.url(path),
                    Duration::ZERO,
                    Resource::Page
                ),
                Err(FetchError::Failed)
            ));
        }
        assert_eq!(
            private.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(server.paths(), ["/robots.txt", "/port", "/private"]);
    }

    #[test]
    fn a_www_redirect_uses_the_new_origins_robots_policy() {
        let server = TestServer::new(|host, path, base| match path {
            "/robots.txt" if host.starts_with("www.") => {
                reply(200, "", "User-agent: *\nDisallow: /\n")
            }
            "/robots.txt" => reply(200, "", ""),
            "/start" => reply(
                302,
                &format!(
                    "Location: {}/final\r\n",
                    base.replace("example.test", "www.example.test")
                ),
                "",
            ),
            _ => panic!("forbidden request: {path}"),
        });
        assert!(matches!(
            Crawl::new().request(
                &server.client(),
                server.url("/start"),
                Duration::ZERO,
                Resource::Page
            ),
            Err(FetchError::RobotsBlocked)
        ));
        assert_eq!(server.paths(), ["/robots.txt", "/start", "/robots.txt"]);
    }

    #[test]
    fn redirected_pages_resolve_links_against_the_final_address() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/old" => reply(302, "Location: /new/\r\n", ""),
            "/new/" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>New</title><a href='child'>child</a>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let state = Crawl::new();
        let Fetch::Page(page) = fetch(
            &server.client(),
            server.url("/old").as_str(),
            0,
            1,
            &state,
            Duration::ZERO,
        ) else {
            panic!("redirected page was not indexed");
        };
        assert_eq!(page.url, server.url("/new/").as_str());
        assert_eq!(page.links, [server.url("/new/child").to_string()]);
    }

    #[test]
    fn unreadable_or_oversized_robots_never_becomes_allow_all() {
        for oversized in [false, true] {
            let server = TestServer::new(move |_, path, _| {
                assert_eq!(path, "/robots.txt");
                if oversized {
                    reply(200, "", &"x".repeat(ROBOTS_MAX_BYTES + 1))
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"
                        .into()
                }
            });
            assert!(matches!(
                Crawl::new().request(
                    &server.client(),
                    server.url("/page"),
                    Duration::ZERO,
                    Resource::Page
                ),
                Err(FetchError::RobotsBlocked)
            ));
            assert_eq!(server.paths(), ["/robots.txt"]);
        }
    }

    #[test]
    fn noindex_headers_remove_entries_even_when_the_body_cannot_be_read() {
        for declared in [100, MAX_BODY_BYTES + 1] {
            let server = TestServer::new(move |_, path, _| {
                if path == "/robots.txt" {
                    return reply(200, "", "");
                }
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Robots-Tag: noindex\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\nshort"
                )
            });
            let outcome = fetch(
                &server.client(),
                server.url("/page").as_str(),
                0,
                1,
                &Crawl::new(),
                Duration::ZERO,
            );
            let Fetch::Remove { urls, links } = outcome else {
                panic!("noindex header was ignored");
            };
            assert_eq!(urls, [server.url("/page").to_string()]);
            assert!(links.is_empty());
        }
    }

    #[test]
    fn sitemaps_cannot_redirect_crawls_to_another_origin() {
        let server = TestServer::new(|_, path, _| {
            assert_eq!(path, "/robots.txt");
            reply(
                200,
                "",
                "Sitemap: http://127.0.0.1/private\nSitemap: https://foreign.test/index.xml\n",
            )
        });
        let mut options = Options::parse(&[]);
        options.delay_ms = 0;
        let urls = sitemap_urls(&server.client(), &Crawl::new(), &server.url("/"), &options);
        assert!(urls.is_empty());
        assert_eq!(server.paths(), ["/robots.txt"]);
    }

    #[test]
    fn sitemap_requests_are_bounded_even_when_no_pages_are_in_scope() {
        let server = TestServer::new(|_, path, base| {
            if path == "/robots.txt" {
                return reply(200, "", &format!("Sitemap: {base}/index.xml\n"));
            }
            let mut xml = format!("<sitemapindex><sitemap><loc>{base}/index.xml</loc></sitemap>");
            for number in 0..100 {
                xml.push_str(&format!(
                    "<sitemap><loc>{base}/map-{number}.xml</loc></sitemap>"
                ));
            }
            xml.push_str("</sitemapindex>");
            reply(200, "", &xml)
        });
        let mut options = Options::parse(&[]);
        options.delay_ms = 0;
        options.max_sitemaps = 4;
        let mut state = Crawl::new();
        state.scope = Some(HashMap::from([(
            host_key(server.url("/").as_str()),
            vec!["/docs/".into()],
        )]));
        assert!(sitemap_urls(&server.client(), &state, &server.url("/docs/"), &options).is_empty());
        let paths = server.paths();
        assert_eq!(
            paths.iter().filter(|path| path.ends_with(".xml")).count(),
            4
        );
        assert_eq!(paths.iter().filter(|path| *path == "/index.xml").count(), 1);
    }

    #[test]
    fn sitemap_page_limits_apply_after_scope_filtering() {
        let server = TestServer::new(|_, path, base| {
            if path == "/robots.txt" {
                return reply(200, "", &format!("Sitemap: {base}/index.xml\n"));
            }
            let mut xml = String::from("<urlset>");
            for number in 0..70 {
                xml.push_str(&format!("<url><loc>{base}/outside/{number}</loc></url>"));
            }
            xml.push_str(&format!("<url><loc>{base}/docs/kept</loc></url></urlset>"));
            reply(200, "", &xml)
        });
        let mut options = Options::parse(&[]);
        options.delay_ms = 0;
        options.max_pages = 1;
        let mut state = Crawl::new();
        state.scope = Some(HashMap::from([(
            host_key(server.url("/").as_str()),
            vec!["/docs/".into()],
        )]));
        assert_eq!(
            sitemap_urls(&server.client(), &state, &server.url("/docs/"), &options),
            [server.url("/docs/kept").to_string()]
        );
    }

    #[test]
    fn the_frontier_bounds_pending_urls_and_deduplicates_cycles() {
        let mut state = Crawl::new();
        state.max_queue = 2;
        queue_links(&state, vec!["https://example.test/a".into(); 100], 0);
        queue_links(
            &state,
            vec![
                "https://example.test/b".into(),
                "https://example.test/c".into(),
            ],
            0,
        );
        assert_eq!(state.queue.lock().unwrap().len, 2);
        assert_eq!(state.counters.queue_capped.load(Ordering::Relaxed), 1);
        let first = state.queue.lock().unwrap().pop(&HashMap::new()).unwrap();
        queue_links(&state, vec![first.0], 1);
        assert_eq!(state.queue.lock().unwrap().len, 1);
    }

    #[test]
    fn shutdown_wakes_workers_waiting_for_backoff() {
        let state = Arc::new(Crawl::new());
        state.back_off("example.test", MAX_BACKOFF);
        let waiting = Arc::clone(&state);
        let worker =
            std::thread::spawn(move || waiting.wait_for_host("example.test", Duration::ZERO));
        state.stop();
        assert!(!worker.join().unwrap());
    }

    #[test]
    fn recrawls_update_changed_pages_and_remove_noindex_gone_and_duplicate_content() {
        let stage = Arc::new(AtomicUsize::new(0));
        let current = Arc::clone(&stage);
        let server = TestServer::new(move |_, path, _| {
            if path == "/robots.txt" {
                return reply(200, "", "");
            }
            let second = current.load(Ordering::SeqCst) == 1;
            let (status, body) = match (path, second) {
                ("/", _) => (
                    200,
                    "<title>Root</title><a href='/changed'>changed</a><a href='/hidden'>hidden</a><a href='/gone'>gone</a><a href='/copy'>copy</a>",
                ),
                ("/changed", false) => (200, "<title>Changing</title><p>obsoletevalue</p>"),
                ("/changed", true) => (200, "<title>Changing</title><p>replacementvalue</p>"),
                ("/hidden", false) => (200, "<title>Hidden</title><p>withdrawnvalue</p>"),
                ("/hidden", true) => (
                    200,
                    "<meta name='robots' content='noindex'><title>Hidden</title><p>withdrawnvalue</p>",
                ),
                ("/gone", false) => (200, "<title>Gone</title><p>deletedvalue</p>"),
                ("/gone", true) => (410, "gone"),
                ("/copy", false) => (200, "<title>Copy</title><p>stalevalue</p>"),
                ("/copy", true) => (200, "<title>Copy</title><p>replacementvalue</p>"),
                _ => panic!("unexpected request: {path}"),
            };
            reply(status, "Content-Type: text/html\r\n", body)
        });
        let directory = tempfile::tempdir().unwrap();
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, server.url("/").as_str()).unwrap();
        let mut options = Options::parse(&[]);
        options.seeds = Some(seeds);
        options.index = directory.path().join("index");
        options.delay_ms = 0;
        options.workers = 1;
        crawl_with_client(&options, server.client()).unwrap();
        assert_eq!(
            LocalIndex::open(&options.index).unwrap().document_count(),
            5
        );
        stage.store(1, Ordering::SeqCst);
        crawl_with_client(&options, server.client()).unwrap();
        let index = LocalIndex::open(&options.index).unwrap();
        assert_eq!(index.document_count(), 2);
        for old in [
            "obsoletevalue",
            "withdrawnvalue",
            "deletedvalue",
            "stalevalue",
        ] {
            assert!(
                index.search(old, 10).unwrap().is_empty(),
                "stale text: {old}"
            );
        }
        let hits = index.search("replacementvalue", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, server.url("/changed").as_str());
    }

    /// A crawl whose seeds are the example.test root.
    fn crawl_options(directory: &tempfile::TempDir, seed: &str) -> Options {
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, seed).unwrap();
        let mut options = Options::parse(&[]);
        options.seeds = Some(seeds);
        options.index = directory.path().join("index");
        options.delay_ms = 0;
        options.workers = 1;
        options
    }

    #[test]
    fn the_prune_scope_is_the_seed_host_and_scope_prefixes() {
        let mut options = Options::parse(&[]);
        let seeds = vec!["https://docs.example/3/".to_string()];
        assert!(prune_covers("https://docs.example/3/x", &options, &seeds));
        assert!(!prune_covers("https://other.example/3/x", &options, &seeds));
        options.within_seed_path = true;
        assert!(!prune_covers(
            "https://docs.example/ja/3/x",
            &options,
            &seeds
        ));
        options.scope = vec!["https://other.example/".into()];
        assert!(prune_covers("https://other.example/x", &options, &seeds));
        assert!(!prune_covers("https://docs.example/3/x", &options, &seeds));
    }

    #[test]
    fn new_flags_are_validated_before_crawling() {
        let directory = tempfile::tempdir().unwrap();
        let mut options = crawl_options(&directory, "https://example.test/");
        let client = mock_client("127.0.0.1:9".parse().unwrap(), "");
        options.dry_run = true;
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("--dry-run needs --prune-missing"), "{error}");

        options.dry_run = false;
        options.prune_missing = true;
        options.skip_indexed = true;
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("--skip-indexed"), "{error}");

        options.skip_indexed = false;
        options.since = Some("2026-01-01T00:00:00Z".into());
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("cannot be combined with --since"), "{error}");

        options.prune_missing = false;
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("--since needs --sitemaps"), "{error}");

        options.use_sitemaps = true;
        options.since = Some("yesterday".into());
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("not an RFC 3339 instant"), "{error}");

        options.since = None;
        options.prune_missing = true;
        options.scope = vec!["not a url".into()];
        let error = crawl_with_client(&options, client).unwrap_err();
        assert!(error.contains("--scope is not a URL"), "{error}");
    }

    #[test]
    fn prune_missing_removes_documents_the_run_never_visited() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Root</title><a href='/kept'>kept</a>",
            ),
            "/kept" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Kept</title><p>keptvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let mut options = crawl_options(&directory, server.url("/").as_str());
        options.prune_missing = true;
        options.dry_run = true;
        let gone = server.url("/gone").to_string();
        {
            let index = LocalIndex::open_for_write(&options.index).unwrap();
            index
                .add(&Page {
                    url: &gone,
                    title: "Gone",
                    body: "gonevalue",
                    lang: "en",
                    snippet: "",
                })
                .unwrap();
            index.commit().unwrap();
        }
        crawl_with_client(&options, server.client()).unwrap();
        assert!(
            LocalIndex::open(&options.index).unwrap().has_url(&gone),
            "a dry run must not delete"
        );

        options.dry_run = false;
        crawl_with_client(&options, server.client()).unwrap();
        let index = LocalIndex::open(&options.index).unwrap();
        assert!(!index.has_url(&gone));
        assert!(index.has_url(server.url("/").as_str()));
        assert!(index.has_url(server.url("/kept").as_str()));
    }

    #[test]
    fn prune_keeps_documents_whose_address_was_redirected() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/old" => reply(302, "Location: /new\r\n", ""),
            "/new" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>New</title><p>newvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let mut options = crawl_options(&directory, server.url("/old").as_str());
        options.prune_missing = true;
        crawl_with_client(&options, server.client()).unwrap();
        // The document is stored under /new but was requested as /old; the
        // deletion pass must not read that as "not visited".
        let index = LocalIndex::open(&options.index).unwrap();
        assert_eq!(index.document_count(), 1);
        assert!(index.has_url(server.url("/new").as_str()));
    }

    #[test]
    fn a_truncated_run_never_prunes() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Root</title><a href='/kept'>kept</a>",
            ),
            "/kept" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Kept</title><p>keptvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let mut options = crawl_options(&directory, server.url("/").as_str());
        options.prune_missing = true;
        options.max_pages = 1;
        let gone = server.url("/gone").to_string();
        {
            let index = LocalIndex::open_for_write(&options.index).unwrap();
            index
                .add(&Page {
                    url: &gone,
                    title: "Gone",
                    body: "gonevalue",
                    lang: "en",
                    snippet: "",
                })
                .unwrap();
            index.commit().unwrap();
        }
        crawl_with_client(&options, server.client()).unwrap();
        assert!(
            LocalIndex::open(&options.index).unwrap().has_url(&gone),
            "a run stopped at --max-pages must not delete"
        );
    }

    #[test]
    fn since_skips_documents_that_are_already_fresh() {
        let server = TestServer::new(|_, path, base| match path {
            "/robots.txt" => reply(200, "", &format!("Sitemap: {base}/sitemap.xml\n")),
            "/sitemap.xml" => {
                let mut xml = String::from("<urlset>");
                for page in ["/a", "/b"] {
                    xml.push_str(&format!("<url><loc>{base}{page}</loc></url>"));
                }
                xml.push_str("</urlset>");
                reply(200, "", &xml)
            }
            "/" => reply(200, "Content-Type: text/html\r\n", "<title>Root</title>"),
            "/a" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>A</title><p>avalue</p>",
            ),
            "/b" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>B</title><p>bvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let mut options = crawl_options(&directory, server.url("/").as_str());
        options.use_sitemaps = true;
        options.max_pages = 10;
        crawl_with_client(&options, server.client()).unwrap();
        let index = LocalIndex::open(&options.index).unwrap();
        assert!(index.has_url(server.url("/a").as_str()));
        assert!(index.has_url(server.url("/b").as_str()));
        drop(index);

        // Fresh: indexed after the cutoff, so neither page is fetched again.
        options.since = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(3600))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let before = server.paths().len();
        crawl_with_client(&options, server.client()).unwrap();
        let fresh: Vec<String> = server.paths()[before..]
            .iter()
            .filter(|path| path.as_str() == "/a" || path.as_str() == "/b")
            .cloned()
            .collect();
        assert!(fresh.is_empty(), "fresh pages were refetched: {fresh:?}");

        // Stale: indexed before the cutoff, so both pages are refetched.
        options.since = Some(
            (chrono::Utc::now() + chrono::Duration::seconds(3600))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let before = server.paths().len();
        crawl_with_client(&options, server.client()).unwrap();
        let stale: Vec<String> = server.paths()[before..]
            .iter()
            .filter(|path| path.as_str() == "/a" || path.as_str() == "/b")
            .cloned()
            .collect();
        assert_eq!(stale, ["/a", "/b"]);
    }

    /// Options for a plan run over a temporary index.
    fn plan_options(directory: &tempfile::TempDir, plan: &Path) -> Options {
        let mut options = Options::parse(&[]);
        options.plan = Some(plan.to_path_buf());
        options.index = directory.path().join("index");
        options.delay_ms = 0;
        options.workers = 1;
        options
    }

    fn write_plan(directory: &tempfile::TempDir, text: &str) -> PathBuf {
        let path = directory.path().join("crawl-plan.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_plan_crawls_only_due_sites_and_records_the_next_run() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Root</title><p>rootvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let plan = write_plan(
            &directory,
            &format!(
                "[[site]]\nname = \"example\"\nseeds = [\"{}\"]\ninterval = \"24h\"\n",
                server.url("/")
            ),
        );
        let mut options = plan_options(&directory, &plan);
        options.due = true;
        crawl_plan(&options, server.client()).unwrap();
        assert_eq!(
            LocalIndex::open(&options.index).unwrap().document_count(),
            1
        );
        let state = Schedule::load(&options.index).unwrap();
        let site = state.get("example").unwrap();
        assert_eq!(site.interval_seconds, 86_400);
        assert_eq!(site.consecutive_failures, 0);
        assert!(site.last_success.is_some());
        assert!(site.next_due > schedule::unix_now());
        let requests = server.paths().len();
        assert!(requests > 0);

        // A later index commit leaves the schedule file alone: it is not part
        // of the index's managed set.
        {
            let index = LocalIndex::open_for_write(&options.index).unwrap();
            index.commit().unwrap();
        }
        assert!(
            Schedule::load(&options.index)
                .unwrap()
                .get("example")
                .is_some()
        );

        // The interval has not elapsed: the next invocation does nothing.
        crawl_plan(&options, server.client()).unwrap();
        assert_eq!(server.paths().len(), requests);

        // An elapsed interval makes the site due again.
        std::fs::write(
            options.index.join(schedule::SCHEDULE_FILE),
            serde_json::json!({
                "version": 1,
                "sites": {
                    "example": {
                        "interval_seconds": 86_400,
                        "last_attempt": 1,
                        "last_success": 1,
                        "consecutive_failures": 0,
                        "next_due": 0
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        crawl_plan(&options, server.client()).unwrap();
        assert!(server.paths().len() > requests);
    }

    #[test]
    fn a_failing_site_backs_off_and_a_success_resets_the_interval() {
        let recovered = Arc::new(AtomicBool::new(false));
        let current = Arc::clone(&recovered);
        let server = TestServer::new(move |_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/" if current.load(Ordering::SeqCst) => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Root</title><p>rootvalue</p>",
            ),
            "/" => reply(500, "", "temporarily unavailable"),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let plan = write_plan(
            &directory,
            &format!(
                "[[site]]\nname = \"example\"\nseeds = [\"{}\"]\ninterval = \"1h\"\n",
                server.url("/")
            ),
        );
        let options = plan_options(&directory, &plan);
        // Every page of the site failed: the site itself is in trouble, so
        // the scheduler has to back off instead of waiting the interval.
        let error = crawl_plan(&options, server.client()).unwrap_err();
        assert!(error.contains("1 of 1 sites failed"), "{error}");
        assert!(!server.paths().is_empty());
        let state = Schedule::load(&options.index).unwrap();
        let site = state.get("example").unwrap();
        assert_eq!(site.consecutive_failures, 1);
        assert!(site.last_success.is_none());
        assert!(site.next_due >= site.last_attempt.unwrap() + 3_600);

        // The same site succeeds next time; the counter and the wait reset.
        recovered.store(true, Ordering::SeqCst);
        crawl_plan(&options, server.client()).unwrap();
        let state = Schedule::load(&options.index).unwrap();
        let site = state.get("example").unwrap();
        assert_eq!(site.consecutive_failures, 0);
        assert_eq!(site.next_due, site.last_success.unwrap() + 3_600);
        assert!(site.last_success.unwrap() >= site.last_attempt.unwrap());
    }

    #[test]
    fn a_planned_pass_prunes_only_inside_its_own_site() {
        let server = TestServer::new(|_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Root</title><p>rootvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let plan = write_plan(
            &directory,
            &format!(
                "[[site]]\nname = \"example\"\nseeds = [\"{}\"]\ninterval = \"24h\"\n",
                server.url("/")
            ),
        );
        let mut options = plan_options(&directory, &plan);
        options.prune_missing = true;
        let inside = server.url("/gone").to_string();
        let outside = "https://other.example/gone".to_string();
        {
            let index = LocalIndex::open_for_write(&options.index).unwrap();
            for url in [&inside, &outside] {
                index
                    .add(&Page {
                        url,
                        title: "Gone",
                        body: "gonevalue",
                        lang: "en",
                        snippet: "",
                    })
                    .unwrap();
            }
            index.commit().unwrap();
        }
        crawl_plan(&options, server.client()).unwrap();
        let index = LocalIndex::open(&options.index).unwrap();
        assert!(
            !index.has_url(&inside),
            "an unvisited document of the site is a deletion"
        );
        assert!(
            index.has_url(&outside),
            "another site's documents are out of scope"
        );
    }

    #[test]
    fn a_planned_pass_applies_the_freshness_cutoff() {
        let server = TestServer::new(|_, path, base| match path {
            "/robots.txt" => reply(200, "", &format!("Sitemap: {base}/sitemap.xml\n")),
            "/sitemap.xml" => reply(
                200,
                "",
                &format!(
                    "<urlset><url><loc>{base}/a</loc></url><url><loc>{base}/b</loc></url></urlset>"
                ),
            ),
            "/" => reply(200, "Content-Type: text/html\r\n", "<title>Root</title>"),
            "/a" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>A</title><p>avalue</p>",
            ),
            "/b" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>B</title><p>bvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let plan = write_plan(
            &directory,
            &format!(
                "[[site]]\nname = \"example\"\nseeds = [\"{}\"]\ninterval = \"24h\"\nsitemaps = true\n",
                server.url("/")
            ),
        );
        let mut options = plan_options(&directory, &plan);
        options.due = true;
        options.max_pages = 10;
        crawl_plan(&options, server.client()).unwrap();
        {
            let index = LocalIndex::open(&options.index).unwrap();
            assert_eq!(index.document_count(), 3);
        }

        // Every page was indexed after this cutoff, so the plan refetches
        // nothing. A plan run without --due runs even though the site is not
        // due, which is how an operator asks for "check again now".
        options.due = false;
        options.since = Some(
            (chrono::Utc::now() - chrono::Duration::seconds(3_600))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let before = server.paths().len();
        crawl_plan(&options, server.client()).unwrap();
        let fresh: Vec<String> = server.paths()[before..]
            .iter()
            .filter(|path| path.as_str() == "/a" || path.as_str() == "/b")
            .cloned()
            .collect();
        assert!(fresh.is_empty(), "fresh pages were refetched: {fresh:?}");

        // Everything is older than this cutoff, so both pages are fetched
        // again: the cutoff has the same meaning as in a --seeds run.
        options.since = Some(
            (chrono::Utc::now() + chrono::Duration::seconds(3_600))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let before = server.paths().len();
        crawl_plan(&options, server.client()).unwrap();
        let stale: Vec<String> = server.paths()[before..]
            .iter()
            .filter(|path| path.as_str() == "/a" || path.as_str() == "/b")
            .cloned()
            .collect();
        assert_eq!(stale, ["/a", "/b"]);
    }

    #[test]
    fn plan_mode_rejects_ambiguous_flag_combinations() {
        let directory = tempfile::tempdir().unwrap();
        let client = mock_client("127.0.0.1:9".parse().unwrap(), "");
        let mut options = Options::parse(&[]);
        options.index = directory.path().join("index");
        options.due = true;
        let error = crawl_with_client(&options, client.clone()).unwrap_err();
        assert!(error.contains("--due needs --plan"), "{error}");

        options.due = false;
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, "https://example.test/").unwrap();
        options.seeds = Some(seeds);
        let plan = write_plan(
            &directory,
            "[[site]]\nseeds = [\"https://example.test/\"]\ninterval = \"1h\"\n",
        );
        options.plan = Some(plan);
        let error = crawl_plan(&options, client.clone()).unwrap_err();
        assert!(error.contains("do not pass --seeds"), "{error}");

        options.seeds = None;
        // A plan that cannot be parsed fails before any site runs.
        let broken = write_plan(
            &directory,
            "[[site]]\nseeds = [\"https://example.test/\"]\ninterval = \"soon\"\n",
        );
        options.plan = Some(broken);
        let error = crawl_plan(&options, client).unwrap_err();
        assert!(error.contains("interval"), "{error}");
    }

    #[test]
    fn a_plan_refuses_an_index_another_crawler_is_writing() {
        let directory = tempfile::tempdir().unwrap();
        let plan = write_plan(
            &directory,
            "[[site]]\nname = \"example\"\nseeds = [\"https://example.test/\"]\ninterval = \"1h\"\n",
        );
        let options = plan_options(&directory, &plan);
        let held = LocalIndex::open_for_write(&options.index).unwrap();
        let error =
            crawl_plan(&options, mock_client("127.0.0.1:9".parse().unwrap(), "")).unwrap_err();
        assert!(error.contains("writer"), "{error}");
        // The lock conflict is not the site's fault: it must not be recorded
        // as a failure that pushes the next run out by an interval.
        assert!(
            Schedule::load(&options.index)
                .unwrap()
                .get("example")
                .is_none()
        );
        drop(held);
    }

    #[test]
    fn schedule_lines_report_due_and_waiting_sites() {
        let mut schedule = Schedule::default();
        schedule.record_success("fresh.example", 3_600, 1_000, 1_000);
        schedule.record_failure("broken.example", 600, 1_000, 1_000);
        let lines = schedule_lines(&schedule, 2_000);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("broken.example: interval 600s"),
            "{lines:?}"
        );
        assert!(lines[0].contains("failures 1"), "{lines:?}");
        assert!(lines[0].contains("due now"), "{lines:?}");
        assert!(
            lines[1].starts_with("fresh.example: interval 3600s"),
            "{lines:?}"
        );
        assert!(lines[1].contains("waiting"), "{lines:?}");
    }

    #[test]
    fn literal_private_seeds_never_open_a_connection() {
        let server = TestServer::new(|_, path, _| panic!("unexpected request: {path}"));
        let directory = tempfile::tempdir().unwrap();
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, format!("http://{}/", server.address)).unwrap();
        let mut options = Options::parse(&[]);
        options.seeds = Some(seeds);
        options.index = directory.path().join("index");
        let error = crawl_with_client(&options, server.client()).unwrap_err();
        assert!(error.contains("not a public HTTP(S) URL"), "{error}");
        assert!(server.paths().is_empty());
    }

    #[test]
    fn recrawling_a_redirect_removes_its_old_index_entry() {
        let moved = Arc::new(AtomicBool::new(false));
        let current = Arc::clone(&moved);
        let server = TestServer::new(move |_, path, _| match path {
            "/robots.txt" => reply(200, "", ""),
            "/old" if current.load(Ordering::SeqCst) => reply(302, "Location: /new\r\n", ""),
            "/old" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>Old</title><p>oldvalue</p>",
            ),
            "/new" => reply(
                200,
                "Content-Type: text/html\r\n",
                "<title>New</title><p>newvalue</p>",
            ),
            _ => panic!("unexpected request: {path}"),
        });
        let directory = tempfile::tempdir().unwrap();
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, server.url("/old").as_str()).unwrap();
        let mut options = Options::parse(&[]);
        options.seeds = Some(seeds);
        options.index = directory.path().join("index");
        options.delay_ms = 0;
        options.workers = 1;
        crawl_with_client(&options, server.client()).unwrap();
        moved.store(true, Ordering::SeqCst);
        crawl_with_client(&options, server.client()).unwrap();
        let index = LocalIndex::open(&options.index).unwrap();
        assert_eq!(index.document_count(), 1);
        assert!(index.search("oldvalue", 5).unwrap().is_empty());
        assert_eq!(
            index.search("newvalue", 5).unwrap()[0].url,
            server.url("/new").as_str()
        );
    }

    #[test]
    fn crawl_follows_links_from_new_and_indexed_pages() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://example.test:{}/", address.port());
        let directory = tempfile::tempdir().unwrap();
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, &url).unwrap();
        let mut cases = Vec::new();
        for name in ["fresh", "resumed"] {
            let mut options = Options::parse(&[]);
            options.seeds = Some(seeds.clone());
            options.index = directory.path().join(name);
            options.workers = 1;
            options.delay_ms = 0;
            options.max_pages = 3;
            options.max_depth = 2;
            if name == "resumed" {
                let index = LocalIndex::open_for_write(&options.index).unwrap();
                index
                    .add(&Page {
                        url: &url,
                        title: "Root",
                        body: "Child",
                        lang: "en",
                        snippet: "",
                    })
                    .unwrap();
                index.commit().unwrap();
            }
            cases.push(options);
        }

        let server = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap() == 0 {
                    break;
                }
                for header in reader.lines() {
                    if header.unwrap().is_empty() {
                        break;
                    }
                }
                let body = match request.split_whitespace().nth(1).unwrap() {
                    "/" => "<title>Root</title><a href='/child'>Child</a>",
                    "/child" => "<title>Child</title><a href='/grandchild'>Grandchild</a>",
                    "/grandchild" => "<title>Grandchild</title><p>The last page</p>",
                    _ => "",
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let results: Vec<_> = cases
            .iter()
            .map(|options| {
                crawl_with_client(options, mock_client(address, &options.contact))?;
                LocalIndex::open(&options.index).map(|index| index.document_count())
            })
            .collect();
        drop(TcpStream::connect(address).unwrap());
        server.join().unwrap();
        assert_eq!(results, vec![Ok(3), Ok(3)]);
    }

    #[test]
    fn crawl_obeys_robots_noindex_nofollow_and_announces_itself() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let seeds = directory.path().join("seeds.txt");
        std::fs::write(&seeds, format!("http://example.test:{}/", address.port())).unwrap();
        let mut options = Options::parse(&[]);
        options.seeds = Some(seeds);
        options.index = directory.path().join("index");
        options.workers = 1;
        options.delay_ms = 0;
        options.max_pages = 20;
        options.max_depth = 3;
        options.contact = "ops@example.org".into();

        let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let log = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap() == 0 {
                    break;
                }
                let path = request.split_whitespace().nth(1).unwrap().to_string();
                let mut agent = String::new();
                for header in reader.lines() {
                    let header = header.unwrap();
                    if header.is_empty() {
                        break;
                    }
                    if let Some(value) = header.to_lowercase().strip_prefix("user-agent:") {
                        agent = value.trim().to_string();
                    }
                }
                log.lock().unwrap().push((path.clone(), agent));
                let (kind, extra, body) = match path.as_str() {
                    "/robots.txt" => (
                        "text/plain",
                        "",
                        "User-agent: *\nDisallow: /private\n".to_string(),
                    ),
                    "/" => (
                        "text/html",
                        "",
                        "<title>Root</title><a href='/ok'>ok</a><a href='/private/secret'>p</a>\
                         <a href='/hidden'>h</a><a rel='nofollow' href='/nofollowed'>n</a>\
                         <a href='/header-noindex'>x</a>"
                            .to_string(),
                    ),
                    "/ok" => (
                        "text/html",
                        "",
                        "<title>Fine</title><p>fine page</p>".to_string(),
                    ),
                    "/hidden" => (
                        "text/html",
                        "",
                        "<meta name='robots' content='noindex'><title>Hidden</title>\
                         <a href='/after-hidden'>a</a>"
                            .to_string(),
                    ),
                    "/after-hidden" => (
                        "text/html",
                        "",
                        "<title>After</title><p>reachable through a noindex page</p>".to_string(),
                    ),
                    "/header-noindex" => (
                        "text/html",
                        "X-Robots-Tag: noindex\r\n",
                        "<title>Header</title><p>never indexed</p>".to_string(),
                    ),
                    _ => ("text/html", "", String::new()),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });

        crawl_with_client(&options, mock_client(address, &options.contact)).unwrap();
        drop(TcpStream::connect(address).unwrap());
        server.join().unwrap();

        let index = LocalIndex::open(&options.index).unwrap();
        let mut found: Vec<String> = ["Root", "Fine", "After", "Hidden", "Header", "never"]
            .iter()
            .flat_map(|word| index.search(word, 5).unwrap())
            .map(|hit| hit.title)
            .collect();
        found.sort();
        found.dedup();
        assert_eq!(found, vec!["After", "Fine", "Root"], "indexed titles");

        let requests = requests.lock().unwrap();
        let paths: Vec<&str> = requests.iter().map(|(path, _)| path.as_str()).collect();
        assert!(
            !paths.contains(&"/private/secret"),
            "robots.txt ignored: {paths:?}"
        );
        assert!(
            !paths.contains(&"/nofollowed"),
            "rel=nofollow ignored: {paths:?}"
        );
        assert!(
            paths.contains(&"/after-hidden"),
            "links of a noindex page are followed"
        );
        for (path, agent) in requests.iter() {
            assert!(
                agent.starts_with("rustxng-index/") && agent.contains("ops@example.org"),
                "{path} was fetched as {agent:?}"
            );
        }
    }

    #[test]
    fn a_ready_host_is_served_before_a_cooling_crowded_one() {
        let mut frontier = Frontier::default();
        for page in 0..5 {
            frontier.push(format!("https://busy.example/{page}"), 0);
        }
        frontier.push("https://quiet.example/".to_string(), 0);
        assert_eq!(frontier.len, 6);
        let mut slots = HashMap::new();
        slots.insert(
            "busy.example".to_string(),
            Instant::now() + Duration::from_secs(60),
        );
        let (url, _) = frontier.pop(&slots).unwrap();
        assert_eq!(url, "https://quiet.example/");
        // Only the cooling host is left: it is still served, earliest first.
        let (url, _) = frontier.pop(&slots).unwrap();
        assert_eq!(url, "https://busy.example/0");
        assert_eq!(frontier.len, 4);
    }

    #[test]
    fn ready_hosts_take_turns() {
        let mut frontier = Frontier::default();
        for page in 0..2 {
            frontier.push(format!("https://a.example/{page}"), 0);
            frontier.push(format!("https://b.example/{page}"), 0);
        }
        let slots = HashMap::new();
        let order: Vec<String> = (0..4).map(|_| frontier.pop(&slots).unwrap().0).collect();
        assert_eq!(
            order,
            [
                "https://a.example/0",
                "https://b.example/0",
                "https://a.example/1",
                "https://b.example/1"
            ]
        );
        assert!(frontier.pop(&slots).is_none());
        assert_eq!(frontier.len, 0);
    }
}
