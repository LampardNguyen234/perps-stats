use thiserror::Error;

#[derive(Debug, Error)]
pub enum BinanceError {
    #[error("Binance SDK error: {0}")]
    SdkError(String),

    #[error("Data conversion error: {0}")]
    ConversionError(String),

    #[error("Symbol not supported: {0}")]
    SymbolNotSupported(String),

    #[error("Invalid response from Binance API")]
    InvalidResponse,

    #[error("API error: {0}")]
    ApiError(String),
}

// Note: BinanceError implements std::error::Error via thiserror::Error,
// so it automatically converts to anyhow::Error via anyhow's blanket impl
