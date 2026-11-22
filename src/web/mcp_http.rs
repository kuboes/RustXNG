//! Model Context Protocol over Streamable HTTP (`/mcp`).
//!
//! The stdio server (`rustxng mcp`) serves one client per process. This module
//! mounts the same tools at `/mcp` on the HTTP listener so a remote agent can
//! reach them, with the session handling the protocol requires.
//!
//! Exposure is opt-in: `mcp.http = true` in the settings, because an MCP
//! endpoint hands an agent the same fetch capability as `read_page` and a
//! public instance should decide that deliberately. The endpoint still
//! validates destinations and honours robots.txt, and it is mounted on the
//! same listener as the rest of the API.

use std::sync::Arc;

use axum::Router;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

use crate::aggregator::Searcher;
use crate::mcp::McpServer;

/// Path the MCP endpoint is mounted at.
pub const PATH: &str = "/mcp";

/// Build the router that serves MCP over Streamable HTTP.
///
/// `allowed_hosts` is the deployment's own authority list: the transport
/// refuses a `Host` header it was not told about, which is what keeps a
/// locally bound instance safe from DNS rebinding. An empty list keeps the
/// transport default (loopback only).
pub fn router(searcher: Arc<Searcher>, allowed_hosts: Vec<String>) -> Router {
    let mut config = StreamableHttpServerConfig::default();
    if !allowed_hosts.is_empty() {
        config = config.with_allowed_hosts(allowed_hosts);
    }
    let service = StreamableHttpService::new(
        move || McpServer::new(searcher.clone()).map_err(std::io::Error::other),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new().nest_service(PATH, service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_path_is_stable() {
        assert_eq!(PATH, "/mcp");
    }
}
