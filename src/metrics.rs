//! Instance wide counters, ported from `searx/metrics/`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Counters incremented by the searcher and the web layer.
#[derive(Debug)]
pub struct Metrics {
    started: Instant,
    pub searches: AtomicU64,
    pub engine_requests: AtomicU64,
    pub engine_errors: AtomicU64,
    pub engine_timeouts: AtomicU64,
    pub results: AtomicU64,
    pub cache_hits: AtomicU64,
    /// Responses by status class (2xx, 3xx, 4xx, 5xx); 1xx is folded into
    /// the 2xx slot because a successful early response is still a success.
    pub response_2xx: AtomicU64,
    pub response_3xx: AtomicU64,
    pub response_4xx: AtomicU64,
    pub response_5xx: AtomicU64,
    /// Total time spent inside searches, in microseconds, and how many ran.
    pub search_duration_micros: AtomicU64,
    pub search_duration_count: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            searches: AtomicU64::new(0),
            engine_requests: AtomicU64::new(0),
            engine_errors: AtomicU64::new(0),
            engine_timeouts: AtomicU64::new(0),
            results: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            response_2xx: AtomicU64::new(0),
            response_3xx: AtomicU64::new(0),
            response_4xx: AtomicU64::new(0),
            response_5xx: AtomicU64::new(0),
            search_duration_micros: AtomicU64::new(0),
            search_duration_count: AtomicU64::new(0),
        }
    }

    /// Record how long one search took.
    pub fn record_search_duration(&self, duration: std::time::Duration) {
        self.search_duration_micros.fetch_add(
            duration.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        self.search_duration_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one HTTP response by its status class.
    pub fn record_status(&self, status: u16) {
        let counter = match status {
            100..=299 => &self.response_2xx,
            300..=399 => &self.response_3xx,
            400..=499 => &self.response_4xx,
            _ => &self.response_5xx,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr(counter: &AtomicU64, amount: u64) {
        counter.fetch_add(amount, Ordering::Relaxed);
    }

    /// A serialisable snapshot, used by `/stats`.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            uptime: self.started.elapsed().as_secs_f64(),
            searches: self.searches.load(Ordering::Relaxed),
            engine_requests: self.engine_requests.load(Ordering::Relaxed),
            engine_errors: self.engine_errors.load(Ordering::Relaxed),
            engine_timeouts: self.engine_timeouts.load(Ordering::Relaxed),
            results: self.results.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            response_2xx: self.response_2xx.load(Ordering::Relaxed),
            response_3xx: self.response_3xx.load(Ordering::Relaxed),
            response_4xx: self.response_4xx.load(Ordering::Relaxed),
            response_5xx: self.response_5xx.load(Ordering::Relaxed),
            search_duration_micros: self.search_duration_micros.load(Ordering::Relaxed),
            search_duration_count: self.search_duration_count.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub uptime: f64,
    pub searches: u64,
    pub engine_requests: u64,
    pub engine_errors: u64,
    pub engine_timeouts: u64,
    pub results: u64,
    pub cache_hits: u64,
    pub response_2xx: u64,
    pub response_3xx: u64,
    pub response_4xx: u64,
    pub response_5xx: u64,
    pub search_duration_micros: u64,
    pub search_duration_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_reported() {
        let metrics = Metrics::new();
        Metrics::incr(&metrics.searches, 3);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.searches, 3);
        assert!(snapshot.uptime >= 0.0);
    }

    #[test]
    fn search_durations_accumulate() {
        let metrics = Metrics::new();
        metrics.record_search_duration(std::time::Duration::from_millis(250));
        metrics.record_search_duration(std::time::Duration::from_millis(750));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.search_duration_count, 2);
        assert_eq!(snapshot.search_duration_micros, 1_000_000);
    }

    #[test]
    fn responses_are_counted_by_status_class() {
        let metrics = Metrics::new();
        for status in [200, 204, 301, 400, 429, 500, 503] {
            metrics.record_status(status);
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.response_2xx, 2);
        assert_eq!(snapshot.response_3xx, 1);
        assert_eq!(snapshot.response_4xx, 2);
        assert_eq!(snapshot.response_5xx, 2);
    }
}
