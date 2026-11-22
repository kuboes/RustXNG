//! Startup diagnostics: is this configuration able to search?
//!
//! The same answer is used by `/readyz` and by `rustxng --check-config`, so the
//! HTTP endpoint and the CLI cannot drift apart.

use serde::Serialize;

use crate::settings::Settings;

/// Outcome of evaluating a configuration without contacting any provider.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// Number of engines the registry built from the configuration.
    pub engines: usize,
    /// Whether a configured local index exists on disk.
    pub local_index: bool,
    /// Why the instance cannot search. Empty when `ready` is true.
    pub reasons: Vec<String>,
}

impl Readiness {
    /// Evaluate `settings` for a registry of `engines` engines.
    ///
    /// Only local facts are inspected: a configured engine, or a local index.
    /// `index_open` is what the searcher actually opened (`Searcher` keeps
    /// `None` when the directory is corrupt); `None` falls back to the path
    /// check for callers that have no searcher. Upstream availability is not
    /// checked here.
    pub fn evaluate(settings: &Settings, engines: usize, index_open: Option<bool>) -> Self {
        let configured = || {
            [
                settings.search.local_index_path.trim(),
                settings.search.wikipedia_index_path.trim(),
            ]
            .iter()
            .filter(|path| !path.is_empty())
            .any(|path| std::path::Path::new(path).join("meta.json").is_file())
        };
        let local_index = index_open.unwrap_or_else(configured);
        let ready = engines > 0 || local_index;
        let reasons = if ready {
            Vec::new()
        } else {
            vec![
                "no search engine is enabled".to_string(),
                "no local index is available".to_string(),
            ]
        };
        Self {
            ready,
            engines,
            local_index,
            reasons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_engine_or_an_index_is_enough() {
        let mut settings = Settings::defaults();
        settings.search.local_index_path = String::new();
        settings.search.wikipedia_index_path = String::new();
        assert_eq!(
            Readiness::evaluate(&settings, 1, None),
            Readiness {
                ready: true,
                engines: 1,
                local_index: false,
                reasons: Vec::new()
            }
        );
        let empty = Readiness::evaluate(&settings, 0, None);
        assert!(!empty.ready);
        assert_eq!(empty.reasons.len(), 2);
    }
}
