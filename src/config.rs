//! Configuration loading for `kilo-provider`.
//!
//! The process is driven by **a single YAML configuration file** whose location
//! is given by the `CONFIG_PATH` environment variable (default: `config.yaml`
//! relative to the current working directory). Everything the gateway needs —
//! including the proxy pool — is read from this one file.
//!
//! `config.example.yaml` at the repository root is the documented sample; it
//! must never contain real credentials. Upstream access is anonymous, so no
//! secrets exist here at all.

use std::{fs, path::Path};

use serde::Deserialize;
use thiserror::Error;

/// Upstream base URL used when the config file does not override it.
pub const DEFAULT_UPSTREAM_BASE_URL: &str = "https://api.kilo.ai/api/gateway";

/// Every error the configuration layer can produce, with clear messages so a
/// misconfigured deployment fails at startup instead of at first request.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read configuration file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse configuration as YAML: {0}")]
    Parse(String),
    #[error("invalid configuration: {0}")]
    Validation(String),
}

/// Top-level configuration document (the whole YAML file).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub upstream: UpstreamConfig,
    pub proxy_pool: ProxyPoolConfig,
    #[serde(default)]
    pub model_catalog: ModelCatalogConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    #[serde(default)]
    pub log: LogConfig,
}

/// `upstream` section. `base_url` is optional and defaults to
/// [`DEFAULT_UPSTREAM_BASE_URL`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpstreamConfig {
    pub base_url: Option<String>,
}

/// Proxy descriptor from the config file.
#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `proxy_pool` section. `proxies` is a **required** list (may be empty, but
/// the key itself must be present so an accidentally dropped section is caught
/// at startup).
#[derive(Debug, Clone, Deserialize)]
pub struct ProxyPoolConfig {
    pub proxies: Vec<ProxyConfig>,
}

/// `model_catalog` section — stub settings only. The actual catalog behaviour
/// is implemented in a follow-up feature PR; these knobs are parsed now so the
/// schema is stable.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelCatalogConfig {
    pub refresh_interval_secs: Option<u64>,
    pub cache_ttl_secs: Option<u64>,
}

/// `circuit_breaker` section — threshold/timeout settings consumed by
/// `crate::breaker`. Not yet wired into the request path (bootstrap only).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures that open the circuit.
    pub failure_threshold: u32,
    /// Consecutive successes in half-open state required to close it again.
    pub success_threshold: u32,
    /// How long the circuit stays open before allowing probes (half-open).
    pub open_timeout_secs: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 3,
            open_timeout_secs: 30,
        }
    }
}

/// `log` section: select JSON structured logs or human-readable text.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogConfig {
    pub format: Option<String>,
    pub level: Option<String>,
}

/// Resolved log format used by the tracing subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Text,
}

impl Config {
    /// Effective upstream base URL (explicit value or the built-in default).
    pub fn upstream_base_url(&self) -> &str {
        self.upstream
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_UPSTREAM_BASE_URL)
    }

    /// Resolved log format; anything other than `json` was already rejected by
    /// validation, so `text` is the catch-all.
    pub fn log_format(&self) -> LogFormat {
        if self.log.format.as_deref() == Some("json") {
            LogFormat::Json
        } else {
            LogFormat::Text
        }
    }
}

/// Load and validate the configuration from `path`. This is the single entry
/// point used at startup and by pool reloads.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    from_str(&contents)
}

/// Parse configuration from an in-memory YAML document (also used by unit
/// tests). Applies defaults and runs validation.
pub fn from_str(yaml: &str) -> Result<Config, ConfigError> {
    let config: Config =
        serde_yaml::from_str(yaml).map_err(|error| ConfigError::Parse(error.to_string()))?;
    validate(&config)?;
    Ok(config)
}

/// Basic sanity checks. Keep them cheap and deterministic: they run at every
/// startup and every pool reload.
fn validate(config: &Config) -> Result<(), ConfigError> {
    // upstream: if a base_url override is given it must be an http(s) URL.
    if let Some(base_url) = &config.upstream.base_url {
        let trimmed = base_url.trim();
        if trimmed.is_empty()
            || !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        {
            return Err(ConfigError::Validation(format!(
                "upstream.base_url must be an http(s) URL, got {base_url:?}"
            )));
        }
    }

    // proxy_pool: non-empty names/urls, only http(s) schemes, no duplicates.
    let mut seen_names = std::collections::HashSet::new();
    let mut seen_urls = std::collections::HashSet::new();
    for (index, proxy) in config.proxy_pool.proxies.iter().enumerate() {
        if proxy.name.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "proxy_pool.proxies[{index}]: 'name' must not be empty"
            )));
        }
        let url = proxy.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ConfigError::Validation(format!(
                "proxy_pool.proxies[{index}] ({}): 'url' must be an http(s) URL, got {url:?}",
                proxy.name
            )));
        }
        if !seen_names.insert(proxy.name.as_str()) {
            return Err(ConfigError::Validation(format!(
                "proxy_pool.proxies[{index}]: duplicate proxy name '{}'",
                proxy.name
            )));
        }
        if !seen_urls.insert(url) {
            return Err(ConfigError::Validation(format!(
                "proxy_pool.proxies[{index}] ({}): duplicate proxy url {url:?}",
                proxy.name
            )));
        }
    }

    // circuit_breaker: thresholds must be strictly positive to behave.
    if config.circuit_breaker.failure_threshold == 0 {
        return Err(ConfigError::Validation(
            "circuit_breaker.failure_threshold must be at least 1".to_string(),
        ));
    }
    if config.circuit_breaker.success_threshold == 0 {
        return Err(ConfigError::Validation(
            "circuit_breaker.success_threshold must be at least 1".to_string(),
        ));
    }

    // log: only the two known formats are accepted.
    // if-like collapsed condition (let-chain, supported by edition 2024)
    if let Some(format) = &config.log.format
        && format != "json"
        && format != "text"
    {
        return Err(ConfigError::Validation(format!(
            "log.format must be 'json' or 'text', got {format:?}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
upstream:
  base_url: https://api.kilo.ai/api/gateway

proxy_pool:
  proxies:
    - name: placeholder-a
      url: http://localhost:0000
    - name: placeholder-b
      url: http://localhost:0001

model_catalog:
  refresh_interval_secs: 300
  cache_ttl_secs: 60

circuit_breaker:
  failure_threshold: 7
  success_threshold: 2
  open_timeout_secs: 15

log:
  format: json
  level: debug
"#;

    #[test]
    fn parses_sample_document() {
        let config = from_str(SAMPLE).expect("sample config must parse");
        assert_eq!(config.proxy_pool.proxies.len(), 2);
        assert_eq!(config.proxy_pool.proxies[0].name, "placeholder-a");
        assert_eq!(
            config.upstream_base_url(),
            "https://api.kilo.ai/api/gateway"
        );
        assert_eq!(config.circuit_breaker.failure_threshold, 7);
        assert_eq!(config.log_format(), LogFormat::Json);
        assert_eq!(config.log.level.as_deref(), Some("debug"));
    }

    #[test]
    fn defaults_are_applied() {
        let config = from_str("proxy_pool: { proxies: [] }").expect("minimal config must parse");
        assert_eq!(config.upstream_base_url(), DEFAULT_UPSTREAM_BASE_URL);
        assert_eq!(config.circuit_breaker.failure_threshold, 5);
        assert_eq!(config.circuit_breaker.success_threshold, 3);
        assert_eq!(config.circuit_breaker.open_timeout_secs, 30);
        assert_eq!(config.log_format(), LogFormat::Text);
        assert!(config.model_catalog.refresh_interval_secs.is_none());
    }

    #[test]
    fn missing_proxy_pool_section_is_an_error() {
        let error = from_str("upstream: {}").expect_err("proxy_pool is required");
        assert!(error.to_string().contains("proxy_pool"), "{error}");
    }

    #[test]
    fn duplicate_proxy_names_are_rejected() {
        let error = from_str(
            "proxy_pool:\n  proxies:\n    - name: a\n      url: http://localhost:0000\n    - name: a\n      url: http://localhost:0001\n",
        )
        .expect_err("duplicate names must fail");
        assert!(
            error.to_string().contains("duplicate proxy name"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_proxy_urls_are_rejected() {
        let error = from_str(
            "proxy_pool:\n  proxies:\n    - name: a\n      url: http://localhost:0000\n    - name: b\n      url: http://localhost:0000\n",
        )
        .expect_err("duplicate urls must fail");
        assert!(error.to_string().contains("duplicate proxy url"), "{error}");
    }

    #[test]
    fn non_http_proxy_url_is_rejected() {
        let error =
            from_str("proxy_pool:\n  proxies:\n    - name: a\n      url: ftp://localhost:0000\n")
                .expect_err("non-http scheme must fail");
        assert!(error.to_string().contains("http(s) URL"), "{error}");
    }

    #[test]
    fn zero_threshold_is_rejected() {
        let error =
            from_str("proxy_pool: { proxies: [] }\ncircuit_breaker: { failure_threshold: 0 }\n")
                .expect_err("zero threshold must fail");
        assert!(error.to_string().contains("failure_threshold"), "{error}");
    }

    #[test]
    fn unknown_log_format_is_rejected() {
        let error = from_str("proxy_pool: { proxies: [] }\nlog: { format: fancy }\n")
            .expect_err("unknown log format must fail");
        assert!(error.to_string().contains("log.format"), "{error}");
    }
}
