use crate::aster::AsterClient;
use crate::binance::BinanceClient;
use crate::bybit::BybitClient;
use crate::extended::ExtendedClient;
use crate::hyperliquid::HyperliquidClient;
use crate::kucoin::KucoinClient;
use crate::lighter::LighterClient;
use crate::pacifica::PacificaClient;
use crate::paradex::ParadexClient;
use perps_core::traits::IPerps;

/// Returns a vector of all available exchange clients.
/// For Binance, automatically enables WebSocket streaming if DATABASE_URL is set.
/// This function is async because Binance client initialization requires async operations.
pub async fn all_exchanges() -> Vec<(String, Box<dyn IPerps + Send + Sync>)> {
    let mut exchanges = vec![
        ("aster".to_string(), Box::new(AsterClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("bybit".to_string(), Box::new(BybitClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("extended".to_string(), Box::new(ExtendedClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("hyperliquid".to_string(), Box::new(HyperliquidClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("kucoin".to_string(), Box::new(KucoinClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("lighter".to_string(), Box::new(LighterClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("pacifica".to_string(), Box::new(PacificaClient::new()) as Box<dyn IPerps + Send + Sync>),
        ("paradex".to_string(), Box::new(ParadexClient::new()) as Box<dyn IPerps + Send + Sync>),
    ];

    // Binance requires async initialization
    if let Ok(binance_client) = BinanceClient::new().await {
        exchanges.push(("binance".to_string(), Box::new(binance_client) as Box<dyn IPerps + Send + Sync>));
    }

    exchanges
}

/// Returns a single exchange client by name.
/// For Binance, automatically enables WebSocket streaming if DATABASE_URL is set.
///
/// Environment variables (Binance only):
/// - `DATABASE_URL`: PostgreSQL connection string (required for streaming)
/// - `ENABLE_ORDERBOOK_STREAMING`: Enable/disable streaming (default: true if DATABASE_URL is set)
/// - `ORDERBOOK_STREAMING_SYMBOLS`: Comma-separated symbols to stream (default: "BTC,ETH")
pub async fn get_exchange(name: &str) -> anyhow::Result<Box<dyn IPerps + Send + Sync>> {
    match name.to_lowercase().as_str() {
        "aster" => Ok(Box::new(AsterClient::new())),
        "binance" => {
            let client = BinanceClient::new().await?;
            Ok(Box::new(client))
        }
        "bybit" => Ok(Box::new(BybitClient::new())),
        "extended" => Ok(Box::new(ExtendedClient::new())),
        "hyperliquid" => Ok(Box::new(HyperliquidClient::new())),
        "kucoin" => Ok(Box::new(KucoinClient::new())),
        "lighter" => Ok(Box::new(LighterClient::new())),
        "pacifica" => Ok(Box::new(PacificaClient::new())),
        "paradex" => Ok(Box::new(ParadexClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: aster, binance, bybit, extended, hyperliquid, kucoin, lighter, pacifica, paradex", name),
    }
}
