use crate::binance::BinanceClient;
use crate::bybit::BybitClient;
use crate::kucoin::KucoinClient;
use crate::lighter::LighterClient;
use crate::paradex::ParadexClient;
use perps_core::traits::IPerps;

/// Returns a vector of all available exchange clients.
pub fn all_exchanges() -> Vec<(String, Box<dyn IPerps + Send + Sync>)> {
    vec![
        ("binance".to_string(), Box::new(BinanceClient::new())),
        ("bybit".to_string(), Box::new(BybitClient::new())),
        ("kucoin".to_string(), Box::new(KucoinClient::new())),
        ("lighter".to_string(), Box::new(LighterClient::new())),
        ("paradex".to_string(), Box::new(ParadexClient::new())),
    ]
}

/// Returns a single exchange client by name.
pub fn get_exchange(name: &str) -> anyhow::Result<Box<dyn IPerps + Send + Sync>> {
    match name.to_lowercase().as_str() {
        "binance" => Ok(Box::new(BinanceClient::new())),
        "bybit" => Ok(Box::new(BybitClient::new())),
        "kucoin" => Ok(Box::new(KucoinClient::new())),
        "lighter" => Ok(Box::new(LighterClient::new())),
        "paradex" => Ok(Box::new(ParadexClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: binance, bybit, kucoin, lighter, paradex", name),
    }
}
