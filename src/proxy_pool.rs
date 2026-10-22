//! Proxy pool plumbing.
//!
//! The pool holds the proxy list that was loaded from the single configuration
//! file and can atomically swap its contents with [`ProxyPool::reload`] when
//! the file changes on disk. This is **config plumbing only**: no rotation or
//! selection strategy lives here yet (see the stub at the end of this file).

use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use thiserror::Error;

use crate::config::{Config, ConfigError, ProxyConfig};

/// A single proxy endpoint resolved from the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub name: String,
    pub url: String,
}

impl Proxy {
    pub fn from_config(proxy: &ProxyConfig) -> Self {
        Self {
            name: proxy.name.clone(),
            url: proxy.url.clone(),
        }
    }
}

/// Errors surfaced by pool operations.
#[derive(Debug, Error)]
pub enum PoolError {
    #[error("proxy pool reload failed: {0}")]
    Config(#[from] ConfigError),
    #[error("proxy pool lock is poisoned")]
    Poisoned,
}

/// Shared, reloadable view of the configured proxies.
///
/// Contents live behind a `RwLock<Arc<...>>` so readers grab a cheap
/// `Arc` snapshot and reloads swap the whole set atomically — a reader either
/// sees the old or the new set, never a mixture.
#[derive(Debug)]
pub struct ProxyPool {
    proxies: RwLock<Arc<Vec<Proxy>>>,
}

impl ProxyPool {
    /// Build the pool from an already validated [`Config`].
    pub fn from_config(config: &Config) -> Self {
        let proxies = config
            .proxy_pool
            .proxies
            .iter()
            .map(Proxy::from_config)
            .collect::<Vec<_>>();
        Self {
            proxies: RwLock::new(Arc::new(proxies)),
        }
    }

    /// Snapshot of the current proxy list. Never blocks long: reload swaps an
    /// `Arc` pointer rather than mutating entries in place.
    pub fn snapshot(&self) -> Arc<Vec<Proxy>> {
        self.proxies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Number of configured proxies.
    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    /// Always `false` for now — an empty pool is allowed (rotation keeps the
    /// placeholder semantics until the follow-up PR lands).
    pub fn is_empty(&self) -> bool {
        self.snapshot().is_empty()
    }

    /// Re-read the same configuration file and swap the pool contents
    /// atomically.
    ///
    /// The config file is tiny, so a short blocking read inside the async fn
    /// is acceptable here (documented trade-off; spawn_blocking can be added
    /// if this ever grows). Returns the freshly loaded [`Config`] so callers
    /// can, in the future, swap other sections (upstream URL, thresholds) at
    /// the same time.
    pub async fn reload(&self, path: &Path) -> Result<Config, PoolError> {
        let fresh = crate::config::load(path)?;
        let proxies = fresh
            .proxy_pool
            .proxies
            .iter()
            .map(Proxy::from_config)
            .collect::<Vec<_>>();
        let mut guard = self.proxies.write().map_err(|_| PoolError::Poisoned)?;
        *guard = Arc::new(proxies);
        Ok(fresh)
    }

    // TODO(feature PR): rotation / selection strategy over `snapshot()`, e.g.
    //
    //   pub fn acquire(&self) -> Option<Proxy> { todo!("planned in the follow-up rotation feature PR") }
    //
    // Deliberately NOT implemented in this bootstrap: no round-robin, no
    // least-error scoring, no health filtering. Keep the interface in mind
    // (returning an owned Proxy) when designing it.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Minimal on-disk fixture without adding a tempfile dependency.
    fn scratch_file(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "kilo-provider-pool-test-{}-{tag}.yaml",
            std::process::id()
        ));
        path
    }

    fn write_config(path: &Path, count: usize) {
        let bodies = (0..count)
            .map(|index| format!("    - name: p{index}\n      url: http://localhost:{index:04}\n"))
            .collect::<String>();
        std::fs::write(path, format!("proxy_pool:\n  proxies:\n{bodies}"))
            .expect("fixture write must succeed");
    }

    #[tokio::test]
    async fn reload_swaps_contents_atomically() {
        let path = scratch_file("reload");
        write_config(&path, 2);
        // Do not leak the fixture if a test fails midway.
        let _guard = RemoveOnDrop(path.clone());

        let initial = crate::config::load(&path).expect("initial config must load");
        let pool = ProxyPool::from_config(&initial);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.snapshot()[1].name, "p1");

        // Rewrite the same file and re-read it through the pool API.
        write_config(&path, 3);
        let fresh = pool.reload(&path).await.expect("reload must succeed");
        assert_eq!(fresh.proxy_pool.proxies.len(), 3);
        assert_eq!(pool.len(), 3);

        // Snapshot remains cheap and consistent after several reloads.
        assert_eq!(pool.snapshot().last().expect("non-empty").name, "p2");
    }

    #[tokio::test]
    async fn reload_surfaces_config_errors() {
        let path = scratch_file("broken");
        let _guard = RemoveOnDrop(path.clone());
        std::fs::write(&path, "not: a: valid: pool\n").expect("fixture write must succeed");

        let pool = ProxyPool::from_config(
            &crate::config::from_str("proxy_pool: { proxies: [] }").expect("valid"),
        );
        let error = pool.reload(&path).await.expect_err("broken file must fail");
        assert!(matches!(error, PoolError::Config(_)), "{error}");
    }

    /// Best-effort cleanup wrapper for the scratch config files.
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
