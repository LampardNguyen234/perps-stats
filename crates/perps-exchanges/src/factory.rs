use crate::aster::AsterClient;
use crate::binance::BinanceClient;
use crate::bybit::BybitClient;
use crate::hyperliquid::HyperliquidClient;
use crate::kucoin::KucoinClient;
use crate::lighter::LighterClient;
use crate::paradex::ParadexClient;
use perps_core::traits::IPerps;

/// Returns a vector of all available exchange clients.
pub fn all_exchanges() -> Vec<(String, Box<dyn IPerps + Send + Sync>)> {
    vec![
        ("aster".to_string(), Box::new(AsterClient::new())),
        ("binance".to_string(), Box::new(BinanceClient::new())),
        ("bybit".to_string(), Box::new(BybitClient::new())),
        ("hyperliquid".to_string(), Box::new(HyperliquidClient::new())),
        ("kucoin".to_string(), Box::new(KucoinClient::new())),
        ("lighter".to_string(), Box::new(LighterClient::new())),
        ("paradex".to_string(), Box::new(ParadexClient::new())),
    ]
}

/// Returns a single exchange client by name.
pub fn get_exchange(name: &str) -> anyhow::Result<Box<dyn IPerps + Send + Sync>> {
    match name.to_lowercase().as_str() {
        "aster" => Ok(Box::new(AsterClient::new())),
        "binance" => Ok(Box::new(BinanceClient::new())),
        "bybit" => Ok(Box::new(BybitClient::new())),
        "hyperliquid" => Ok(Box::new(HyperliquidClient::new())),
        "kucoin" => Ok(Box::new(KucoinClient::new())),
        "lighter" => Ok(Box::new(LighterClient::new())),
        "paradex" => Ok(Box::new(ParadexClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: aster, binance, bybit, hyperliquid, kucoin, lighter, paradex", name),
    }
}
