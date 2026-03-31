use once_cell::sync::OnceCell;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Per-exchange alias tables with bidirectional lookup capabilities loaded from `symbol_aliases.toml`.
/// The TOML structure is:
/// ```toml
/// [qfex]
/// XAU = "GOLD"
///
/// [lighter]
/// GOLD = "XAU"
/// ```
///
/// Keys are the global symbol names used throughout this project (e.g. "XAU").
/// Values are the exchange-specific base names (e.g. "GOLD").
/// All lookups are case-insensitive on the key side.
///
/// The structure provides O(1) lookups for both:
/// 1. Global -> Exchange (resolve)
/// 2. Exchange -> Global (unresolve)
#[derive(Debug, Default)]
pub struct SymbolAliases {
    /// exchange name (lowercase) -> { global_symbol (uppercase) -> exchange_base_name }
    forward: HashMap<String, HashMap<String, String>>,
    /// exchange name (lowercase) -> { exchange_base_name -> global_symbol (uppercase) }
    reverse: HashMap<String, HashMap<String, String>>,
}

/// Internal helper for deserializing the TOML structure.
#[derive(Deserialize)]
struct RawAliases(HashMap<String, HashMap<String, String>>);

impl SymbolAliases {
    /// Resolve a global symbol to an exchange-specific base name.
    /// Returns the aliased name if one exists, otherwise returns the input unchanged.
    pub fn resolve<'a>(&'a self, exchange: &str, symbol: &'a str) -> &'a str {
        self.forward
            .get(&exchange.to_lowercase())
            .and_then(|table| table.get(&symbol.to_uppercase()))
            .map(|s| s.as_str())
            .unwrap_or(symbol)
    }

    /// Unresolve an exchange-specific base name back to a global symbol.
    /// Returns the global symbol if a mapping exists, otherwise returns the input unchanged.
    pub fn unresolve<'a>(&'a self, exchange: &str, exchange_symbol: &'a str) -> &'a str {
        self.reverse
            .get(&exchange.to_lowercase())
            .and_then(|table| table.get(exchange_symbol))
            .map(|s| s.as_str())
            .unwrap_or(exchange_symbol)
    }

    /// Builds the bidirectional maps from a raw nested HashMap.
    fn from_raw(raw: HashMap<String, HashMap<String, String>>) -> Self {
        let mut forward = HashMap::new();
        let mut reverse = HashMap::new();

        for (exchange, mappings) in raw {
            let exchange_lower = exchange.to_lowercase();
            let mut f_map = HashMap::new();
            let mut r_map = HashMap::new();

            for (global, exchange_name) in mappings {
                let global_upper = global.to_uppercase();
                // Map: Global (Upper) -> Exchange (Original)
                f_map.insert(global_upper.clone(), exchange_name.clone());
                // Map: Exchange (Original) -> Global (Upper)
                r_map.insert(exchange_name, global_upper);
            }

            forward.insert(exchange_lower.clone(), f_map);
            reverse.insert(exchange_lower, r_map);
        }

        Self { forward, reverse }
    }
}

/// Custom Deserialization to transform the TOML input into Bi-Maps.
impl<'de> serde::Deserialize<'de> for SymbolAliases {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawAliases::deserialize(deserializer)?;
        Ok(SymbolAliases::from_raw(raw.0))
    }
}

static ALIASES: OnceCell<SymbolAliases> = OnceCell::new();

/// Load symbol aliases from a TOML file and store them globally.
pub fn init_aliases(path: &Path) {
    let aliases = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                "symbol_aliases.toml not found at {:?}, using empty alias map",
                path
            );
            SymbolAliases::default()
        }
        Err(e) => {
            tracing::warn!("Failed to read symbol_aliases.toml at {:?}: {}", path, e);
            SymbolAliases::default()
        }
        Ok(contents) => match toml::from_str::<SymbolAliases>(&contents) {
            Ok(a) => {
                tracing::info!("Loaded symbol aliases from {:?}", path);
                a
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to parse symbol_aliases.toml: {}, using empty map",
                    e
                );
                SymbolAliases::default()
            }
        },
    };
    let _ = ALIASES.set(aliases);
}

/// Resolve a global symbol against the loaded alias map for the given exchange.
pub fn resolve_alias<'a>(exchange: &str, symbol: &'a str) -> &'a str {
    ALIASES
        .get()
        .map(|a| a.resolve(exchange, symbol))
        .unwrap_or(symbol)
}

/// Unresolve an exchange symbol back to a global symbol against the loaded alias map.
pub fn unresolve_alias<'a>(exchange: &str, exchange_symbol: &'a str) -> &'a str {
    ALIASES
        .get()
        .map(|a| a.unresolve(exchange, exchange_symbol))
        .unwrap_or(exchange_symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to simulate loading from TOML for testing
    fn setup_test_aliases() -> SymbolAliases {
        let mut raw = HashMap::new();
        let mut qfex = HashMap::new();
        qfex.insert("XAU".to_string(), "GOLD".to_string());
        raw.insert("qfex".to_string(), qfex);
        SymbolAliases::from_raw(raw)
    }

    #[test]
    fn test_resolve_known_alias() {
        let aliases = setup_test_aliases();
        assert_eq!(aliases.resolve("qfex", "XAU"), "GOLD");
        assert_eq!(aliases.resolve("qfex", "xau"), "GOLD");
    }

    #[test]
    fn test_unresolve_known_alias() {
        let aliases = setup_test_aliases();
        assert_eq!(aliases.unresolve("qfex", "GOLD"), "XAU");
    }

    #[test]
    fn test_unresolve_unknown_passthrough() {
        let aliases = setup_test_aliases();
        // Unknown symbol should return as-is
        assert_eq!(aliases.unresolve("qfex", "BTC"), "BTC");
        // Unknown exchange should return as-is
        assert_eq!(aliases.unresolve("binance", "GOLD"), "GOLD");
    }

    #[test]
    fn test_case_insensitivity_on_exchange() {
        let aliases = setup_test_aliases();
        assert_eq!(aliases.resolve("QFEX", "XAU"), "GOLD");
        assert_eq!(aliases.unresolve("QFEX", "GOLD"), "XAU");
    }
}

