//! Offline routing comparison harness (Phase 4.2 experiment).
//!
//! Compares candidate engine-routing strategies against the two shipping ones in
//! src/aggregator.rs (SearchStrategy::Parallel and run_staged).
//!
//! This is a simulation. Providers are synthetic and described by a latency /
//! failure / duplication / quality profile; the harness performs no network I/O.
//! The numbers are therefore comparable relative to each other only: they say
//! nothing about how any real engine behaves.
//!
//! Taken from production code, not re-implemented:
//!
//! * result fusion and re-ranking: ResultContainer::extend + ResultContainer::close
//! * query term extraction: results::query_terms
//! * failure taxonomy and error_type(): rustxng::engine::FailureKind
//! * every routing constant: Search::default() (staged_tier, staged_min_results,
//!   timeout_limit, ban_time_on_fail, max_ban_time_on_fail, empty_result_limit,
//!   empty_result_cooldown, suspended_times, relevance_weight, authority_weight)
//!
//! Re-implemented on purpose, because it needs a virtual clock:
//! src/engine/state.rs suspension / resting bookkeeping (same fields, same
//! thresholds, seconds mapped onto virtual milliseconds).
//!
//! Strategies under test:
//!
//! * current-parallel   - shipping default: every selected engine at once
//! * current-staged     - shipping SearchStrategy::Staged with its defaults
//! * staged-tuned       - the same loop with tier = 3 (sensitivity check)
//! * capability-aware   - select engines whose capability fits the query kind
//! * category-aware     - select engines by the category the query kind maps to
//! * reliability-aware  - honour resting/suspended state, drop error-prone engines
//! * progressive        - wave 1 of 3 engines, widen only if results are thin
//! * early-stop         - start all, cancel the rest once enough results arrived
//!
//! Metrics: Hit@1/5/10, Recall@10, MRR@10, nDCG@10 (binary gains) against a
//! synthetic gold set, plus provider request count, simulated latency, failure
//! tolerance and deadline compliance. Paired sign tests compare every strategy
//! with current-parallel per query.
//!
//! Usage:
//!
//!   cargo run --release --example routing_lab -- --queries 150 --seed 7 \
//!       --out bench/local-retrieval/routing-lab/routing-lab.json

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use rustxng::categories;
use rustxng::engine::{EngineItem, FailureKind};
use rustxng::results::{ResultContainer, SearchResult, query_terms};
use rustxng::settings::Search;

/// Results cut for the metric columns. nDCG/MRR use this depth.
const TOP_K: usize = 10;
/// Gold-precision factor an engine keeps when it does not have the query's
/// capability. Sensitivity parameter: the report runs 0.35 (default) and 0.70.
const DEFAULT_OFFCAP_FIT: f64 = 0.35;
/// Virtual time between two query starts. Fixed, so the engine-state timeline
/// (suspensions, resting) is identical for every strategy: otherwise a fast
/// strategy would see fewer expired suspensions and the request counts would
/// not be comparable.
const QUERY_INTERVAL_MS: f64 = 4_000.0;

// --------------------------------------------------------------------- PRNG

/// SplitMix64. Deterministic and dependency free so runs reproduce exactly.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5_5A5A_C3C3_3C3C,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Mix three coordinates into one seed. Provider behaviour must depend only on
/// (query, provider, request ordinal), never on the order the strategies happen
/// to issue requests: otherwise two strategies would face different worlds and
/// every comparison would be noise.
fn mix(query: u64, provider: u64, ordinal: u64) -> u64 {
    let mut value = query
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(provider.wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add(ordinal.wrapping_mul(0x1656_67B1_9E37_79F9));
    value ^= value >> 29;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 32)
}

// -------------------------------------------------------------- query kinds

/// What an engine is good at. A routing capability, not a SearXNG category.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cap {
    Web,
    Code,
    Docs,
    News,
    Science,
    Qa,
}

/// The kind of question a query asks. Drives capability and category routing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Web,
    Code,
    Docs,
    News,
    Science,
    Qa,
}

impl Kind {
    const ALL: [Self; 6] = [
        Self::Web,
        Self::Code,
        Self::Docs,
        Self::News,
        Self::Science,
        Self::Qa,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Code => "code",
            Self::Docs => "docs",
            Self::News => "news",
            Self::Science => "science",
            Self::Qa => "qa",
        }
    }

    fn cap(self) -> Cap {
        match self {
            Self::Web => Cap::Web,
            Self::Code => Cap::Code,
            Self::Docs => Cap::Docs,
            Self::News => Cap::News,
            Self::Science => Cap::Science,
            Self::Qa => Cap::Qa,
        }
    }

    /// The categories Aggregator::select_engines would ask for this kind.
    fn categories(self) -> &'static [&'static str] {
        match self {
            Self::Web => &[categories::GENERAL],
            Self::Code => &[
                categories::IT,
                categories::REPOSITORIES,
                categories::PACKAGES,
                categories::CODE,
            ],
            Self::Docs => &[categories::GENERAL, categories::IT, categories::BLOGS],
            Self::News => &[categories::NEWS],
            Self::Science => &[categories::SCIENCE, categories::SCIENTIFIC_PUBLICATIONS],
            Self::Qa => &[categories::QA],
        }
    }

    fn vocab(self) -> &'static [&'static str] {
        match self {
            Self::Web => &["weather", "city", "guide", "timetable", "museum", "trail"],
            Self::Code => &["borrow", "async", "macro", "trait", "runtime", "iterator"],
            Self::Docs => &[
                "reference",
                "configuration",
                "migration",
                "timeout",
                "example",
            ],
            Self::News => &["election", "budget", "recall", "tariff", "storm", "merger"],
            Self::Science => &[
                "protein", "lattice", "orbital", "cohort", "entropy", "crystal",
            ],
            Self::Qa => &["howto", "why", "best", "difference", "fix", "setup"],
        }
    }

    fn gold_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Web => &["example.org", "civic-guide.example", "trail-notes.example"],
            Self::Code => &["docs.rs", "crates.example", "code-notes.example"],
            Self::Docs => &["docs.example", "manual.example", "handbook.example"],
            Self::News => &["wire.example", "daily.example", "press.example"],
            Self::Science => &["arxiv.example", "journal.example", "preprints.example"],
            Self::Qa => &["answers.example", "forum.example", "stack.example"],
        }
    }
}

// ------------------------------------------------------------ provider model

/// One synthetic provider. The names are deliberately neutral: these are lab
/// fixtures, not measurements of any real engine.
#[derive(Clone)]
struct Provider {
    name: &'static str,
    caps: &'static [Cap],
    categories: &'static [&'static str],
    /// Registry weight, fed to the production fusion as the extend() weight.
    weight: f64,
    base_ms: f64,
    jitter_ms: f64,
    tail_prob: f64,
    tail_ms: f64,
    /// Probability that a request fails outright.
    fail_prob: f64,
    /// Probability that one gold document appears in this provider's answer when
    /// its capability fits. Off-capability engines keep 35% of it.
    precision: f64,
    /// Results per answer.
    depth: usize,
    /// Probability that a non-gold slot comes from the shared commons pool (the
    /// same pages many providers return).
    dup_rate: f64,
    /// Probability that a gold document is placed in the top three.
    rank_bias: f64,
    /// Requests allowed per run before the provider answers 429. 0 = unlimited.
    rate_budget: u32,
}

impl Provider {
    fn fits(&self, kind: Kind) -> bool {
        self.caps.contains(&kind.cap())
    }
}

/// Latency / failure / quality profile of one synthetic provider.
struct Profile {
    base_ms: f64,
    jitter_ms: f64,
    tail_prob: f64,
    tail_ms: f64,
    fail_prob: f64,
    precision: f64,
    depth: usize,
    dup_rate: f64,
    rank_bias: f64,
}

impl Profile {
    #[allow(clippy::too_many_arguments)]
    const fn new(
        base_ms: f64,
        jitter_ms: f64,
        tail_prob: f64,
        tail_ms: f64,
        fail_prob: f64,
        precision: f64,
        depth: usize,
        dup_rate: f64,
        rank_bias: f64,
    ) -> Self {
        Self {
            base_ms,
            jitter_ms,
            tail_prob,
            tail_ms,
            fail_prob,
            precision,
            depth,
            dup_rate,
            rank_bias,
        }
    }
}

/// The synthetic provider set: 14 engines, the size of a default general search.
fn providers() -> Vec<Provider> {
    let table: [(&'static str, &'static [Cap], &'static [&'static str], f64); 14] = [
        ("generic-a", &[Cap::Web], &[categories::GENERAL], 1.0),
        ("generic-b", &[Cap::Web], &[categories::GENERAL], 1.0),
        ("generic-c", &[Cap::Web], &[categories::GENERAL], 1.0),
        (
            "code-a",
            &[Cap::Code, Cap::Docs],
            &[categories::IT, categories::REPOSITORIES],
            1.2,
        ),
        (
            "code-b",
            &[Cap::Code],
            &[categories::IT, categories::CODE],
            1.1,
        ),
        (
            "docs-a",
            &[Cap::Docs, Cap::Web],
            &[categories::GENERAL, categories::IT],
            1.1,
        ),
        ("news-a", &[Cap::News], &[categories::NEWS], 1.0),
        ("news-b", &[Cap::News], &[categories::NEWS], 0.9),
        (
            "sci-a",
            &[Cap::Science],
            &[categories::SCIENCE, categories::SCIENTIFIC_PUBLICATIONS],
            1.2,
        ),
        (
            "sci-b",
            &[Cap::Science, Cap::Web],
            &[categories::SCIENCE],
            1.0,
        ),
        ("qa-a", &[Cap::Qa], &[categories::QA], 1.0),
        (
            "blog-a",
            &[Cap::Web, Cap::Docs],
            &[categories::GENERAL, categories::BLOGS],
            0.9,
        ),
        ("generic-d", &[Cap::Web], &[categories::GENERAL], 0.8),
        (
            "sci-c",
            &[Cap::Science, Cap::Code],
            &[categories::SCIENCE, categories::IT],
            0.9,
        ),
    ];
    // Latency / failure / quality profiles, index aligned with the table above.
    // The failure rate is deliberately low: one hard failure suspends an engine
    // for minutes (Search::default().suspended_times), so a high rate would turn
    // every scenario into "everything is suspended" within a few queries.
    let profiles = [
        Profile::new(420.0, 180.0, 0.06, 900.0, 0.010, 0.55, 12, 0.25, 0.50),
        Profile::new(500.0, 220.0, 0.05, 800.0, 0.015, 0.50, 12, 0.30, 0.45),
        Profile::new(380.0, 150.0, 0.04, 700.0, 0.010, 0.45, 10, 0.20, 0.40),
        Profile::new(350.0, 120.0, 0.05, 600.0, 0.010, 0.62, 10, 0.15, 0.55),
        Profile::new(480.0, 200.0, 0.06, 800.0, 0.012, 0.55, 12, 0.20, 0.50),
        Profile::new(600.0, 250.0, 0.07, 1_200.0, 0.015, 0.50, 12, 0.25, 0.45),
        Profile::new(300.0, 120.0, 0.05, 600.0, 0.012, 0.65, 10, 0.10, 0.55),
        Profile::new(520.0, 200.0, 0.08, 1_100.0, 0.020, 0.50, 10, 0.15, 0.45),
        Profile::new(700.0, 300.0, 0.09, 1_500.0, 0.014, 0.70, 8, 0.10, 0.60),
        Profile::new(900.0, 400.0, 0.10, 1_800.0, 0.018, 0.50, 10, 0.20, 0.45),
        Profile::new(450.0, 200.0, 0.06, 900.0, 0.016, 0.60, 10, 0.20, 0.55),
        Profile::new(620.0, 260.0, 0.07, 1_100.0, 0.018, 0.45, 12, 0.30, 0.40),
        Profile::new(700.0, 300.0, 0.08, 1_300.0, 0.022, 0.40, 10, 0.35, 0.35),
        Profile::new(800.0, 350.0, 0.09, 1_600.0, 0.020, 0.45, 8, 0.20, 0.40),
    ];

    table
        .into_iter()
        .zip(profiles)
        .map(|((name, caps, categories, weight), profile)| Provider {
            name,
            caps,
            categories,
            weight,
            base_ms: profile.base_ms,
            jitter_ms: profile.jitter_ms,
            tail_prob: profile.tail_prob,
            tail_ms: profile.tail_ms,
            fail_prob: profile.fail_prob,
            precision: profile.precision,
            depth: profile.depth,
            dup_rate: profile.dup_rate,
            rank_bias: profile.rank_bias,
            rate_budget: 0,
        })
        .collect()
}

#[derive(Clone)]
struct Doc {
    url: String,
    title: String,
    content: String,
}

/// One synthetic query with its document universe.
struct QueryCase {
    index: usize,
    kind: Kind,
    text: String,
    gold: Vec<Doc>,
    /// Pages many providers return; the duplication source.
    commons: Vec<Doc>,
    /// Filler pages, sliced per provider so answers are not identical.
    noise: Vec<Doc>,
}

fn make_query(rng: &mut Rng, index: usize) -> QueryCase {
    let kind = Kind::ALL[index % Kind::ALL.len()];
    let vocab = kind.vocab();
    let slug = |terms: &[String]| terms.join("-");

    let mut terms: Vec<String> = Vec::new();
    while terms.len() < 3 {
        let word = (*rng.pick(vocab)).to_string();
        if !terms.contains(&word) {
            terms.push(word);
        }
    }
    let text = terms.join(" ");

    let gold_count = 1 + rng.below(3);
    let mut gold = Vec::with_capacity(gold_count);
    for slot in 0..gold_count {
        let host = *rng.pick(kind.gold_hosts());
        gold.push(Doc {
            url: format!(
                "https://{host}/{}/{}_gold_{slot}_{index}",
                kind.as_str(),
                slug(&terms)
            ),
            // Gold pages cover every query term, which is what the relevance
            // blend in ResultContainer::close rewards.
            title: format!("{text} reference {slot}"),
            content: format!("{text} - {} page covering all terms", kind.as_str()),
        });
    }

    let mut commons = Vec::with_capacity(8);
    for slot in 0..8 {
        commons.push(Doc {
            url: format!(
                "https://commons.example/{}/item_{slot}_{index}",
                kind.as_str()
            ),
            // One term at most: plausible, but weaker than gold for both the
            // relevance blend and position fusion.
            title: format!("{} digest {slot}", terms[0]),
            content: format!("roundup of {} material", terms[0]),
        });
    }

    let mut noise = Vec::with_capacity(40);
    for slot in 0..40 {
        let title = if rng.chance(0.25) {
            format!("{} notes {slot}", *rng.pick(vocab))
        } else {
            format!("unrelated topic {slot}")
        };
        noise.push(Doc {
            url: format!(
                "https://filler{}.example/{}/p_{slot}_{index}",
                slot % 7,
                kind.as_str()
            ),
            title,
            content: format!("page {slot} on {}", kind.as_str()),
        });
    }

    QueryCase {
        index,
        kind,
        text,
        gold,
        commons,
        noise,
    }
}

// ------------------------------------------------- virtual engine state machine

/// EngineStats of src/engine/state.rs on a virtual clock.
#[derive(Clone, Default)]
struct VirtualStats {
    requests: u64,
    /// Answers that carried at least one result.
    results: u64,
    errors: u64,
    total_time_ms: f64,
}

#[derive(Clone, Default)]
struct VirtualState {
    consecutive_failures: u64,
    empty_streak: u64,
    quiet_until: Option<f64>,
    suspended_until: Option<f64>,
    suspension_reason: Option<String>,
    stats: VirtualStats,
}

/// Mirror of EngineStates (src/engine/state.rs) with an injectable clock.
/// Thresholds come from Search::default(); configured seconds are virtual ms.
struct VirtualStates {
    now: f64,
    search: Search,
    states: HashMap<&'static str, VirtualState>,
}

impl VirtualStates {
    fn new(search: Search) -> Self {
        Self {
            now: 0.0,
            search,
            states: HashMap::new(),
        }
    }

    fn set_now(&mut self, now: f64) {
        self.now = now;
    }

    fn state(&self, engine: &str) -> Option<&VirtualState> {
        self.states.get(engine)
    }

    /// EngineStates::suspension (state.rs:87): resting is checked first.
    fn suspension(&mut self, engine: &str) -> Option<(f64, Option<String>)> {
        let now = self.now;
        let state = self.states.get_mut(engine)?;
        if let Some(until) = state.quiet_until {
            if until > now {
                return Some((until - now, Some("resting".to_string())));
            }
            state.quiet_until = None;
        }
        match state.suspended_until {
            Some(until) if until > now => Some((until - now, state.suspension_reason.clone())),
            Some(_) => {
                state.suspended_until = None;
                state.suspension_reason = None;
                None
            }
            None => None,
        }
    }

    /// EngineStates::record_success (state.rs:111).
    fn record_success(&mut self, engine: &'static str, results: usize, elapsed_ms: f64) {
        let empty_limit = self.search.empty_result_limit;
        let cooldown = self.search.empty_result_cooldown.max(1) as f64 * 1000.0;
        let now = self.now;
        let state = self.states.entry(engine).or_default();
        state.consecutive_failures = 0;
        if results == 0 {
            state.empty_streak += 1;
            if empty_limit > 0 && state.empty_streak >= empty_limit {
                state.quiet_until = Some(now + cooldown);
                state.empty_streak = 0;
            }
        } else {
            state.empty_streak = 0;
            state.quiet_until = None;
            state.stats.results += 1;
        }
        state.suspended_until = None;
        state.suspension_reason = None;
        state.stats.requests += 1;
        state.stats.total_time_ms += elapsed_ms;
    }

    /// EngineStates::record_failure (state.rs:143).
    fn record_failure(&mut self, engine: &'static str, kind: FailureKind) {
        let seconds = match kind.suspension_key() {
            Some(key) => self
                .search
                .suspended_times
                .get(key)
                .copied()
                .unwrap_or(3_600),
            None => 0,
        };
        let ban = self.search.ban_time_on_fail;
        let max_ban = self.search.max_ban_time_on_fail;
        let now = self.now;
        let state = self.states.entry(engine).or_default();
        state.stats.errors += 1;
        state.consecutive_failures += 1;
        let seconds = if seconds > 0 {
            seconds
        } else if state.consecutive_failures >= 2 {
            (state.consecutive_failures * ban).min(max_ban)
        } else {
            0
        };
        if seconds > 0 {
            state.suspended_until = Some(now + seconds as f64 * 1000.0);
            state.suspension_reason = Some(kind.error_type().to_string());
        }
    }

    /// Put an engine out of rotation before a run starts.
    fn pre_suspend(&mut self, engine: &'static str, ms: f64, reason: &str) {
        let now = self.now;
        let state = self.states.entry(engine).or_default();
        state.suspended_until = Some(now + ms);
        state.suspension_reason = Some(reason.to_string());
    }

    /// Aggregator::order_engines_by_health (aggregator.rs:1159) as a sort key:
    /// proven engines first (fastest first), untried next, empty, suspended last.
    fn health_key(&self, engine: &str) -> (u8, u64) {
        let Some(state) = self.states.get(engine) else {
            return (1, 0);
        };
        let suspended = state.suspended_until.is_some_and(|until| until > self.now)
            || state.quiet_until.is_some_and(|until| until > self.now);
        let rank = if suspended {
            3
        } else if state.stats.requests == 0 {
            1
        } else if state.stats.results > 0 {
            0
        } else {
            2
        };
        let millis = if state.stats.requests == 0 {
            0
        } else {
            (state.stats.total_time_ms / state.stats.requests as f64) as u64
        };
        (rank, millis)
    }
}

// ------------------------------------------------------------------ simulator

#[derive(Clone)]
enum Resp {
    Ok(Vec<Doc>),
    Fail(FailureKind),
}

struct Job {
    provider: usize,
    seq: usize,
    start: f64,
    end: f64,
    resp: Resp,
}

struct Done {
    provider: usize,
    seq: usize,
    resp: Resp,
}

fn sample_failure(rng: &mut Rng) -> FailureKind {
    match rng.below(100) {
        0..=73 => FailureKind::Http,
        74..=85 => FailureKind::Parse,
        86..=93 => FailureKind::TooManyRequests,
        _ => FailureKind::AccessDenied,
    }
}

/// Discrete-event simulation of one search: open requests, completions, the
/// search deadline and per-run request budgets. Latency and responses depend
/// only on (query, provider, request ordinal), see mix().
struct Sim<'a> {
    query: &'a QueryCase,
    providers: &'a [Provider],
    states: &'a mut VirtualStates,
    budget: &'a mut [u32],
    issued_to: Vec<u32>,
    /// Gold-precision factor of an engine without the query's capability.
    offcap_fit: f64,
    now: f64,
    deadline: f64,
    inflight: Vec<Job>,
    done: Vec<Done>,
    issued: usize,
    suspended_skips: usize,
    seq: usize,
}

impl<'a> Sim<'a> {
    fn new(
        query: &'a QueryCase,
        providers: &'a [Provider],
        states: &'a mut VirtualStates,
        budget: &'a mut [u32],
        deadline: f64,
        offcap_fit: f64,
    ) -> Self {
        let clock = states.now;
        Self {
            query,
            providers,
            states,
            budget,
            issued_to: vec![0; providers.len()],
            offcap_fit,
            now: clock,
            deadline,
            inflight: Vec::new(),
            done: Vec::new(),
            issued: 0,
            suspended_skips: 0,
            seq: 0,
        }
    }

    fn now(&self) -> f64 {
        self.now
    }

    fn deadline(&self) -> f64 {
        self.deadline
    }

    fn collected(&self) -> usize {
        self.done
            .iter()
            .map(|entry| match &entry.resp {
                Resp::Ok(docs) => docs.len(),
                Resp::Fail(_) => 0,
            })
            .sum()
    }

    fn health_key(&self, provider: usize) -> (u8, u64) {
        self.states.health_key(self.providers[provider].name)
    }

    fn order_by_health(&self, selected: &[usize]) -> Vec<usize> {
        let mut ordered = selected.to_vec();
        ordered.sort_by_key(|provider| self.health_key(*provider));
        ordered
    }

    fn start_all(&mut self, selected: &[usize]) {
        for provider in selected {
            self.start(*provider);
        }
    }

    /// Issue one request, or record why it was not issued.
    fn start(&mut self, provider: usize) {
        let spec = self.providers[provider].clone();
        if let Some((_left, reason)) = self.states.suspension(spec.name) {
            // Production parity (aggregator.rs:1021): a suspended engine is
            // skipped without a request and shows up as unresponsive.
            self.suspended_skips += 1;
            let kind = match reason.as_deref() {
                Some("resting") => FailureKind::Resting,
                Some("too many requests") => FailureKind::TooManyRequests,
                Some("access denied") => FailureKind::AccessDenied,
                Some("captcha") => FailureKind::Captcha,
                _ => FailureKind::Other,
            };
            self.push_done(provider, Resp::Fail(kind), self.now, self.now);
            return;
        }
        if self.now >= self.deadline {
            // engine_budget() (aggregator.rs:1563) refuses to start after the cutoff.
            self.push_done(
                provider,
                Resp::Fail(FailureKind::Timeout),
                self.now,
                self.now,
            );
            return;
        }

        let ordinal = u64::from(self.issued_to[provider]);
        self.issued_to[provider] += 1;
        let mut rng = Rng::new(mix(self.query.index as u64, provider as u64, ordinal));
        let latency = spec.base_ms
            + rng.unit() * spec.jitter_ms
            + if rng.chance(spec.tail_prob) {
                spec.tail_ms
            } else {
                0.0
            };
        let resp = self.respond(provider, &spec, &mut rng);
        self.issued += 1;

        let start = self.now;
        if start + latency > self.deadline {
            // The deadline cuts the request short (engine_budget); the partial
            // answer is lost, exactly like a timeout in production.
            self.push_done(
                provider,
                Resp::Fail(FailureKind::Timeout),
                start,
                self.deadline,
            );
            return;
        }
        let seq = self.seq;
        self.seq += 1;
        self.inflight.push(Job {
            provider,
            seq,
            start,
            end: start + latency,
            resp,
        });
    }

    fn respond(&mut self, provider: usize, spec: &Provider, rng: &mut Rng) -> Resp {
        if spec.rate_budget > 0 {
            if self.budget[provider] == 0 {
                return Resp::Fail(FailureKind::TooManyRequests);
            }
            self.budget[provider] -= 1;
        }
        if rng.chance(spec.fail_prob) {
            return Resp::Fail(sample_failure(rng));
        }

        let query = self.query;
        let mut slots: Vec<Option<Doc>> = vec![None; spec.depth];
        let mut free: Vec<usize> = (0..spec.depth).collect();
        let fit = if spec.fits(query.kind) {
            1.0
        } else {
            self.offcap_fit
        };
        for doc in &query.gold {
            if free.is_empty() {
                break;
            }
            if !rng.chance(spec.precision * fit) {
                continue;
            }
            let pick = if rng.chance(spec.rank_bias) {
                rng.below(free.len().clamp(1, 3))
            } else {
                rng.below(free.len())
            };
            let position = free.remove(pick.min(free.len() - 1));
            slots[position] = Some(doc.clone());
        }

        let mut cursor = (provider * 7) % query.noise.len();
        for position in free {
            let doc = if rng.chance(spec.dup_rate) {
                rng.pick(&query.commons).clone()
            } else {
                let doc = query.noise[cursor % query.noise.len()].clone();
                cursor += 1;
                doc
            };
            slots[position] = Some(doc);
        }

        Resp::Ok(slots.into_iter().flatten().collect())
    }

    fn push_done(&mut self, provider: usize, resp: Resp, start: f64, end: f64) {
        let seq = self.seq;
        self.seq += 1;
        let name = self.providers[provider].name;
        match &resp {
            Resp::Ok(docs) => self.states.record_success(name, docs.len(), end - start),
            Resp::Fail(kind) => self.states.record_failure(name, *kind),
        }
        self.done.push(Done {
            provider,
            seq,
            resp,
        });
    }

    /// Advance to the next completion. Returns false when nothing is in flight.
    fn poll(&mut self) -> bool {
        if self.inflight.is_empty() {
            return false;
        }
        let next = self
            .inflight
            .iter()
            .map(|job| job.end)
            .fold(f64::INFINITY, f64::min);
        self.now = self.now.max(next);

        let mut finished = Vec::new();
        let mut running = Vec::with_capacity(self.inflight.len());
        for job in std::mem::take(&mut self.inflight) {
            if job.end <= self.now + 1e-9 {
                finished.push(job);
            } else {
                running.push(job);
            }
        }
        self.inflight = running;
        for job in finished {
            let name = self.providers[job.provider].name;
            match &job.resp {
                Resp::Ok(docs) => self
                    .states
                    .record_success(name, docs.len(), job.end - job.start),
                Resp::Fail(kind) => self.states.record_failure(name, *kind),
            }
            self.done.push(Done {
                provider: job.provider,
                seq: job.seq,
                resp: job.resp,
            });
        }
        true
    }

    fn run_to_idle(&mut self) {
        while !self.inflight.is_empty() {
            self.poll();
        }
    }

    fn finish(mut self) -> RunResult {
        let cancelled = self.inflight.len();
        self.inflight.clear();
        let mut done = self.done;
        done.sort_by_key(|entry| entry.seq);
        let failures = done
            .iter()
            .filter(|entry| matches!(entry.resp, Resp::Fail(_)))
            .count();
        RunResult {
            done,
            latency_ms: self.now,
            issued: self.issued,
            suspended_skips: self.suspended_skips,
            cancelled,
            failures,
            deadline_missed: self.now + 1e-9 >= self.deadline,
        }
    }
}

/// What one (query, strategy) run produced, in merge order.
struct RunResult {
    /// Merge order = request order, the way the aggregator feeds the container.
    done: Vec<Done>,
    latency_ms: f64,
    issued: usize,
    suspended_skips: usize,
    cancelled: usize,
    failures: usize,
    deadline_missed: bool,
}

// ------------------------------------------------------------------ strategies

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Strategy {
    CurrentParallel,
    CurrentStaged,
    StagedTuned,
    Capability,
    Category,
    Reliability,
    Progressive,
    EarlyStop,
}

const STRATEGIES: [Strategy; 8] = [
    Strategy::CurrentParallel,
    Strategy::CurrentStaged,
    Strategy::StagedTuned,
    Strategy::Capability,
    Strategy::Category,
    Strategy::Reliability,
    Strategy::Progressive,
    Strategy::EarlyStop,
];

impl Strategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::CurrentParallel => "current-parallel",
            Self::CurrentStaged => "current-staged",
            Self::StagedTuned => "staged-tuned",
            Self::Capability => "capability-aware",
            Self::Category => "category-aware",
            Self::Reliability => "reliability-aware",
            Self::Progressive => "progressive",
            Self::EarlyStop => "early-stop",
        }
    }
}

/// An engine is unreliable when it has a real sample and half its requests
/// failed. The resting/suspended state is checked separately, by the simulator,
/// for every strategy (production parity).
fn unreliable(states: &VirtualStates, engine: &str) -> bool {
    states.state(engine).is_some_and(|state| {
        state.stats.requests >= 4 && state.stats.errors as f64 / state.stats.requests as f64 >= 0.5
    })
}

/// Engines a strategy would ask.
///
/// An explicit engine list in the request (aggregator.rs:405) wins over every
/// routing rule; only the ordering/filtering part of a strategy is left then.
fn select(
    strategy: Strategy,
    query: &QueryCase,
    providers: &[Provider],
    states: &VirtualStates,
    pinned: Option<&[&'static str]>,
) -> Vec<usize> {
    if let Some(pinned) = pinned {
        return pinned
            .iter()
            .filter_map(|name| providers.iter().position(|provider| provider.name == *name))
            .collect();
    }
    let all: Vec<usize> = (0..providers.len()).collect();
    match strategy {
        Strategy::Capability => {
            let mut selected: Vec<usize> = all
                .iter()
                .copied()
                .filter(|index| providers[*index].fits(query.kind))
                .collect();
            if selected.len() < 3 {
                for index in &all {
                    if selected.len() >= 3 {
                        break;
                    }
                    if !selected.contains(index) && providers[*index].caps.contains(&Cap::Web) {
                        selected.push(*index);
                    }
                }
            }
            selected
        }
        Strategy::Category => {
            let wanted = query.kind.categories();
            let mut selected: Vec<usize> = all
                .iter()
                .copied()
                .filter(|index| {
                    providers[*index]
                        .categories
                        .iter()
                        .any(|category| wanted.contains(category))
                })
                .collect();
            if selected.len() < 3 {
                for index in &all {
                    if selected.len() >= 3 {
                        break;
                    }
                    if !selected.contains(index)
                        && providers[*index].categories.contains(&categories::GENERAL)
                    {
                        selected.push(*index);
                    }
                }
            }
            selected
        }
        Strategy::Reliability => all
            .into_iter()
            .filter(|index| !unreliable(states, providers[*index].name))
            .collect(),
        _ => all,
    }
}

fn execute(strategy: Strategy, sim: &mut Sim<'_>, selected: &[usize], search: &Search) {
    match strategy {
        // One wave, every engine at once: aggregator.rs:702.
        Strategy::CurrentParallel
        | Strategy::Capability
        | Strategy::Category
        | Strategy::Reliability => {
            sim.start_all(selected);
            sim.run_to_idle();
        }
        // aggregator.rs:1120 with the shipping defaults.
        Strategy::CurrentStaged => staged(
            sim,
            selected,
            search.staged_tier.max(1),
            search.staged_min_results.max(1),
        ),
        // Sensitivity check on the tier width only.
        Strategy::StagedTuned => staged(sim, selected, 3, search.staged_min_results.max(1)),
        // The result-count target is 3x staged_min_results: stopping at the
        // shipping 10 results was measured to cost too much recall (see the
        // report), so the candidates get a wider target and the difference to
        // the shipped default stays visible in the table.
        Strategy::Progressive => {
            progressive(sim, selected, search.staged_min_results.max(1) * 3);
        }
        Strategy::EarlyStop => {
            early_stop(sim, selected, search.staged_min_results.max(1) * 3);
        }
    }
}

/// The shipping staged loop: tiers of healthy engines, stop once enough results.
fn staged(sim: &mut Sim<'_>, selected: &[usize], tier: usize, target: usize) {
    let ordered = sim.order_by_health(selected);
    for chunk in ordered.chunks(tier) {
        if sim.now() >= sim.deadline() {
            break;
        }
        sim.start_all(chunk);
        sim.run_to_idle();
        if sim.collected() >= target {
            break;
        }
    }
}

/// Wave 1 of two engines, then double the width while the answer stays thin.
fn progressive(sim: &mut Sim<'_>, selected: &[usize], target: usize) {
    let ordered = sim.order_by_health(selected);
    let first = ordered.len().min(2);
    let (head, mut rest) = ordered.split_at(first);
    sim.start_all(head);
    sim.run_to_idle();

    let mut width = first.max(1);
    while sim.collected() < target && !rest.is_empty() && sim.now() < sim.deadline() {
        width *= 2;
        let take = width.min(rest.len());
        let (wave, tail) = rest.split_at(take);
        sim.start_all(wave);
        sim.run_to_idle();
        rest = tail;
    }
}

/// Start everything, then drop the outstanding requests once the answer is good
/// enough. Dropped results never reach the merge: that is what the latency win
/// is paid for.
fn early_stop(sim: &mut Sim<'_>, selected: &[usize], target: usize) {
    sim.start_all(selected);
    while sim.poll() {
        if sim.collected() >= target {
            break;
        }
    }
}

// --------------------------------------------------------- fusion + metrics

/// Merge one run the way the aggregator does, then take the production ranking.
fn fuse(
    query: &QueryCase,
    run: &RunResult,
    providers: &[Provider],
    search: &Search,
) -> Vec<SearchResult> {
    let mut container = ResultContainer::new();
    for entry in &run.done {
        let provider = &providers[entry.provider];
        match &entry.resp {
            Resp::Ok(docs) => {
                let items: Vec<EngineItem> = docs
                    .iter()
                    .map(|doc| {
                        EngineItem::Result(
                            SearchResult::new(doc.url.clone(), doc.title.clone())
                                .with_content(doc.content.clone()),
                        )
                    })
                    .collect();
                container.extend(provider.name, provider.weight, items);
            }
            Resp::Fail(kind) => container.add_unresponsive_engine(
                provider.name,
                kind.error_type(),
                matches!(kind, FailureKind::Resting),
            ),
        }
    }
    let weight_of = |name: &str| {
        providers
            .iter()
            .find(|provider| provider.name == name)
            .map_or(1.0, |provider| provider.weight)
    };
    let terms = query_terms(&query.text);
    container.close(
        weight_of,
        &terms,
        search.relevance_weight,
        &query.text,
        search.authority_weight,
    );
    container.ordered().iter().take(TOP_K).cloned().collect()
}

#[derive(Default, Clone, Copy)]
struct Observation {
    /// 1-based rank of the first gold result inside the cut, 0 = miss.
    rank: usize,
    mrr: f64,
    ndcg: f64,
    recall: f64,
    /// Mean number of engines behind the top results (corroboration).
    sources: f64,
}

/// Same metric definitions as examples/fusion_lab.rs, so the numbers stay
/// comparable across the two harnesses.
fn observe(ranked: &[SearchResult], gold: &[String], limit: usize) -> Observation {
    let depth = ranked.len().min(limit);
    let mut rank = 0usize;
    let mut dcg = 0.0;
    let mut found = 0usize;
    let mut sources = 0.0;
    for (index, result) in ranked.iter().take(depth).enumerate() {
        let position = index + 1;
        sources += result.engines.len() as f64;
        if gold.iter().any(|url| url == &result.url) {
            if rank == 0 {
                rank = position;
            }
            found += 1;
            dcg += 1.0 / (position as f64 + 1.0).log2();
        }
    }
    let mut idcg = 0.0;
    for position in 1..=found.min(limit) {
        idcg += 1.0 / (position as f64 + 1.0).log2();
    }
    Observation {
        rank,
        mrr: if rank > 0 { 1.0 / rank as f64 } else { 0.0 },
        ndcg: if idcg > 0.0 { dcg / idcg } else { 0.0 },
        recall: if gold.is_empty() {
            0.0
        } else {
            found as f64 / gold.len() as f64
        },
        sources: if depth == 0 {
            0.0
        } else {
            sources / depth as f64
        },
    }
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    hit1: f64,
    hit5: f64,
    hit10: f64,
    recall10: f64,
    mrr10: f64,
    ndcg10: f64,
}

fn aggregate(observations: &[Observation]) -> Metrics {
    let n = observations.len().max(1) as f64;
    let hit = |low: usize, high: usize| {
        observations
            .iter()
            .filter(|item| item.rank >= low && item.rank <= high)
            .count() as f64
            / n
    };
    Metrics {
        hit1: hit(1, 1),
        hit5: hit(1, 5),
        hit10: hit(1, 10),
        recall10: observations.iter().map(|item| item.recall).sum::<f64>() / n,
        mrr10: observations.iter().map(|item| item.mrr).sum::<f64>() / n,
        ndcg10: observations.iter().map(|item| item.ndcg).sum::<f64>() / n,
    }
}

/// Two-sided exact sign test over per-query paired differences.
fn sign_test(wins: usize, losses: usize) -> f64 {
    let n = wins + losses;
    if n == 0 {
        return 1.0;
    }
    let tail = wins.min(losses);
    let mut coefficient = 1.0f64;
    let mut sum = 1.0f64;
    for index in 1..=tail {
        coefficient *= (n - index + 1) as f64 / index as f64;
        sum += coefficient;
    }
    (2.0 * sum / 2f64.powi(n as i32)).min(1.0)
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Everything one (query, strategy) run measured.
struct QueryRow {
    query: usize,
    observation: Observation,
    latency_ms: f64,
    requests: usize,
    suspended_skips: usize,
    cancelled: usize,
    failures: usize,
    deadline_missed: bool,
    /// The run lost at least one engine to a failure or to a suspension.
    degraded: bool,
}

fn evaluate(
    query: &QueryCase,
    run: &RunResult,
    providers: &[Provider],
    search: &Search,
    elapsed_ms: f64,
) -> QueryRow {
    let gold: Vec<String> = query.gold.iter().map(|doc| doc.url.clone()).collect();
    let ranked = fuse(query, run, providers, search);
    QueryRow {
        query: query.index,
        observation: observe(&ranked, &gold, TOP_K),
        latency_ms: elapsed_ms,
        requests: run.issued,
        suspended_skips: run.suspended_skips,
        cancelled: run.cancelled,
        failures: run.failures,
        deadline_missed: run.deadline_missed,
        degraded: run.failures > 0 || run.suspended_skips > 0,
    }
}

/// Per (scenario, strategy) aggregate.
struct Report {
    strategy: Strategy,
    metrics: Metrics,
    rows: Vec<QueryRow>,
    requests_mean: f64,
    latency_p50: f64,
    latency_p95: f64,
    latency_mean: f64,
    /// Share of queries that did not hit the deadline.
    deadline_ok: f64,
    /// Share of queries that still found gold although an engine was lost.
    resilience: f64,
    degraded_queries: usize,
    sources_mean: f64,
    suspended_mean: f64,
    cancelled_mean: f64,
}

fn report(strategy: Strategy, rows: Vec<QueryRow>) -> Report {
    let observations: Vec<Observation> = rows.iter().map(|row| row.observation).collect();
    let n = rows.len().max(1) as f64;
    let mut latencies: Vec<f64> = rows.iter().map(|row| row.latency_ms).collect();
    latencies.sort_by(f64::total_cmp);
    let degraded: Vec<&QueryRow> = rows.iter().filter(|row| row.degraded).collect();
    let resilient = degraded
        .iter()
        .filter(|row| row.observation.rank >= 1 && row.observation.rank <= TOP_K)
        .count();
    Report {
        strategy,
        metrics: aggregate(&observations),
        requests_mean: rows.iter().map(|row| row.requests as f64).sum::<f64>() / n,
        latency_p50: percentile(&latencies, 0.50),
        latency_p95: percentile(&latencies, 0.95),
        latency_mean: rows.iter().map(|row| row.latency_ms).sum::<f64>() / n,
        deadline_ok: rows.iter().filter(|row| !row.deadline_missed).count() as f64 / n,
        resilience: if degraded.is_empty() {
            1.0
        } else {
            resilient as f64 / degraded.len() as f64
        },
        degraded_queries: degraded.len(),
        sources_mean: observations.iter().map(|item| item.sources).sum::<f64>() / n,
        suspended_mean: rows
            .iter()
            .map(|row| row.suspended_skips as f64)
            .sum::<f64>()
            / n,
        cancelled_mean: rows.iter().map(|row| row.cancelled as f64).sum::<f64>() / n,
        rows,
    }
}

// ------------------------------------------------------------------ scenarios

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Healthy,
    PartialSuspend,
    PartialSlow,
    LowQuality,
    RateLimit,
    DuplicateHeavy,
    ExplicitEngines,
    DeadlineOverrun,
}

const SCENARIOS: [Scenario; 8] = [
    Scenario::Healthy,
    Scenario::PartialSuspend,
    Scenario::PartialSlow,
    Scenario::LowQuality,
    Scenario::RateLimit,
    Scenario::DuplicateHeavy,
    Scenario::ExplicitEngines,
    Scenario::DeadlineOverrun,
];

impl Scenario {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::PartialSuspend => "partial-suspend",
            Self::PartialSlow => "partial-slow",
            Self::LowQuality => "all-low-quality",
            Self::RateLimit => "rate-limit",
            Self::DuplicateHeavy => "duplicate-heavy",
            Self::ExplicitEngines => "explicit-engines",
            Self::DeadlineOverrun => "deadline-overrun",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Healthy => "every provider answers, all profiles nominal",
            Self::PartialSuspend => "4 of 14 providers already suspended (captcha) at start",
            Self::PartialSlow => "4 of 14 providers 5x slower",
            Self::LowQuality => {
                "7 of 14 providers return irrelevant pages, 3 answer empty (resting)"
            }
            Self::RateLimit => "3 of 14 providers answer 429 after 20 requests",
            Self::DuplicateHeavy => "every provider leans on the shared commons pool",
            Self::ExplicitEngines => "the request pins 5 engines explicitly",
            Self::DeadlineOverrun => "5 of 14 providers are slower than the 3 s deadline",
        }
    }

    /// Apply the state to the provider set. Returns an explicit engine pin if the
    /// scenario models one.
    fn apply(
        self,
        providers: &mut [Provider],
        states: &mut VirtualStates,
    ) -> Option<Vec<&'static str>> {
        match self {
            Self::Healthy => {}
            Self::PartialSuspend => {
                for name in ["generic-b", "news-b", "generic-d", "sci-c"] {
                    if let Some(provider) = providers.iter().find(|provider| provider.name == name)
                    {
                        states.pre_suspend(provider.name, 60_000.0, "captcha");
                    }
                }
            }
            Self::PartialSlow => {
                for name in ["generic-a", "code-a", "docs-a", "sci-b"] {
                    if let Some(provider) =
                        providers.iter_mut().find(|provider| provider.name == name)
                    {
                        provider.base_ms *= 5.0;
                        provider.jitter_ms *= 3.0;
                        provider.tail_ms *= 1.5;
                    }
                }
            }
            Self::LowQuality => {
                for (index, provider) in providers.iter_mut().enumerate() {
                    if index % 2 == 0 {
                        provider.precision *= 0.15;
                        provider.rank_bias *= 0.5;
                        provider.dup_rate = 0.45;
                    }
                    // A few providers answer with nothing at all: this is the
                    // path that makes the production resting state kick in.
                    if index % 5 == 3 {
                        provider.depth = 0;
                    }
                }
            }
            Self::RateLimit => {
                for name in ["news-a", "code-a", "sci-a"] {
                    if let Some(provider) =
                        providers.iter_mut().find(|provider| provider.name == name)
                    {
                        provider.rate_budget = 20;
                    }
                }
            }
            Self::DuplicateHeavy => {
                for provider in providers.iter_mut() {
                    provider.dup_rate = (provider.dup_rate + 0.35).min(0.9);
                }
            }
            Self::ExplicitEngines => {
                return Some(vec!["code-a", "docs-a", "generic-a", "qa-a", "sci-a"]);
            }
            Self::DeadlineOverrun => {
                for name in ["sci-b", "sci-c", "blog-a", "generic-d", "docs-a"] {
                    if let Some(provider) =
                        providers.iter_mut().find(|provider| provider.name == name)
                    {
                        provider.base_ms = 3_600.0;
                        provider.jitter_ms = 900.0;
                        provider.tail_prob = 0.2;
                        provider.tail_ms = 2_500.0;
                    }
                }
            }
        }
        None
    }
}

// --------------------------------------------------------------------- driver

struct Options {
    queries: usize,
    seed: u64,
    only: Option<String>,
    out: Option<PathBuf>,
    rows: bool,
    offcap_fit: f64,
}

impl Options {
    fn parse() -> Self {
        let mut options = Self {
            queries: 150,
            seed: 7,
            only: None,
            out: None,
            rows: false,
            offcap_fit: DEFAULT_OFFCAP_FIT,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--queries" => {
                    if let Some(value) = args.next() {
                        options.queries = value.parse().unwrap_or(options.queries);
                    }
                }
                "--seed" => {
                    if let Some(value) = args.next() {
                        options.seed = value.parse().unwrap_or(options.seed);
                    }
                }
                "--scenario" => options.only = args.next(),
                "--out" => options.out = args.next().map(PathBuf::from),
                "--rows" => options.rows = true,
                "--offcap-fit" => {
                    if let Some(value) = args.next() {
                        options.offcap_fit = value.parse().unwrap_or(options.offcap_fit);
                    }
                }
                other => eprintln!("ignoring unknown argument {other}"),
            }
        }
        options
    }
}

fn main() {
    let options = Options::parse();
    let search = Search::default();
    let deadline_ms = search.timeout_limit * 1000.0;

    println!("# routing_lab — Phase 4.2 routing comparison (SIMULATION)");
    println!();
    println!(
        "seed = {}, queries per scenario = {}, deadline = {deadline_ms:.0} ms, providers = 14",
        options.seed, options.queries
    );
    println!();
    println!("Synthetic providers only; no network I/O. Relative comparison, not an");
    println!("engine benchmark. Production constants come from Search::default().");
    println!();

    let mut bundle = Vec::new();
    for scenario in SCENARIOS {
        if let Some(only) = &options.only {
            if !only.eq_ignore_ascii_case(scenario.as_str()) {
                continue;
            }
        }
        let mut reports = Vec::with_capacity(STRATEGIES.len());
        for strategy in STRATEGIES {
            reports.push(run_scenario(
                scenario,
                strategy,
                &options,
                &search,
                deadline_ms,
            ));
        }
        bundle.push((scenario, reports));
    }

    let markdown = render_markdown(&bundle, &options, deadline_ms);
    print!("{markdown}");

    if let Some(path) = &options.out {
        let json = render_json(&bundle, &options, &search, deadline_ms);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(path, json) {
            Ok(()) => println!("\nraw data written to {}", path.display()),
            Err(error) => eprintln!("could not write {}: {error}", path.display()),
        }
    }
}

fn run_scenario(
    scenario: Scenario,
    strategy: Strategy,
    options: &Options,
    search: &Search,
    deadline_ms: f64,
) -> Report {
    let mut providers = providers();
    let mut states = VirtualStates::new(search.clone());
    let pinned = scenario.apply(&mut providers, &mut states);
    let mut budgets: Vec<u32> = providers
        .iter()
        .map(|provider| {
            if provider.rate_budget == 0 {
                u32::MAX
            } else {
                provider.rate_budget
            }
        })
        .collect();

    let mut clock = 0.0;
    let mut rows = Vec::with_capacity(options.queries);
    for index in 0..options.queries {
        let mut rng = Rng::new(options.seed ^ ((index as u64 + 1) << 32));
        let query = make_query(&mut rng, index);
        let selected = select(strategy, &query, &providers, &states, pinned.as_deref());
        states.set_now(clock);
        let mut sim = Sim::new(
            &query,
            &providers,
            &mut states,
            &mut budgets,
            clock + deadline_ms,
            options.offcap_fit,
        );
        execute(strategy, &mut sim, &selected, search);
        let run = sim.finish();
        // The simulator works on an absolute virtual clock; the metric is the
        // per-query elapsed time.
        let elapsed = (run.latency_ms - clock).max(0.0);
        let row = evaluate(&query, &run, &providers, search, elapsed);
        clock += QUERY_INTERVAL_MS;
        rows.push(row);
    }
    report(strategy, rows)
}

// -------------------------------------------------------------------- output

fn pct(value: f64) -> String {
    format!("{value:.1}%", value = value * 100.0)
}

fn num3(value: f64) -> String {
    format!("{value:.3}")
}

/// Paired per-query comparison of two strategies on MRR.
fn paired(parallel: &Report, other: &Report) -> (f64, f64, usize, usize) {
    let mut delta = 0.0;
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut compared = 0usize;
    for (left, right) in parallel.rows.iter().zip(other.rows.iter()) {
        let difference = right.observation.mrr - left.observation.mrr;
        if difference.abs() < 1e-9 {
            continue;
        }
        compared += 1;
        delta += difference;
        if difference > 0.0 {
            wins += 1;
        } else {
            losses += 1;
        }
    }
    let mean = if compared == 0 {
        0.0
    } else {
        delta / compared as f64
    };
    (mean, sign_test(wins, losses), wins, losses)
}

fn render_markdown(
    bundle: &[(Scenario, Vec<Report>)],
    options: &Options,
    deadline_ms: f64,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "## Provider model\n");
    let _ = writeln!(
        text,
        "| provider | caps | weight | base ms | fail p | precision | depth | dup | rank bias |"
    );
    let _ = writeln!(
        text,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for provider in providers() {
        let caps: Vec<&str> = provider
            .caps
            .iter()
            .map(|cap| match cap {
                Cap::Web => "web",
                Cap::Code => "code",
                Cap::Docs => "docs",
                Cap::News => "news",
                Cap::Science => "science",
                Cap::Qa => "qa",
            })
            .collect();
        let _ = writeln!(
            text,
            "| {} | {} | {:.1} | {:.0} | {:.2} | {:.2} | {} | {:.2} | {:.2} |",
            provider.name,
            caps.join("+"),
            provider.weight,
            provider.base_ms,
            provider.fail_prob,
            provider.precision,
            provider.depth,
            provider.dup_rate,
            provider.rank_bias
        );
    }

    for (scenario, reports) in bundle {
        let _ = writeln!(
            text,
            "\n## {} — {}\n",
            scenario.as_str(),
            scenario.describe()
        );
        let _ = writeln!(
            text,
            "| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |"
        );
        let _ = writeln!(
            text,
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
        );
        let parallel = &reports[0];
        for report in reports {
            let (delta, p_value, _wins, _losses) = paired(parallel, report);
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} | {} | {} | {} | {:.1} | {:.0} | {:.0} | {} | {} | {:.3} | {:.3} |",
                report.strategy.as_str(),
                pct(report.metrics.hit1),
                pct(report.metrics.hit5),
                pct(report.metrics.hit10),
                pct(report.metrics.recall10),
                num3(report.metrics.mrr10),
                num3(report.metrics.ndcg10),
                report.requests_mean,
                report.latency_p50,
                report.latency_p95,
                pct(report.deadline_ok),
                pct(report.resilience),
                delta,
                p_value
            );
        }
    }

    let _ = writeln!(text, "\n## Summary — mean over scenarios\n");
    let _ = writeln!(
        text,
        "| strategy | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req/query | p95 ms | deadline | resilience |"
    );
    let _ = writeln!(
        text,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    let scenario_count = bundle.len().max(1) as f64;
    for strategy in STRATEGIES {
        let picked: Vec<&Report> = bundle
            .iter()
            .filter_map(|(_, reports)| reports.iter().find(|report| report.strategy == strategy))
            .collect();
        if picked.is_empty() {
            continue;
        }
        let mean = |selector: &dyn Fn(&Report) -> f64| {
            picked.iter().map(|report| selector(report)).sum::<f64>() / scenario_count
        };
        let worst = |selector: &dyn Fn(&Report) -> f64| {
            picked
                .iter()
                .map(|report| selector(report))
                .fold(f64::INFINITY, f64::min)
        };
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} | {:.1} | {:.0} | {} | {} |",
            strategy.as_str(),
            pct(mean(&|report| report.metrics.hit10)),
            pct(mean(&|report| report.metrics.recall10)),
            num3(mean(&|report| report.metrics.mrr10)),
            num3(mean(&|report| report.metrics.ndcg10)),
            mean(&|report| report.requests_mean),
            mean(&|report| report.latency_p95),
            pct(worst(&|report| report.deadline_ok)),
            pct(worst(&|report| report.resilience)),
        );
    }

    let _ = writeln!(text, "\n## Dominance check (mechanical)\n");
    let _ = writeln!(
        text,
        "A scenario counts as dominated when the strategy issues no more requests and"
    );
    let _ = writeln!(text, "keeps MRR@10 within 0.005 of current-parallel.\n");
    let _ = writeln!(
        text,
        "| strategy | scenarios dominated | scenarios with MRR loss > 0.02 | worst MRR loss |"
    );
    let _ = writeln!(text, "| --- | --- | --- | --- |");
    for strategy in STRATEGIES {
        if strategy == Strategy::CurrentParallel {
            continue;
        }
        let mut dominated = 0usize;
        let mut worse = 0usize;
        let mut worst = 0.0f64;
        for (_, reports) in bundle {
            let parallel = &reports[0];
            let Some(report) = reports.iter().find(|item| item.strategy == strategy) else {
                continue;
            };
            let loss = report.metrics.mrr10 - parallel.metrics.mrr10;
            if report.requests_mean <= parallel.requests_mean + 1e-9 && loss >= -0.005 {
                dominated += 1;
            }
            if loss < -0.02 {
                worse += 1;
            }
            worst = worst.min(loss);
        }
        let _ = writeln!(
            text,
            "| {} | {} / {} | {} | {:.3} |",
            strategy.as_str(),
            dominated,
            bundle.len(),
            worse,
            worst
        );
    }

    let _ = writeln!(
        text,
        "\nReproduce: cargo run --release --example routing_lab -- --queries {} --seed {} --out bench/local-retrieval/routing-lab/routing-lab.json",
        options.queries, options.seed
    );
    let _ = writeln!(
        text,
        "Simulated deadline: {:.0} ms (Search::default().timeout_limit).",
        deadline_ms
    );
    text
}

fn render_json(
    bundle: &[(Scenario, Vec<Report>)],
    options: &Options,
    search: &Search,
    deadline_ms: f64,
) -> String {
    let mut results = Vec::new();
    let mut rows = Vec::new();
    for (scenario, reports) in bundle {
        let parallel = &reports[0];
        for report in reports {
            let (delta, p_value, wins, losses) = paired(parallel, report);
            results.push(serde_json::json!({
                "scenario": scenario.as_str(),
                "strategy": report.strategy.as_str(),
                "hit1": report.metrics.hit1,
                "hit5": report.metrics.hit5,
                "hit10": report.metrics.hit10,
                "recall10": report.metrics.recall10,
                "mrr10": report.metrics.mrr10,
                "ndcg10": report.metrics.ndcg10,
                "requests_mean": report.requests_mean,
                "suspended_mean": report.suspended_mean,
                "issued_mean": report.cancelled_mean,
                "latency_p50": report.latency_p50,
                "latency_p95": report.latency_p95,
                "latency_mean": report.latency_mean,
                "deadline_ok": report.deadline_ok,
                "resilience": report.resilience,
                "degraded_queries": report.degraded_queries,
                "sources_mean": report.sources_mean,
                "dmrr_vs_parallel": delta,
                "p_value": p_value,
                "paired_wins": wins,
                "paired_losses": losses,
            }));
            if options.rows {
                // Compact per-query raw data: one array per row, see rows_layout.
                for row in &report.rows {
                    rows.push(serde_json::json!([
                        scenario.as_str(),
                        report.strategy.as_str(),
                        row.query,
                        row.observation.rank,
                        (row.observation.mrr * 1000.0).round() / 1000.0,
                        (row.observation.ndcg * 1000.0).round() / 1000.0,
                        (row.observation.recall * 1000.0).round() / 1000.0,
                        row.latency_ms.round(),
                        row.requests,
                        row.suspended_skips,
                        row.cancelled,
                        row.failures,
                        u8::from(row.deadline_missed),
                        u8::from(row.degraded),
                    ]));
                }
            }
        }
    }

    let providers_json: Vec<serde_json::Value> = providers()
        .into_iter()
        .map(|provider| {
            serde_json::json!({
                "name": provider.name,
                "weight": provider.weight,
                "base_ms": provider.base_ms,
                "jitter_ms": provider.jitter_ms,
                "tail_prob": provider.tail_prob,
                "tail_ms": provider.tail_ms,
                "fail_prob": provider.fail_prob,
                "precision": provider.precision,
                "depth": provider.depth,
                "dup_rate": provider.dup_rate,
                "rank_bias": provider.rank_bias,
            })
        })
        .collect();

    let document = serde_json::json!({
        "harness": "routing_lab",
        "kind": "simulation",
        "note": "synthetic providers, no network I/O; relative comparison only",
        "seed": options.seed,
        "queries_per_scenario": options.queries,
        "deadline_ms": deadline_ms,
        "offcap_fit": options.offcap_fit,
        "rows_layout": [
            "scenario", "strategy", "query", "rank", "mrr", "ndcg", "recall",
            "latency_ms", "requests", "suspended_skips", "cancelled", "failures",
            "deadline_missed", "degraded"
        ],
        "settings_used": {
            "strategy_default": "parallel",
            "staged_tier": search.staged_tier,
            "staged_min_results": search.staged_min_results,
            "timeout_limit": search.timeout_limit,
            "relevance_weight": search.relevance_weight,
            "authority_weight": search.authority_weight,
            "ban_time_on_fail": search.ban_time_on_fail,
            "max_ban_time_on_fail": search.max_ban_time_on_fail,
            "empty_result_limit": search.empty_result_limit,
            "empty_result_cooldown": search.empty_result_cooldown,
        },
        "providers": providers_json,
        "results": results,
        "rows": if options.rows { Some(rows) } else { None },
    });
    // Per-query rows dominate the size: keep the aggregate file readable, the
    // raw dump compact.
    let rendered = if options.rows {
        serde_json::to_string(&document)
    } else {
        serde_json::to_string_pretty(&document)
    };
    rendered.unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_generation_is_deterministic() {
        let mut left = Rng::new(11);
        let mut right = Rng::new(11);
        let first = make_query(&mut left, 3);
        let second = make_query(&mut right, 3);
        assert_eq!(first.text, second.text);
        assert_eq!(first.gold.len(), second.gold.len());
        assert_eq!(first.gold[0].url, second.gold[0].url);
    }

    /// The relevance blend only lifts a page whose title covers the query terms.
    #[test]
    fn gold_pages_cover_every_query_term() {
        for index in 0..12 {
            let mut rng = Rng::new(5);
            let query = make_query(&mut rng, index);
            for term in query_terms(&query.text) {
                assert!(
                    query
                        .gold
                        .iter()
                        .all(|doc| doc.title.contains(&term) || doc.content.contains(&term)),
                    "gold doc missing term {term} for query {}",
                    query.index
                );
            }
        }
    }

    #[test]
    fn sign_test_is_a_two_sided_exact_test() {
        assert!((sign_test(0, 0) - 1.0).abs() < 1e-12);
        assert!(sign_test(6, 0) < 0.05);
        assert!(sign_test(3, 3) > 0.9);
    }

    #[test]
    fn observe_ranks_gold_and_ignores_others() {
        let gold = vec!["https://gold.example/doc".to_string()];
        let ranked = vec![
            SearchResult::new("https://noise.example/1", "noise"),
            SearchResult::new("https://gold.example/doc", "gold"),
        ];
        let observation = observe(&ranked, &gold, TOP_K);
        assert_eq!(observation.rank, 2);
        assert!((observation.mrr - 0.5).abs() < 1e-9);
        assert!((observation.recall - 1.0).abs() < 1e-9);
    }

    /// Production parity: a suspended engine costs no request (aggregator.rs:1021).
    #[test]
    fn suspended_engines_are_skipped_without_a_request() {
        let search = Search::default();
        let providers = providers();
        let mut states = VirtualStates::new(search);
        states.pre_suspend(providers[0].name, 10_000.0, "captcha");
        let mut rng = Rng::new(1);
        let query = make_query(&mut rng, 0);
        let mut budgets: Vec<u32> = providers.iter().map(|_| u32::MAX).collect();
        let mut sim = Sim::new(
            &query,
            &providers,
            &mut states,
            &mut budgets,
            3_000.0,
            DEFAULT_OFFCAP_FIT,
        );
        sim.start(0);
        let run = sim.finish();
        assert_eq!(run.issued, 0);
        assert_eq!(run.suspended_skips, 1);
    }
}
