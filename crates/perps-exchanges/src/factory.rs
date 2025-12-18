use crate::aster::AsterClient;
use crate::binance::BinanceClient;
use crate::bybit::BybitClient;
use crate::extended::ExtendedClient;
use crate::gravity::GravityClient;
use crate::hyperliquid::HyperliquidClient;
use crate::kucoin::KucoinClient;
use crate::lighter::LighterClient;
use crate::nado::NadoClient;
use crate::pacifica::PacificaClient;
use crate::paradex::ParadexClient;
use perps_core::traits::IPerps;

/// Returns a vector of all available exchange clients.
/// For Aster, Binance, Extended, and KuCoin, automatically enables WebSocket streaming if DATABASE_URL is set.
/// This function is async because Aster, Binance, Extended, and KuCoin client initialization requires async operations.
pub async fn all_exchanges() -> Vec<(String, Box<dyn IPerps + Send + Sync>)> {
    let mut exchanges = vec![
        (
            "bybit".to_string(),
            Box::new(BybitClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "gravity".to_string(),
            Box::new(GravityClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "hyperliquid".to_string(),
            Box::new(HyperliquidClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "lighter".to_string(),
            Box::new(LighterClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "nado".to_string(),
            Box::new(NadoClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "pacifica".to_string(),
            Box::new(PacificaClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
        (
            "paradex".to_string(),
            Box::new(ParadexClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
    ];

    // Aster requires async initialization
    if let Ok(aster_client) = AsterClient::new().await {
        exchanges.push((
            "aster".to_string(),
            Box::new(aster_client) as Box<dyn IPerps + Send + Sync>,
        ));
    }

    // Binance requires async initialization
    if let Ok(binance_client) = BinanceClient::new().await {
        exchanges.push((
            "binance".to_string(),
            Box::new(binance_client) as Box<dyn IPerps + Send + Sync>,
        ));
    }

    // Extended requires async initialization
    if let Ok(extended_client) = ExtendedClient::new().await {
        exchanges.push((
            "extended".to_string(),
            Box::new(extended_client) as Box<dyn IPerps + Send + Sync>,
        ));
    }

    // KuCoin requires async initialization
    if let Ok(kucoin_client) = KucoinClient::new().await {
        exchanges.push((
            "kucoin".to_string(),
            Box::new(kucoin_client) as Box<dyn IPerps + Send + Sync>,
        ));
    }

    exchanges
}

/// Returns a single exchange client by name.
/// For Aster, Binance, Extended, Pacifica, and KuCoin, WebSocket streaming is disabled by default.
///
/// Environment variables (Aster, Binance, Extended, Pacifica, KuCoin):
/// - `DATABASE_URL`: PostgreSQL connection string (required for streaming)
/// - `ENABLE_ORDERBOOK_STREAMING`: Set to "true" to enable streaming (default: false)
pub async fn get_exchange(name: &str) -> anyhow::Result<Box<dyn IPerps + Send + Sync>> {
    match name.to_lowercase().as_str() {
        "aster" => {
            let client = AsterClient::new().await?;
            Ok(Box::new(client))
        }
        "binance" => {
            let client = BinanceClient::new().await?;
            Ok(Box::new(client))
        }
        "bybit" => Ok(Box::new(BybitClient::new())),
        "extended" => {
            let client = ExtendedClient::new().await?;
            Ok(Box::new(client))
        }
        "gravity" => Ok(Box::new(GravityClient::new())),
        "hyperliquid" => Ok(Box::new(HyperliquidClient::new())),
        "kucoin" => {
            let client = KucoinClient::new().await?;
            Ok(Box::new(client))
        }
        "lighter" => Ok(Box::new(LighterClient::new())),
        "nado" => Ok(Box::new(NadoClient::new())),
        "pacifica" => Ok(Box::new(PacificaClient::new())),
        "paradex" => Ok(Box::new(ParadexClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: aster, binance, bybit, extended, gravity, hyperliquid, kucoin, lighter, nado, pacifica, paradex", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that Gravity exchange can be created via factory
    #[tokio::test]
    async fn test_get_exchange_gravity() {
        let result = get_exchange("gravity").await;
        assert!(result.is_ok(), "get_exchange(\"gravity\") should succeed");

        let client = result.unwrap();
        assert_eq!(client.get_name(), "gravity");
    }

    /// Test that Gravity exchange respects case-insensitive exchange names
    #[tokio::test]
    async fn test_get_exchange_gravity_case_insensitive() {
        let lowercase = get_exchange("gravity").await;
        let uppercase = get_exchange("GRAVITY").await;
        let mixedcase = get_exchange("Gravity").await;

        assert!(lowercase.is_ok(), "lowercase 'gravity' should work");
        assert!(uppercase.is_ok(), "uppercase 'GRAVITY' should work");
        assert!(mixedcase.is_ok(), "mixedcase 'Gravity' should work");

        assert_eq!(lowercase.unwrap().get_name(), "gravity");
        assert_eq!(uppercase.unwrap().get_name(), "gravity");
        assert_eq!(mixedcase.unwrap().get_name(), "gravity");
    }

    /// Test that Gravity is included in all_exchanges()
    #[tokio::test]
    async fn test_all_exchanges_includes_gravity() {
        let exchanges = all_exchanges().await;
        let gravity_found = exchanges.iter().any(|(name, _)| name == "gravity");
        assert!(
            gravity_found,
            "Gravity should be included in all_exchanges()"
        );
    }

    /// Test that Gravity in all_exchanges has correct name
    #[tokio::test]
    async fn test_gravity_in_all_exchanges_has_correct_name() {
        let exchanges = all_exchanges().await;
        for (name, client) in exchanges {
            if name == "gravity" {
                assert_eq!(client.get_name(), "gravity");
                return;
            }
        }
        panic!("Gravity not found in all_exchanges()");
    }

    /// Test that unknown exchange returns error
    #[tokio::test]
    async fn test_get_exchange_unknown() {
        let result = get_exchange("nonexistent_exchange_xyz").await;
        assert!(result.is_err(), "Unknown exchange should return error");
    }

    /// Test that at least one exchange is available
    #[tokio::test]
    async fn test_all_exchanges_not_empty() {
        let exchanges = all_exchanges().await;
        assert!(
            !exchanges.is_empty(),
            "all_exchanges() should return at least one exchange"
        );
    }

    /// Test that factory has Bybit and Gravity as guaranteed exchanges
    #[tokio::test]
    async fn test_guaranteed_exchanges_present() {
        let exchanges = all_exchanges().await;
        let names: Vec<_> = exchanges.iter().map(|(n, _)| n.as_str()).collect();

        assert!(
            names.contains(&"bybit"),
            "bybit should be in all_exchanges()"
        );
        assert!(
            names.contains(&"gravity"),
            "gravity should be in all_exchanges()"
        );
    }
}
