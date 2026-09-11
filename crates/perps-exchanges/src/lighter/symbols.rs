/// Base symbols whose Lighter market name is the literal string with `USD` appended —
/// not a base symbol plus a strippable quote suffix. Korean equities (`SKHYNIX`,
/// `SAMSUNG`) and Lighter's forex-pair markets (`GBP`, `EUR`, `AUD`, `NZD`, i.e.
/// `GBPUSD`/`EURUSD`/`AUDUSD`/`NZDUSD`) are all named this way; either the bare base or
/// the full `<base>USD` form is accepted as input and both resolve to `<base>USD`.
const LITERAL_USD_SYMBOLS: &[&str] = &["SKHYNIX", "SAMSUNG", "GBP", "EUR", "AUD", "NZD"];

/// If `candidate` is a bare base or `<base>USD` form of a `LITERAL_USD_SYMBOLS` entry,
/// returns the canonical `<base>USD` market name.
fn literal_usd_match(candidate: &str) -> Option<String> {
    LITERAL_USD_SYMBOLS.iter().find_map(|base| {
        let with_usd = format!("{base}USD");
        (candidate == *base || candidate == with_usd).then_some(with_usd)
    })
}

/// Convert user input to an exact Lighter market name. For `LITERAL_USD_SYMBOLS`
/// entries, the `USD` suffix is part of the market name itself, not a quote suffix to
/// discard (generic suffix-stripping would otherwise turn e.g. `GBPUSD` into `GBP`,
/// which is not a market Lighter actually lists).
pub(super) fn to_exchange_symbol(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    let aliased = crate::symbol_aliases::resolve_alias("lighter", &upper).to_uppercase();

    if let Some(lit) = literal_usd_match(&aliased) {
        return lit;
    }

    let base = ["-USDT", "USDT", "-USD", "USD"]
        .iter()
        .find_map(|suffix| aliased.strip_suffix(suffix))
        .unwrap_or(&aliased);
    literal_usd_match(base)
        .unwrap_or_else(|| crate::symbol_aliases::resolve_alias("lighter", base).to_string())
}

pub(super) fn to_global_symbol(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    match upper.as_str() {
        "SKHYNIXUSD" => "SKHYNIX".to_string(),
        "SAMSUNGUSD" => "SAMSUNG".to_string(),
        _ => crate::symbol_aliases::unresolve_alias("lighter", &upper).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_equities_roundtrip_and_accept_quote_suffixes() {
        for base in ["SKHYNIX", "SAMSUNG"] {
            let exchange = format!("{base}USD");
            for input in [base.to_string(), base.to_lowercase(), exchange.clone(),
                format!("{base}-USDT"), format!("{base}USDT"), format!("{base}-USD")] {
                assert_eq!(to_exchange_symbol(&input), exchange);
            }
            assert_eq!(to_exchange_symbol(&exchange), exchange);
            assert_eq!(to_global_symbol(&exchange), base);
        }
        assert_eq!(to_exchange_symbol("BTC-USDT"), "BTC");
        assert_eq!(to_exchange_symbol("ETHUSD"), "ETH");
    }

    #[test]
    fn forex_pairs_are_literal_names_not_base_plus_quote_suffix() {
        // Lighter's raw market name for these is the whole string (e.g. "GBPUSD"), not
        // "GBP" + a strippable "USD" quote suffix -- generic suffix-stripping must not
        // turn "GBPUSD" into "GBP" (no such market exists on Lighter).
        for base in ["GBP", "EUR", "AUD", "NZD"] {
            let exchange = format!("{base}USD");
            for input in [
                base.to_string(),
                base.to_lowercase(),
                exchange.clone(),
                format!("{base}-USDT"),
                format!("{base}USDT"),
            ] {
                assert_eq!(to_exchange_symbol(&input), exchange, "input={input}");
            }
            // Global symbol for these forex pairs is the full "<base>USD" form itself,
            // unlike the Korean equities (no truncation on the way back).
            assert_eq!(to_global_symbol(&exchange), exchange);
        }
    }
}
