use crate::aster::AsterClient;
use crate::binance::BinanceClient;
use crate::bybit::BybitClient;
use crate::extended::ExtendedClient;
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
        "hyperliquid" => Ok(Box::new(HyperliquidClient::new())),
        "kucoin" => {
            let client = KucoinClient::new().await?;
            Ok(Box::new(client))
        }
        "lighter" => Ok(Box::new(LighterClient::new())),
        "nado" => Ok(Box::new(NadoClient::new())),
        "pacifica" => Ok(Box::new(PacificaClient::new())),
        "paradex" => Ok(Box::new(ParadexClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: aster, binance, bybit, extended, hyperliquid, kucoin, lighter, nado, pacifica, paradex", name),
    }
}
