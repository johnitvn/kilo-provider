//! Model catalog placeholder.
//!
//! The catalog will, in a follow-up feature PR, hold the list of models
//! exposed by the upstream gateway together with a small cache layer. For this
//! bootstrap only the storage primitive and its accessors exist — **no
//! business logic**: nothing fetches, filters or ranks models yet.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

/// One catalog entry. Kept intentionally minimal; new fields can be added
/// without breaking in-memory consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Stable model identifier as used on the wire (e.g. `kilo/…`).
    pub id: String,
    /// Optional human-friendly name.
    pub display_name: Option<String>,
}

/// Placeholder cache state for catalog snapshots.
#[derive(Debug)]
struct CacheState {
    /// Location the snapshot came from (upstream path or config key).
    source: String,
}

/// In-memory catalog storage with a cache placeholder.
#[derive(Debug)]
pub struct ModelCatalog {
    entries: RwLock<Arc<HashMap<String, CatalogEntry>>>,
    cache: RwLock<Option<CacheState>>,
}

impl ModelCatalog {
    /// Empty catalog; the bootstrap never populates it automatically.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Arc::new(HashMap::new())),
            cache: RwLock::new(None),
        }
    }

    /// Replace the whole entry set atomically (single config-style swap, same
    /// pattern as the proxy pool).
    pub fn replace(&self, entries: Vec<CatalogEntry>) {
        let map = entries
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect::<HashMap<_, _>>();
        *self
            .entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(map);
    }

    /// Cheap snapshot of the current entry set.
    pub fn snapshot(&self) -> Arc<HashMap<String, CatalogEntry>> {
        self.entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Look up a single entry by id.
    pub fn get(&self, id: &str) -> Option<CatalogEntry> {
        self.snapshot().get(id).cloned()
    }

    /// Number of entries currently stored (used by `/readyz`).
    pub fn entry_count(&self) -> usize {
        self.snapshot().len()
    }

    /// Stamp the cache placeholder. The real cache (TTL, invalidation) is
    /// planned for the feature PR; this only records provenance so plumbing
    /// exists.
    pub fn stamp_cache(&self, source: impl Into<String>) {
        *self
            .cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CacheState {
            source: source.into(),
        });
    }

    /// Provenance recorded by the last [`ModelCatalog::stamp_cache`] call, if
    /// any. The rest of the cache state (TTL, invalidation) comes with the
    /// feature PR.
    pub fn cache_source(&self) -> Option<String> {
        self.cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|cache| cache.source.clone())
    }

    // TODO(feature PR): refresh/loading logic, cache TTL handling and any
    // upstream interaction. Nothing else belongs in this bootstrap.
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_is_atomic_and_lookup_works() {
        let catalog = ModelCatalog::new();
        assert_eq!(catalog.entry_count(), 0);

        catalog.replace(vec![
            CatalogEntry {
                id: "model-a".to_string(),
                display_name: Some("Model A".to_string()),
            },
            CatalogEntry {
                id: "model-b".to_string(),
                display_name: None,
            },
        ]);
        assert_eq!(catalog.entry_count(), 2);
        assert_eq!(
            catalog.get("model-a"),
            Some(CatalogEntry {
                id: "model-a".to_string(),
                display_name: Some("Model A".to_string()),
            })
        );
        assert!(catalog.get("missing").is_none());

        // A second replace fully swaps the old set.
        catalog.replace(vec![CatalogEntry {
            id: "model-c".to_string(),
            display_name: None,
        }]);
        assert_eq!(catalog.entry_count(), 1);
        assert!(catalog.get("model-a").is_none());
    }

    #[test]
    fn cache_stamp_records_provenance() {
        let catalog = ModelCatalog::new();
        assert!(catalog.cache_source().is_none());
        catalog.stamp_cache("upstream://v1/models");
        assert_eq!(
            catalog.cache_source().as_deref(),
            Some("upstream://v1/models")
        );
    }
}
