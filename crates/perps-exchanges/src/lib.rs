// This crate contains exchange-specific implementations of the IPerps trait.
// Each exchange has its own module.

pub mod binance;

// Re-export the core trait
pub use perps_core::IPerps;

// Re-export exchange clients
pub use binance::BinanceClient;
