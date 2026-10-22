//! HTTP wiring: routes and shared application state.
//!
//! The bootstrap exposes only wellness endpoints:
//!
//! * `GET /healthz` — liveness, always `200` while the process runs.
//! * `GET /readyz`  — readiness, reports the loaded configuration/proxy pool
//!   status as JSON so operators can see what the gateway is wired to.
//!
//! No business routes (inference, catalog retrieval, …) exist yet.

use std::sync::Arc;

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;

use crate::{
    breaker::CircuitBreaker, catalog::ModelCatalog, config::Config, proxy_pool::ProxyPool,
    upstream::UpstreamClient,
};

/// State shared by all handlers. Everything is `Arc`ed so cloning the state
/// per-request is cheap and pooled resources stay singletons.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: Arc<ProxyPool>,
    pub catalog: Arc<ModelCatalog>,
    pub breaker: Arc<CircuitBreaker>,
    pub upstream: Arc<UpstreamClient>,
}

impl AppState {
    /// Build shared state from already-validated configuration.
    pub fn new(config: Arc<Config>) -> Result<Self, crate::upstream::UpstreamError> {
        let upstream = UpstreamClient::for_config(Some(config.upstream_base_url()))?;
        Ok(Self {
            upstream: Arc::new(upstream),
            pool: Arc::new(ProxyPool::from_config(&config)),
            catalog: Arc::new(ModelCatalog::new()),
            breaker: Arc::new(CircuitBreaker::new(config.circuit_breaker.clone())),
            config,
        })
    }
}

/// Build the axum router with all bootstrap routes.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// `GET /healthz` — liveness probe. The process is alive whenever this
/// handler runs; nothing else is checked on purpose.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// `GET /readyz` — readiness probe. Reports the current configuration/pool
/// status; a `200` with descriptive JSON is the bootstrap contract.
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, axum::Json(readiness_json(&state)))
}

/// Pure snapshot builder — kept free of handler machinery so it can be
/// tested without spinning up HTTP.
fn readiness_json(state: &AppState) -> serde_json::Value {
    let proxies = state
        .pool
        .snapshot()
        .iter()
        .map(|proxy| {
            json!({
                "name": proxy.name,
                "url": proxy.url,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "status": "ready",
        "upstream": {
            "base_url": state.upstream.base_url(),
        },
        "proxy_pool": {
            "count": proxies.len(),
            "proxies": proxies,
        },
        "model_catalog": {
            "entries": state.catalog.entry_count(),
        },
        "circuit_breaker": state.breaker.snapshot(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> AppState {
        let config = Arc::new(
            crate::config::from_str(
                "proxy_pool:\n  proxies:\n    - name: a\n      url: http://localhost:0000\n",
            )
            .expect("test config must parse"),
        );
        AppState::new(config).expect("state must build")
    }

    #[test]
    fn readiness_json_reflects_pool_and_upstream() {
        let state = test_state();
        let body = readiness_json(&state);
        assert_eq!(body["status"], "ready");
        assert_eq!(
            body["upstream"]["base_url"],
            "https://api.kilo.ai/api/gateway"
        );
        assert_eq!(body["proxy_pool"]["count"], 1);
        assert_eq!(body["proxy_pool"]["proxies"][0]["name"], "a");
        assert_eq!(body["model_catalog"]["entries"], 0);
        assert_eq!(body["circuit_breaker"]["state"], "closed");
    }
}
