//! Unit tests for `parse_symbol` / `normalize_symbol` on every exchange client.
//!
//! Each exchange must satisfy:
//! 1. `parse_symbol` converts a global symbol (e.g. "BTC") to exchange-specific format.
//! 2. `parse_symbol` is idempotent — passing the exchange-specific format returns the same format.
//! 3. `normalize_symbol` inverts `parse_symbol` — the round-trip returns the original global symbol.
//! 4. Alias mappings are applied (e.g. "XAU" → "GOLD" on qfex, back to "XAU" via `normalize_symbol`).
//!
//! These are pure unit tests (no network calls).

use perps_core::IPerps;
use perps_exchanges::{
    AsterClient, BybitClient, ExtendedClient, GravityClient, HibachiClient, HotstuffClient,
    HyperliquidClient, KucoinClient, LighterClient, NadoClient, O1Client, PacificaClient,
    ParadexClient, QfexClient,
};

// ---------------------------------------------------------------------------
// Binance
// ---------------------------------------------------------------------------
mod binance {
    use perps_core::IPerps;
    use perps_exchanges::BinanceClient;

    fn client() -> BinanceClient {
        BinanceClient::new_rest_only()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC-USDT");
        assert_eq!(c.parse_symbol("ETH"), "ETH-USDT");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC-USDT"), "BTC-USDT");
        assert_eq!(c.parse_symbol("BTCUSDT"), "BTC-USDT");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC-USDT"), "BTC");
        assert_eq!(c.normalize_symbol("BTCUSDT"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-USDT"), "ETH");
    }

    #[test]
    fn alias_copper_roundtrip() {
        // [binance] XCU = "COPPER" — requires init_aliases
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = client();
        assert_eq!(c.parse_symbol("XCU"), "COPPER-USDT");
        assert_eq!(c.normalize_symbol("COPPER-USDT"), "XCU");
    }
}

// ---------------------------------------------------------------------------
// Bybit
// ---------------------------------------------------------------------------
mod bybit {
    use super::*;

    fn client() -> BybitClient {
        BybitClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTCUSDT");
        assert_eq!(c.parse_symbol("ETH"), "ETHUSDT");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTCUSDT"), "BTCUSDT");
        assert_eq!(c.parse_symbol("ETHUSDT"), "ETHUSDT");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTCUSDT"), "BTC");
        assert_eq!(c.normalize_symbol("ETHUSDT"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Hyperliquid
// ---------------------------------------------------------------------------
mod hyperliquid {
    use super::*;

    fn client() -> HyperliquidClient {
        HyperliquidClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC");
        assert_eq!(c.parse_symbol("ETH"), "ETH");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC"), "BTC");
        assert_eq!(c.normalize_symbol("ETH"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// KuCoin
// ---------------------------------------------------------------------------
mod kucoin {
    use super::*;

    fn client() -> KucoinClient {
        KucoinClient::new_rest_only()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "XBTUSDTM");
        assert_eq!(c.parse_symbol("ETH"), "ETHUSDTM");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("XBTUSDTM"), "XBTUSDTM");
        assert_eq!(c.parse_symbol("ETHUSDTM"), "ETHUSDTM");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("XBTUSDTM"), "BTC");
        assert_eq!(c.normalize_symbol("ETHUSDTM"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Aster
// ---------------------------------------------------------------------------
mod aster {
    use super::*;

    fn client() -> AsterClient {
        AsterClient::new_rest_only()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTCUSDT");
        assert_eq!(c.parse_symbol("ETH"), "ETHUSDT");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTCUSDT"), "BTCUSDT");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTCUSDT"), "BTC");
        assert_eq!(c.normalize_symbol("ETHUSDT"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Lighter
// ---------------------------------------------------------------------------
mod lighter {
    use super::*;

    fn client() -> LighterClient {
        LighterClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC");
        assert_eq!(c.parse_symbol("ETH"), "ETH");
    }

    #[test]
    fn alias_cl_roundtrip() {
        // [lighter] CL = "WTI" — requires init_aliases
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = client();
        assert_eq!(c.parse_symbol("CL"), "WTI");
        assert_eq!(c.normalize_symbol("WTI"), "CL");
    }

    #[test]
    fn normalize_passthrough() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC"), "BTC");
        assert_eq!(c.normalize_symbol("ETH"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// QFEX
// ---------------------------------------------------------------------------
mod qfex {
    use super::*;

    #[tokio::test]
    async fn parse_global() {
        let c = QfexClient::new();
        assert_eq!(c.parse_symbol("BTC"), "BTC-USD");
        assert_eq!(c.parse_symbol("ETH"), "ETH-USD");
    }

    #[tokio::test]
    async fn parse_idempotent() {
        let c = QfexClient::new();
        assert_eq!(c.parse_symbol("BTC-USD"), "BTC-USD");
    }

    #[tokio::test]
    async fn normalize_roundtrip() {
        let c = QfexClient::new();
        assert_eq!(c.normalize_symbol("BTC-USD"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-USD"), "ETH");
    }

    #[tokio::test]
    async fn alias_xau_roundtrip() {
        // [qfex] XAU = "GOLD" — requires init_aliases
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = QfexClient::new();
        assert_eq!(c.parse_symbol("XAU"), "GOLD-USD");
        assert_eq!(c.normalize_symbol("GOLD-USD"), "XAU");
    }

    #[tokio::test]
    async fn alias_xag_roundtrip() {
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = QfexClient::new();
        assert_eq!(c.parse_symbol("XAG"), "SILVER-USD");
        assert_eq!(c.normalize_symbol("SILVER-USD"), "XAG");
    }
}

// ---------------------------------------------------------------------------
// Extended
// ---------------------------------------------------------------------------
mod extended {
    use super::*;

    fn client() -> ExtendedClient {
        ExtendedClient::new_rest_only()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC-USD");
        assert_eq!(c.parse_symbol("ETH"), "ETH-USD");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC-USD"), "BTC-USD");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC-USD"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-USD"), "ETH");
    }

    #[test]
    fn alias_googl_roundtrip() {
        // [extended] GOOGL = "GOOG" — requires init_aliases
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = client();
        assert_eq!(c.parse_symbol("GOOGL"), "GOOG-USD");
        assert_eq!(c.normalize_symbol("GOOG-USD"), "GOOGL");
    }
}

// ---------------------------------------------------------------------------
// Gravity
// ---------------------------------------------------------------------------
mod gravity {
    use super::*;

    fn client() -> GravityClient {
        GravityClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC_USDT_Perp");
        assert_eq!(c.parse_symbol("ETH"), "ETH_USDT_Perp");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC_USDT_Perp"), "BTC_USDT_Perp");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC_USDT_Perp"), "BTC");
        assert_eq!(c.normalize_symbol("ETH_USDT_Perp"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Paradex
// ---------------------------------------------------------------------------
mod paradex {
    use super::*;

    fn client() -> ParadexClient {
        ParadexClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC-USD-PERP");
        assert_eq!(c.parse_symbol("ETH"), "ETH-USD-PERP");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC-USD-PERP"), "BTC-USD-PERP");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC-USD-PERP"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-USD-PERP"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Hibachi
// ---------------------------------------------------------------------------
mod hibachi {
    use super::*;

    fn client() -> HibachiClient {
        HibachiClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC/USDT-P");
        assert_eq!(c.parse_symbol("ETH"), "ETH/USDT-P");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC/USDT-P"), "BTC/USDT-P");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC/USDT-P"), "BTC");
        assert_eq!(c.normalize_symbol("ETH/USDT-P"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Hotstuff
// ---------------------------------------------------------------------------
mod hotstuff {
    use super::*;

    fn client() -> HotstuffClient {
        HotstuffClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC-PERP");
        assert_eq!(c.parse_symbol("ETH"), "ETH-PERP");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC-PERP"), "BTC-PERP");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC-PERP"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-PERP"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Nado
// ---------------------------------------------------------------------------
mod nado {
    use super::*;

    fn client() -> NadoClient {
        NadoClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC-PERP_USDT0");
        assert_eq!(c.parse_symbol("ETH"), "ETH-PERP_USDT0");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC-PERP_USDT0"), "BTC-PERP_USDT0");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC-PERP_USDT0"), "BTC");
        assert_eq!(c.normalize_symbol("ETH-PERP_USDT0"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// O1
// ---------------------------------------------------------------------------
mod o1 {
    use super::*;

    fn client() -> O1Client {
        O1Client::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTCUSD");
        assert_eq!(c.parse_symbol("ETH"), "ETHUSD");
    }

    #[test]
    fn parse_idempotent() {
        let c = client();
        assert_eq!(c.parse_symbol("BTCUSD"), "BTCUSD");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTCUSD"), "BTC");
        assert_eq!(c.normalize_symbol("ETHUSD"), "ETH");
    }
}

// ---------------------------------------------------------------------------
// Pacifica
// ---------------------------------------------------------------------------
mod pacifica {
    use super::*;

    fn client() -> PacificaClient {
        PacificaClient::new()
    }

    #[test]
    fn parse_global() {
        let c = client();
        assert_eq!(c.parse_symbol("BTC"), "BTC");
        assert_eq!(c.parse_symbol("ETH"), "ETH");
    }

    #[test]
    fn normalize_roundtrip() {
        let c = client();
        assert_eq!(c.normalize_symbol("BTC"), "BTC");
        assert_eq!(c.normalize_symbol("ETH"), "ETH");
    }
}
