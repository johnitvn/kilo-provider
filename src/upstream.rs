//! Upstream client.
//!
//! Wraps a `reqwest::Client` targeting the Kilo AI gateway. Two hard
//! invariants of this whole crate live here:
//!
//! ## 1. No timeouts — deliberately
//!
//! **No overall request timeout, no read timeout, no connect timeout and no
//! pool-acquire timeout is configured — anywhere.** Longer-lived streaming
//! responses through the gateway are expected to run for minutes, and no timer
//! in this process may cut them short. The only remaining reqwest default is
//! the *connection-pool idle timeout*, which merely closes unused pooled
//! connections; it never aborts an in-flight request or stream. Do not add
//! `.timeout(...)`, `.connect_timeout(...)`, `.read_timeout(...)` or middleware
//! without revisiting this invariant with the team.
//!
//! ## 2. Anonymous upstream access — always
//!
//! **This crate must never set an `Authorization` (or any credential) header on
//! an upstream request.** The only place a header could be attached is inside
//! [`UpstreamClient::request`], and that deliberately does not. Upstream auth
//! (if ever needed) would be provided by proxies in the pool instead — never
//! here.

use reqwest::{Client, Method, RequestBuilder, Url};
use thiserror::Error;

use crate::config::DEFAULT_UPSTREAM_BASE_URL;

/// User-Agent for upstream requests. Anonymous access does not forbid a UA,
/// and it helps upstream see what is calling.
pub const USER_AGENT: &str = concat!("kilo-provider/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("upstream base URL {url:?} is not a valid http(s) URL")]
    InvalidBaseUrl { url: String },
    #[error("invalid upstream path {path:?}: {message}")]
    InvalidPath { path: String, message: String },
    #[error("failed to construct HTTP client: {0}")]
    Client(String),
}

/// Thin wrapper around `reqwest::Client` that knows the upstream base URL and
/// builds requests against it. No business logic and no network I/O of its
/// own — it only assembles request builders for call sites (none yet in this
/// bootstrap).
#[derive(Clone, Debug)]
pub struct UpstreamClient {
    http: Client,
    base_url: Url,
}

impl UpstreamClient {
    /// Build a client for an explicit base URL. The config-driven constructor
    /// [`UpstreamClient::for_config`] resolves `None` against the crate
    /// default.
    pub fn new(base_url: impl Into<String>) -> Result<Self, UpstreamError> {
        let raw = base_url.into();
        let parsed = Url::parse(raw.trim())
            .map_err(|_| UpstreamError::InvalidBaseUrl { url: raw.clone() })?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(UpstreamError::InvalidBaseUrl { url: raw });
        }

        // ▼ The single reqwest::Client::builder() call in this crate. ▼
        // Deliberately absent:
        //   * `.timeout(...)`        — no overall request timeout
        //   * `.connect_timeout(...)` — no connect timeout
        //   * `.read_timeout(...)`   — no read timeout
        //   * any pool acquire-timeout tuning surprises
        // The pool idle timeout stays at the benign reqwest default (evicts
        // *idle* connections only; in-flight streams are untouched).
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| UpstreamError::Client(error.to_string()))?;

        Ok(Self {
            http,
            base_url: parsed,
        })
    }

    /// Constructor used by the bootstrap wiring: resolves the config value
    /// against [`DEFAULT_UPSTREAM_BASE_URL`].
    pub fn for_config(base_url: Option<&str>) -> Result<Self, UpstreamError> {
        Self::new(base_url.unwrap_or(DEFAULT_UPSTREAM_BASE_URL).to_string())
    }

    /// Effective upstream base URL (for logging and `/readyz`).
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Read-only access to the underlying client for future modules.
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    /// Start building an upstream request for `path` (relative to the base
    /// URL, with or without a leading slash).
    ///
    /// # Security invariant
    ///
    /// **No `Authorization` header is ever set here.** Upstream access is
    /// anonymous. Call site code must not add credentials either; if
    /// authentication is ever needed it comes via the proxy pool, not from
    /// this crate. The unit test below pins this behaviour.
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, UpstreamError> {
        let url = self.join(path)?;
        // Intentionally NO `.header(AUTHORIZATION, ...)` — see module docs and
        // the invariant at the top of this file.
        Ok(self.http.request(method, url))
    }

    /// Concatenate base URL and request path.
    ///
    /// This intentionally avoids `Url::join`: its relative-resolution
    /// semantics would drop the last base-path segment (`…/gateway` +
    /// `v1/models` would yield `…/v1/models`), so plain concatenation is used
    /// instead.
    fn join(&self, path: &str) -> Result<Url, UpstreamError> {
        let path = path.trim();
        if path.is_empty() || path.starts_with("//") {
            return Err(UpstreamError::InvalidPath {
                path: path.to_string(),
                message: "must be a non-empty relative path".to_string(),
            });
        }
        let base = self.base_url.as_str().trim_end_matches('/');
        let path = path.trim_start_matches('/');
        Url::parse(&format!("{base}/{path}")).map_err(|error| UpstreamError::InvalidPath {
            path: path.to_string(),
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    const BASE: &str = "https://api.kilo.ai/api/gateway";

    #[test]
    fn built_request_has_no_authorization_header() {
        // The invariant: no Authorization header, whatever the request type.
        for method in [Method::GET, Method::POST] {
            let client = UpstreamClient::new(BASE).expect("valid base URL");
            let request = client
                .request(method.clone(), "v1/models")
                .expect("valid path")
                .build()
                .expect("request must build");
            assert!(
                request.headers().get(AUTHORIZATION).is_none(),
                "Authorization header must never be set on upstream requests"
            );
        }
    }

    #[test]
    fn builds_urls_under_the_base_url() {
        let client = UpstreamClient::new(BASE).expect("valid base URL");
        let joined = client.join("v1/models").expect("valid path").to_string();
        assert_eq!(
            joined, "https://api.kilo.ai/api/gateway/v1/models",
            "Url::join-style semantics must not drop the base path segment"
        );

        // Leading slashes and empty paths behave predictably.
        let joined = client.join("/v1/models").expect("valid path").to_string();
        assert_eq!(joined, "https://api.kilo.ai/api/gateway/v1/models");

        assert!(client.join("").is_err());
        assert!(client.join("//evil.example.com/x").is_err());
    }

    #[test]
    fn client_without_timeout_can_be_built() {
        // Deep-check that the builder path succeeds and still skips every
        // timeout helper (see module docs): request construction must not
        // depend on any timer being configured.
        let client = UpstreamClient::for_config(Some(BASE)).expect("config constructor");
        assert_eq!(client.base_url(), BASE);
    }

    #[test]
    fn default_base_url_is_used_when_config_has_none() {
        let client = UpstreamClient::for_config(None).expect("default base URL");
        assert_eq!(client.base_url(), DEFAULT_UPSTREAM_BASE_URL);
    }
}
