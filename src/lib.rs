//! # kilo-provider
//!
//! A gateway in front of the upstream Kilo AI API gateway
//! (`https://api.kilo.ai/api/gateway`, accessed **anonymously**).
//!
//! This bootstrap crate contains configuration plumbing and module skeletons
//! only: proxy pool primitives, a model catalog placeholder, a circuit-breaker
//! skeleton and the HTTP wellness routes (`/healthz`, `/readyz`). There is
//! deliberately **no inference or business logic** yet; follow-up feature PRs
//! will build on the interfaces exposed here.
//!
//! ## Invariants respected across the crate
//!
//! * **No timeouts.** No overall request timeout, no read timeout, no connect
//!   timeout is configured anywhere. Long-lived streaming responses are the
//!   primary use case and must never be aborted by a timer.
//! * **No credentials for upstream.** Upstream requests are anonymous; an
//!   `Authorization` header is never attached (see [`upstream`]).
//! * **`PORT` is required.** A missing or unparseable `PORT` env var is a fatal
//!   startup error with a clear log line; there is no silent fallback.

pub mod breaker;
pub mod catalog;
pub mod config;
pub mod error;
pub mod proxy_pool;
pub mod router;
pub mod upstream;
