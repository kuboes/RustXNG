//! `/map` and `/extract`: page discovery and structured extraction.
//!
//! Both endpoints take a caller-supplied URL, so they use the same checked
//! page reader as `read_page` and `include_raw_content`: destinations are
//! validated (no private addresses, no unvalidated redirects) and robots.txt
//! is honoured. The limiter applies like every other fetch route.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::AppState;
use super::limiter::ClientAddr;
use crate::extract::{MAX_MAP_URLS, MAX_RECORDS};

/// Query parameters of `/map`.
#[derive(Debug, Deserialize)]
pub struct MapParams {
    pub url: Option<String>,
    /// Most URLs to return; clamped to [`MAX_MAP_URLS`].
    pub limit: Option<usize>,
}

/// Query parameters of `/extract`.
#[derive(Debug, Deserialize)]
pub struct ExtractParams {
    pub url: Option<String>,
    /// Most records to return; clamped to [`MAX_RECORDS`].
    pub limit: Option<usize>,
}

/// A JSON error, matching the shape the rest of the API uses.
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The 400 both endpoints answer when `url` is missing or blank.
fn missing_url() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "url is required and must be an absolute http(s) URL",
    )
}

/// The URL argument, or `None` when it is missing or blank.
///
/// Returning the value rather than a response keeps the error path at the
/// handler, where the 400 is produced once.
pub(crate) fn url_argument(url: &Option<String>) -> Option<String> {
    url.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `GET /map`: the URLs a site declares, without crawling it.
pub async fn map(
    State(state): State<AppState>,
    Query(params): Query<MapParams>,
    peer: ClientAddr,
    headers: HeaderMap,
) -> Response {
    let Some(url) = url_argument(&params.url) else {
        return missing_url();
    };
    if let Some(response) = super::handlers::limiter_response(
        &state,
        "json",
        state.limiter.check(peer.0, &headers, "json"),
    ) {
        return response;
    }
    let limit = params.limit.unwrap_or(MAX_MAP_URLS).clamp(1, MAX_MAP_URLS);
    match crate::extract::map_site(&state.searcher.page_reader, &url, limit).await {
        Ok(report) => Json(report).into_response(),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

/// `GET /extract`: structured records from one page.
pub async fn extract(
    State(state): State<AppState>,
    Query(params): Query<ExtractParams>,
    peer: ClientAddr,
    headers: HeaderMap,
) -> Response {
    let Some(url) = url_argument(&params.url) else {
        return missing_url();
    };
    if let Some(response) = super::handlers::limiter_response(
        &state,
        "json",
        state.limiter.check(peer.0, &headers, "json"),
    ) {
        return response;
    }
    let limit = params.limit.unwrap_or(MAX_RECORDS).clamp(1, MAX_RECORDS);
    match crate::extract::extract(&state.searcher.page_reader, &url, limit).await {
        Ok(report) => Json(report).into_response(),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractReport, MapReport};

    #[test]
    fn a_missing_url_is_a_bad_request() {
        for missing in [None, Some(String::new()), Some("   ".to_string())] {
            assert!(url_argument(&missing).is_none(), "{missing:?}");
            assert_eq!(missing_url().status(), StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn a_present_url_is_returned_trimmed() {
        assert_eq!(
            url_argument(&Some("  https://example.com/  ".to_string())).unwrap(),
            "https://example.com/"
        );
    }

    #[test]
    fn the_reports_serialise_with_their_sources() {
        let report = MapReport {
            seed: "https://example.com/".into(),
            origin: "https://example.com".into(),
            used_sitemap: true,
            truncated: false,
            urls: vec![crate::extract::MappedUrl {
                url: "https://example.com/a".into(),
                depth: 1,
                source: crate::extract::UrlSource::RobotsSitemap,
            }],
            errors: Vec::new(),
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["urls"][0]["source"], "robots_sitemap");
        let extract = ExtractReport {
            url: "https://example.com/".into(),
            records: Vec::new(),
            truncated: false,
        };
        assert!(serde_json::to_value(&extract).unwrap()["records"].is_array());
    }
}
