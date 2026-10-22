//! `kilo-provider` binary entrypoint.
//!
//! Startup sequence:
//!
//! 1. Load the configuration file (`CONFIG_PATH`, default `config.yaml`).
//!    Failures here are reported to stderr *before* tracing is available.
//! 2. Initialize structured tracing (JSON when configured, env-filtered,
//!    default level `info`).
//! 3. Parse the **required** `PORT` env var — a missing or invalid value is a
//!    fatal error with a clear log line; there is no default port.
//! 4. Build shared state, bind `0.0.0.0:{PORT}` and serve until a shutdown
//!    signal (`SIGINT`/`SIGTERM`) arrives.

use std::sync::Arc;

use thiserror::Error;
use tokio::net::TcpListener;

use kilo_provider::{config, router::AppState};

/// Default configuration file location, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "config.yaml";

/// Fatal startup errors. Each one is already reported through the best
/// available channel (stderr before tracing init, tracing afterwards); the
/// empty struct just signals `exit(1)`.
#[derive(Debug, Error)]
#[error("startup failed, see previously reported error")]
pub struct FatalError;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build a tokio runtime");
    if runtime.block_on(run()).is_err() {
        std::process::exit(1);
    }
}

async fn run() -> Result<(), FatalError> {
    // ---- 1. Configuration (single YAML file) ------------------------------
    let config_path =
        std::env::var("CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let config = match config::load(std::path::Path::new(&config_path)) {
        Ok(config) => config,
        Err(error) => {
            // tracing is not up yet — stderr is the clear error log here.
            eprintln!(
                "fatal: failed to load configuration from '{}': {error}",
                config_path
            );
            return Err(FatalError);
        }
    };

    // ---- 2. Structured tracing -------------------------------------------
    init_tracing(&config);
    tracing::info!(
        config_path = %config_path,
        "configuration loaded"
    );

    // ---- 3. Required PORT (no default, no silent fallback) ---------------
    let port = match parse_port(std::env::var("PORT").ok().as_deref()) {
        Ok(port) => port,
        Err(error) => {
            tracing::error!(
                error = %error,
                "fatal: the PORT environment variable is required and must be a valid port \
                 in 1..=65535; refusing to start without it (no default port is used)"
            );
            return Err(FatalError);
        }
    };

    // ---- 4. Shared state, listener, graceful serve ------------------------
    let state = AppState::new(Arc::new(config)).map_err(|error| {
        tracing::error!(error = %error, "fatal: failed to initialize upstream client");
        FatalError
    })?;

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await.map_err(|error| {
        tracing::error!(error = %error, %addr, "fatal: failed to bind listener");
        FatalError
    })?;
    tracing::info!(addr = %listener.local_addr().expect("bound listener has a local addr"), "kilo-provider listening");

    let app = kilo_provider::router::build_router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "fatal: server terminated with an error");
            FatalError
        })?;

    tracing::info!("shutdown complete");
    Ok(())
}

/// Resolve to a future that completes on SIGINT/SIGTERM (SIGINT via ctrl_c).
/// Allows a clean drain of in-flight streaming responses before exit.
async fn shutdown_signal() {
    let sigterm = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            match signal(SignalKind::terminate()) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "unable to install SIGTERM handler");
                    std::future::pending::<()>().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            std::future::pending::<()>().await;
        }
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, draining connections");
        }
        _ = sigterm => {
            tracing::info!("SIGTERM received, draining connections");
        }
    }
}

/// Parse the `PORT` environment variable. Strict by design:
///
/// * missing → error (never fall back to a default port),
/// * not a `u16` → error,
/// * `0` → error (reserved for OS-assigned ports; a fixed port is wanted).
pub fn parse_port(raw: Option<&str>) -> Result<u16, PortError> {
    let value = raw.ok_or(PortError::Missing)?.to_string();
    let port = value
        .trim()
        .parse::<u16>()
        .map_err(|_| PortError::Invalid {
            value: value.clone(),
            reason: "not a 16-bit unsigned integer".to_string(),
        })?;
    if port == 0 {
        return Err(PortError::Invalid {
            value,
            reason: "port 0 is reserved and cannot be used as a fixed port".to_string(),
        });
    }
    Ok(port)
}

/// PORT parse failures, rendered as clear operator-facing messages.
#[derive(Debug, Error)]
pub enum PortError {
    #[error(
        "PORT is not set. kilo-provider requires the PORT environment variable \
         (a port in 1..=65535); there is no default port"
    )]
    Missing,
    #[error("PORT={value:?} is not a valid port in 1..=65535: {reason}")]
    Invalid { value: String, reason: String },
}

/// Initialize the global tracing subscriber:
///
/// * filter: `RUST_LOG` when set, otherwise `config.log.level`, otherwise
///   `info`;
/// * format: JSON when `config.log.format == "json"` (structured/collectible),
///   human-readable text otherwise.
fn init_tracing(config: &config::Config) {
    let filter = match (
        tracing_subscriber::EnvFilter::try_from_default_env(),
        config.log.level.as_deref(),
    ) {
        (Ok(filter), _) => filter,
        (Err(_), Some(level)) => tracing_subscriber::EnvFilter::new(level),
        (Err(_), None) => tracing_subscriber::EnvFilter::new("info"),
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);
    match config.log_format() {
        config::LogFormat::Json => builder.json().init(),
        config::LogFormat::Text => builder.init(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_missing_is_an_error() {
        assert!(matches!(parse_port(None), Err(PortError::Missing)));
    }

    #[test]
    fn port_must_parse_as_u16() {
        for bad in ["", "  ", "abc", "-1", "70000", "1 2"] {
            let error = parse_port(Some(bad));
            assert!(
                matches!(error, Err(PortError::Invalid { .. })),
                "{bad}: {error:?}"
            );
        }
    }

    #[test]
    fn port_zero_is_rejected() {
        let error = parse_port(Some("0"));
        assert!(matches!(error, Err(PortError::Invalid { .. })), "{error:?}");
    }

    #[test]
    fn valid_ports_parse() {
        assert_eq!(parse_port(Some("8123")).expect("valid"), 8123);
        assert_eq!(parse_port(Some("1")).expect("valid"), 1);
        assert_eq!(parse_port(Some("65535")).expect("valid"), 65535);
        assert_eq!(parse_port(Some(" 8123 ")).expect("trimmed"), 8123);
    }

    #[tokio::test]
    async fn fatal_error_render_is_clear() {
        // The Display implementation must be enough on its own for operators.
        assert!(!PortError::Missing.to_string().is_empty());
        assert!(!FatalError.to_string().is_empty());
    }
}
