//! Minimal HTTP-facing error type shared across handlers.
//!
//! Any handler may return `Result<_, AppError>`; it converts into a JSON
//! error body with a sensible status. Keep it minimal — richer per-module
//! errors can wrap these via `From`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    /// Misconfiguration detected at request time (e.g. a stale reload).
    #[error("configuration error: {0}")]
    Configuration(String),

    /// Unexpected internal failure.
    #[error("internal server error: {0}")]
    Internal(String),

    /// The caller sent something invalid.
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Configuration(_) | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Configuration/internal failures are operator-visible problems; log
        // them so they appear in the structured (JSON) trace stream.
        if !matches!(self, AppError::BadRequest(_)) {
            tracing::error!(error = %self, status = status.as_u16(), "request failed");
        }
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}
