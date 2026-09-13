pub mod client;
pub mod conversions;
pub mod types;
pub mod ws_client;
pub mod ws_types;

pub(crate) const GATEWAY_URL: &str = "https://gateway.prod.nado.xyz/v2";
/// Legacy gateway REST base for `/query` (market_liquidity, etc.) - this is `/v1`, not `/v2`
/// like `GATEWAY_URL` above; Nado's docs list them as separate base paths.
pub(crate) const GATEWAY_QUERY_URL: &str = "https://gateway.prod.nado.xyz/v1";

pub use client::NadoClient;
pub use ws_client::NadoWsClient;
