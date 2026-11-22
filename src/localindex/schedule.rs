//! Crawl schedule metadata kept next to a local index.
//!
//! The index records which pages were crawled; this module records when each
//! site is crawled next. The crawler is a batch command, so the state only has
//! to survive between invocations: an external scheduler starts the command,
//! and the state it finds decides which sites are due.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The schedule file lives inside the index directory.
pub const SCHEDULE_FILE: &str = "crawl-schedule.json";

/// Version of the file format. A file with another version is rejected
/// instead of being read as if its fields meant the same thing.
pub const SCHEDULE_VERSION: u32 = 1;

/// Longest a site waits between attempts after repeated failures.
pub const MAX_BACKOFF_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Read an interval such as 30s, 15m, 6h, 7d or 1w.
///
/// A bare number is seconds. Zero is rejected: an interval of zero would make
/// every invocation crawl the site again.
pub fn parse_interval(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("interval is empty".to_string());
    }
    let (digits, unit) = match text.char_indices().last() {
        Some((index, ch)) if ch.is_ascii_alphabetic() => (&text[..index], Some(ch)),
        _ => (text, None),
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("interval is not a whole number of seconds: {text:?}"))?;
    let multiplier = match unit {
        None | Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3_600,
        Some('d') => 86_400,
        Some('w') => 604_800,
        Some(other) => {
            return Err(format!(
                "interval unit {other:?} is not one of s, m, h, d, w: {text:?}"
            ));
        }
    };
    let seconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("interval is too long: {text:?}"))?;
    if seconds == 0 {
        return Err("interval must be at least one second".to_string());
    }
    Ok(seconds)
}

/// A crawl plan: the sites and intervals declared in the plan file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrawlPlan {
    sites: Vec<PlannedSite>,
}

impl CrawlPlan {
    /// Parse and validate a plan file.
    ///
    /// The declarations are checked here rather than at crawl time so a typo
    /// cannot silently turn into "no site is due": every seed is normalised
    /// and must be a public HTTP(S) URL, the seeds of one site must share a
    /// host, and the names must be unique.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: PlanFile =
            toml::from_str(text).map_err(|error| format!("parse crawl plan: {error}"))?;
        let mut sites = Vec::with_capacity(file.site.len());
        let mut names = BTreeSet::new();
        for (index, entry) in file.site.into_iter().enumerate() {
            let label = format!("plan site #{}", index + 1);
            let interval_seconds =
                parse_interval(&entry.interval).map_err(|error| format!("{label}: {error}"))?;
            let mut raw_seeds = Vec::new();
            if let Some(seed) = entry.seed {
                raw_seeds.push(seed);
            }
            raw_seeds.extend(entry.seeds);
            if raw_seeds.is_empty() {
                return Err(format!("{label}: needs seed or seeds"));
            }
            let mut seeds = Vec::with_capacity(raw_seeds.len());
            for raw in &raw_seeds {
                let Some(url) = crate::crawl::normalize_url(raw) else {
                    return Err(format!("{label}: not an HTTP(S) URL: {raw:?}"));
                };
                let parsed = url::Url::parse(&url)
                    .map_err(|error| format!("{label}: not a URL ({raw:?}): {error}"))?;
                if !crate::fetch::is_fetchable(&parsed) {
                    return Err(format!("{label}: seed is not a public HTTP(S) URL: {url}"));
                }
                seeds.push(url);
            }
            let host = site_of(&seeds[0])
                .ok_or_else(|| format!("{label}: seed has no host: {:?}", seeds[0]))?;
            for seed in &seeds {
                if site_of(seed).as_deref() != Some(host.as_str()) {
                    return Err(format!(
                        "{label}: the seeds of one site must share a host; {seed:?} is not {host:?}"
                    ));
                }
            }
            if entry.max_pages == Some(0) {
                return Err(format!("{label}: max_pages must be positive"));
            }
            for prefix in &entry.scope {
                let parsed = url::Url::parse(prefix)
                    .map_err(|error| format!("{label}: scope is not a URL ({prefix}): {error}"))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(format!("{label}: scope must be an HTTP(S) URL: {prefix}"));
                }
            }
            let name = match entry.name {
                Some(name) if !name.trim().is_empty() => name.trim().to_string(),
                Some(_) => return Err(format!("{label}: name must not be empty")),
                None => seeds[0].clone(),
            };
            if !names.insert(name.clone()) {
                return Err(format!("{label}: duplicate site name {name:?}"));
            }
            sites.push(PlannedSite {
                name,
                seeds,
                interval_seconds,
                sitemaps: entry.sitemaps,
                max_pages: entry.max_pages,
                within_seed_path: entry.within_seed_path,
                scope: entry.scope,
            });
        }
        Ok(Self { sites })
    }

    /// The declared sites, in file order.
    pub fn sites(&self) -> &[PlannedSite] {
        &self.sites
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }
}

/// One site of a plan, normalised and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSite {
    /// Stable key of the schedule state.
    pub name: String,
    /// Seed URLs, normalised.
    pub seeds: Vec<String>,
    /// How often the site is crawled, in seconds.
    pub interval_seconds: u64,
    /// Overrides the command line when set.
    pub sitemaps: Option<bool>,
    pub max_pages: Option<usize>,
    pub within_seed_path: Option<bool>,
    /// URL prefixes the deletion pass covers instead of the seeds.
    pub scope: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFile {
    #[serde(default)]
    site: Vec<SiteEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SiteEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    seed: Option<String>,
    #[serde(default)]
    seeds: Vec<String>,
    interval: String,
    #[serde(default)]
    sitemaps: Option<bool>,
    #[serde(default)]
    max_pages: Option<usize>,
    #[serde(default)]
    within_seed_path: Option<bool>,
    #[serde(default)]
    scope: Vec<String>,
}

/// The host and port of a URL, with www. folded away, so one plan entry
/// cannot span two sites.
fn site_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(host.as_str());
    Some(match parsed.port() {
        Some(port) => format!("{bare}:{port}"),
        None => bare.to_string(),
    })
}

/// What the scheduler remembers about one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteState {
    /// The interval the plan asked for, in seconds.
    pub interval_seconds: u64,
    /// When the last attempt started, in seconds since the Unix epoch.
    #[serde(default)]
    pub last_attempt: Option<u64>,
    /// When the last attempt finished successfully.
    #[serde(default)]
    pub last_success: Option<u64>,
    /// Failed attempts in a row; a success resets it.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// When the next attempt becomes due, in seconds since the Unix epoch.
    pub next_due: u64,
}

impl SiteState {
    fn new(interval_seconds: u64, now: u64) -> Self {
        Self {
            interval_seconds: interval_seconds.max(1),
            last_attempt: None,
            last_success: None,
            consecutive_failures: 0,
            next_due: now,
        }
    }
}

/// Crawl schedule stored inside an index directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    version: u32,
    #[serde(default)]
    sites: BTreeMap<String, SiteState>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            version: SCHEDULE_VERSION,
            sites: BTreeMap::new(),
        }
    }
}

impl Schedule {
    /// Read the schedule of an index.
    ///
    /// A missing file is an empty schedule (the first run has nothing to
    /// remember). A broken file or a file of another format version is an
    /// error: silently resetting it would forget the backoff of every site.
    pub fn load(index_dir: &Path) -> Result<Self, String> {
        let path = index_dir.join(SCHEDULE_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("read {path:?}: {error}")),
        };
        let schedule: Self =
            serde_json::from_str(&text).map_err(|error| format!("parse {path:?}: {error}"))?;
        if schedule.version != SCHEDULE_VERSION {
            return Err(format!(
                "{path:?} uses schedule format {}, this build reads {SCHEDULE_VERSION}",
                schedule.version
            ));
        }
        Ok(schedule)
    }

    /// Write the schedule, replacing the old file atomically.
    pub fn save(&self, index_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(index_dir)
            .map_err(|error| format!("create {index_dir:?}: {error}"))?;
        let path = index_dir.join(SCHEDULE_FILE);
        let temporary = index_dir.join(format!("{SCHEDULE_FILE}.tmp"));
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("serialise schedule: {error}"))?;
        text.push('\n');
        std::fs::write(&temporary, text)
            .map_err(|error| format!("write {temporary:?}: {error}"))?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("replace {path:?}: {error}")
        })
    }

    /// Every site the schedule knows, by name.
    pub fn sites(&self) -> impl Iterator<Item = (&str, &SiteState)> {
        self.sites
            .iter()
            .map(|(name, state)| (name.as_str(), state))
    }

    /// The remembered state of one site.
    pub fn get(&self, name: &str) -> Option<&SiteState> {
        self.sites.get(name)
    }

    /// Register a site's interval, creating it due now the first time.
    ///
    /// A changed interval reschedules the next attempt from the last attempt,
    /// so shortening an interval takes effect on the next invocation instead
    /// of after the old, longer one.
    pub fn plan_site(&mut self, name: &str, interval_seconds: u64, now: u64) -> &mut SiteState {
        let interval_seconds = interval_seconds.max(1);
        let state = self
            .sites
            .entry(name.to_string())
            .or_insert_with(|| SiteState::new(interval_seconds, now));
        if state.interval_seconds != interval_seconds {
            state.interval_seconds = interval_seconds;
            let base = state.last_success.or(state.last_attempt).unwrap_or(now);
            state.next_due = base.saturating_add(interval_seconds);
        }
        state
    }

    /// Whether the site may be crawled at now.
    ///
    /// A site the schedule has never seen is due: its first crawl must not
    /// wait a whole interval.
    pub fn is_due(&self, name: &str, now: u64) -> bool {
        self.sites
            .get(name)
            .is_none_or(|state| state.next_due <= now)
    }

    /// Record a completed attempt.
    pub fn record_success(
        &mut self,
        name: &str,
        interval_seconds: u64,
        started: u64,
        finished: u64,
    ) {
        let interval_seconds = interval_seconds.max(1);
        let state = self
            .sites
            .entry(name.to_string())
            .or_insert_with(|| SiteState::new(interval_seconds, finished));
        state.interval_seconds = interval_seconds;
        state.last_attempt = Some(started);
        state.last_success = Some(finished);
        state.consecutive_failures = 0;
        state.next_due = finished.saturating_add(interval_seconds);
    }

    /// Record a failed attempt: the next one waits longer, see
    /// [retry_delay].
    pub fn record_failure(
        &mut self,
        name: &str,
        interval_seconds: u64,
        started: u64,
        finished: u64,
    ) {
        let interval_seconds = interval_seconds.max(1);
        let state = self
            .sites
            .entry(name.to_string())
            .or_insert_with(|| SiteState::new(interval_seconds, finished));
        state.interval_seconds = interval_seconds;
        state.last_attempt = Some(started);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.next_due =
            finished.saturating_add(retry_delay(interval_seconds, state.consecutive_failures));
    }
}

/// How long a site waits after its consecutive_failures-th failed attempt.
///
/// The first retry waits one interval, each further consecutive failure
/// doubles it, and the wait never exceeds [MAX_BACKOFF_SECONDS] (or the
/// interval itself, when that is longer). A success resets the ladder.
pub fn retry_delay(interval_seconds: u64, consecutive_failures: u32) -> u64 {
    let interval = interval_seconds.max(1);
    let doublings = consecutive_failures.saturating_sub(1).min(32);
    let delay = interval.saturating_mul(1u64 << doublings);
    delay.min(interval.max(MAX_BACKOFF_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_accept_units_and_reject_zero() {
        assert_eq!(parse_interval("30s").unwrap(), 30);
        assert_eq!(parse_interval("15m").unwrap(), 900);
        assert_eq!(parse_interval("6h").unwrap(), 21_600);
        assert_eq!(parse_interval("7d").unwrap(), 604_800);
        assert_eq!(parse_interval("1w").unwrap(), 604_800);
        assert_eq!(parse_interval("90").unwrap(), 90);
        assert_eq!(parse_interval(" 2h ").unwrap(), 7_200);
        assert!(parse_interval("").is_err());
        assert!(parse_interval("0").is_err());
        assert!(parse_interval("0h").is_err());
        assert!(parse_interval("5x").is_err());
        assert!(parse_interval("h").is_err());
        assert!(parse_interval("1.5h").is_err());
    }

    #[test]
    fn a_plan_is_validated_before_it_can_skip_every_site() {
        let plan = CrawlPlan::parse(
            r#"
            [[site]]
            seeds = ["https://docs.example/3/", "https://docs.example/3/library/"]
            interval = "24h"
            max_pages = 50

            [[site]]
            name = "python"
            seed = "https://www.python.example/3/"
            interval = "7d"
            sitemaps = true
            within_seed_path = true
            scope = ["https://www.python.example/3/"]
            "#,
        )
        .unwrap();
        assert_eq!(plan.sites().len(), 2);
        let first = &plan.sites()[0];
        assert_eq!(first.name, "https://docs.example/3/");
        assert_eq!(first.interval_seconds, 86_400);
        assert_eq!(first.max_pages, Some(50));
        assert_eq!(first.sitemaps, None);
        assert_eq!(plan.sites()[1].name, "python");
        assert_eq!(plan.sites()[1].scope.len(), 1);

        for (broken, expected) in [
            ("[[site]]\nseeds = []\ninterval = \"1h\"\n", "needs seed"),
            (
                "[[site]]\nseeds = [\"https://a.example/\"]\ninterval = \"never\"\n",
                "interval",
            ),
            (
                "[[site]]\nseeds = [\"ftp://a.example/\"]\ninterval = \"1h\"\n",
                "not an HTTP(S) URL",
            ),
            (
                "[[site]]\nseeds = [\"https://a.example/\", \"https://b.example/\"]\ninterval = \"1h\"\n",
                "share a host",
            ),
            (
                "[[site]]\nseeds = [\"https://a.example/\"]\ninterval = \"1h\"\nunknown = 1\n",
                "unknown",
            ),
            (
                "[[site]]\nseeds = [\"https://a.example/\"]\ninterval = \"1h\"\nmax_pages = 0\n",
                "max_pages",
            ),
        ] {
            let error = CrawlPlan::parse(broken).expect_err("must be rejected");
            assert!(error.contains(expected), "{broken:?}: {error}");
        }

        // The same stable name written twice would make one state override
        // the other.
        let duplicate = "[[site]]\nname = \"a\"\nseeds = [\"https://a.example/\"]\ninterval = \"1h\"\n[[site]]\nname = \"a\"\nseeds = [\"https://a.example/x\"]\ninterval = \"2h\"\n";
        assert!(
            CrawlPlan::parse(duplicate)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn the_first_crawl_is_due_and_a_success_schedules_the_next_one() {
        let mut schedule = Schedule::default();
        assert!(schedule.is_due("docs.example", 100));
        schedule.plan_site("docs.example", 60, 100);
        assert!(schedule.is_due("docs.example", 100));
        schedule.record_success("docs.example", 60, 100, 105);
        let state = schedule.get("docs.example").unwrap();
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_attempt, Some(100));
        assert_eq!(state.last_success, Some(105));
        assert_eq!(state.next_due, 165);
        assert!(!schedule.is_due("docs.example", 164));
        assert!(schedule.is_due("docs.example", 165));
    }

    #[test]
    fn failures_double_the_wait_and_a_success_resets_it() {
        let mut schedule = Schedule::default();
        let mut finished = 1_000;
        let mut waits = Vec::new();
        for _ in 0..3 {
            schedule.record_failure("docs.example", 3_600, finished - 10, finished);
            let state = schedule.get("docs.example").unwrap();
            waits.push(state.next_due - finished);
            finished = state.next_due;
        }
        assert_eq!(waits, [3_600, 7_200, 14_400]);

        schedule.record_success("docs.example", 3_600, finished - 10, finished);
        let state = schedule.get("docs.example").unwrap();
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.next_due, finished + 3_600);
    }

    #[test]
    fn the_backoff_ladder_is_bounded_and_never_shorter_than_the_interval() {
        assert_eq!(retry_delay(3_600, 1), 3_600);
        assert_eq!(retry_delay(3_600, 2), 7_200);
        assert_eq!(retry_delay(3_600, 3), 14_400);
        assert_eq!(retry_delay(3_600, 40), MAX_BACKOFF_SECONDS);
        let weekly = MAX_BACKOFF_SECONDS;
        assert_eq!(retry_delay(weekly, 2), weekly);
        assert_eq!(retry_delay(weekly, 9), weekly);
        assert_eq!(retry_delay(1, 100), MAX_BACKOFF_SECONDS);
    }

    #[test]
    fn a_changed_interval_reschedules_from_the_last_attempt() {
        let mut schedule = Schedule::default();
        schedule.record_success("docs.example", 60, 100, 101);
        assert_eq!(schedule.get("docs.example").unwrap().next_due, 161);
        // Longer: the next attempt moves out with the new interval.
        schedule.plan_site("docs.example", 600, 130);
        assert_eq!(schedule.get("docs.example").unwrap().next_due, 701);
        // Shorter: the site is due as soon as the new interval has elapsed
        // since the last attempt, not after the old one.
        schedule.plan_site("docs.example", 30, 130);
        assert_eq!(schedule.get("docs.example").unwrap().next_due, 131);
        assert!(schedule.is_due("docs.example", 131));
    }

    #[test]
    fn the_schedule_survives_a_round_trip_and_a_broken_file_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let mut schedule = Schedule::default();
        schedule.record_success("docs.example", 3_600, 100, 120);
        schedule.record_failure("broken.example", 600, 100, 130);
        schedule.save(directory.path()).unwrap();

        assert_eq!(Schedule::load(directory.path()).unwrap(), schedule);
        assert!(directory.path().join(SCHEDULE_FILE).is_file());
        assert!(
            !directory
                .path()
                .join(format!("{SCHEDULE_FILE}.tmp"))
                .exists()
        );

        std::fs::write(directory.path().join(SCHEDULE_FILE), "{ not json").unwrap();
        assert!(Schedule::load(directory.path()).is_err());
        std::fs::write(
            directory.path().join(SCHEDULE_FILE),
            r#"{"version": 99, "sites": {}}"#,
        )
        .unwrap();
        let error = Schedule::load(directory.path()).unwrap_err();
        assert!(error.contains("format 99"), "{error}");
    }

    #[test]
    fn a_schedule_file_does_not_stop_the_index_from_being_created() {
        // A failed first crawl can leave the schedule file in a directory that
        // has no index yet. Creating the index in that directory must still
        // work, or the site could never recover.
        let directory = tempfile::tempdir().unwrap();
        Schedule::default().save(directory.path()).unwrap();
        let index = crate::localindex::LocalIndex::open_for_write(directory.path()).unwrap();
        index.commit().unwrap();
        assert!(Schedule::load(directory.path()).is_ok());
    }

    #[test]
    fn a_missing_schedule_file_is_empty_not_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let schedule = Schedule::load(directory.path()).unwrap();
        assert_eq!(schedule, Schedule::default());
        assert_eq!(schedule.sites().count(), 0);
        // Saving into a directory that does not exist yet creates it, so the
        // first scheduled run can record its outcome.
        let missing = directory.path().join("index-data");
        schedule.save(&missing).unwrap();
        assert!(missing.join(SCHEDULE_FILE).is_file());
    }
}
