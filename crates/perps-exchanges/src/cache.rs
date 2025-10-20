use once_cell::sync::OnceCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A thread-safe cache for storing supported symbols
/// Uses OnceCell for one-time initialization and RwLock for concurrent reads
#[derive(Clone)]
pub struct SymbolsCache {
    inner: Arc<OnceCell<RwLock<HashSet<String>>>>,
}

/// A thread-safe cache for storing contract information indexed by symbol
/// Uses OnceCell for one-time initialization and RwLock for concurrent reads
/// Generic over contract type T to support exchange-specific contract structures
#[derive(Clone)]
pub struct ContractCache<T: Clone> {
    inner: Arc<OnceCell<RwLock<HashMap<String, T>>>>,
}

impl SymbolsCache {
    /// Create a new empty cache
    pub fn new() -> Self {
        Self {
            inner: Arc::new(OnceCell::new()),
        }
    }

    /// Initialize the cache with a set of symbols
    /// If the cache is already initialized, this is a no-op
    pub fn initialize(&self, symbols: HashSet<String>) {
        let _ = self.inner.set(RwLock::new(symbols));
    }

    /// Check if a symbol is supported (returns false if cache not initialized)
    pub async fn contains(&self, symbol: &str) -> bool {
        if let Some(cache) = self.inner.get() {
            cache.read().await.contains(symbol)
        } else {
            false
        }
    }

    /// Get the cache, initializing it if needed using the provided function
    pub async fn get_or_init<F, Fut>(&self, init_fn: F) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<HashSet<String>>>,
    {
        if self.inner.get().is_some() {
            return Ok(());
        }

        let symbols = init_fn().await?;
        self.initialize(symbols);
        Ok(())
    }

    /// Check if the cache is initialized
    pub fn is_initialized(&self) -> bool {
        self.inner.get().is_some()
    }
}

impl Default for SymbolsCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_initialization() {
        let cache = SymbolsCache::new();
        assert!(!cache.is_initialized());

        let symbols = vec!["BTC-USDT".to_string(), "ETH-USDT".to_string()]
            .into_iter()
            .collect();
        cache.initialize(symbols);

        assert!(cache.is_initialized());
        assert!(cache.contains("BTC-USDT").await);
        assert!(cache.contains("ETH-USDT").await);
        assert!(!cache.contains("XRP-USDT").await);
    }

    #[tokio::test]
    async fn test_cache_get_or_init() {
        let cache = SymbolsCache::new();

        cache
            .get_or_init(|| async {
                Ok(vec!["BTC-USDT".to_string(), "ETH-USDT".to_string()]
                    .into_iter()
                    .collect())
            })
            .await
            .unwrap();

        assert!(cache.is_initialized());
        assert!(cache.contains("BTC-USDT").await);
    }
}

impl<T: Clone> ContractCache<T> {
    /// Create a new empty cache
    pub fn new() -> Self {
        Self {
            inner: Arc::new(OnceCell::new()),
        }
    }

    /// Initialize the cache with a map of symbol -> contract
    /// If the cache is already initialized, this is a no-op
    pub fn initialize(&self, contracts: HashMap<String, T>) {
        let _ = self.inner.set(RwLock::new(contracts));
    }

    /// Get contract info for a symbol (returns None if not found or cache not initialized)
    pub async fn get(&self, symbol: &str) -> Option<T> {
        if let Some(cache) = self.inner.get() {
            cache.read().await.get(symbol).cloned()
        } else {
            None
        }
    }

    /// Check if a symbol exists in the cache
    pub async fn contains(&self, symbol: &str) -> bool {
        if let Some(cache) = self.inner.get() {
            cache.read().await.contains_key(symbol)
        } else {
            false
        }
    }

    /// Get the cache, initializing it if needed using the provided function
    pub async fn get_or_init<F, Fut>(&self, init_fn: F) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<HashMap<String, T>>>,
    {
        if self.inner.get().is_some() {
            return Ok(());
        }

        let contracts = init_fn().await?;
        self.initialize(contracts);
        Ok(())
    }

    /// Check if the cache is initialized
    pub fn is_initialized(&self) -> bool {
        self.inner.get().is_some()
    }

    /// Get all symbols in the cache
    pub async fn get_all_symbols(&self) -> Vec<String> {
        if let Some(cache) = self.inner.get() {
            cache.read().await.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }
}

impl<T: Clone> Default for ContractCache<T> {
    fn default() -> Self {
        Self::new()
    }
}
