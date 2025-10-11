use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Level {
    pub px: String,
    pub sz: String,
    pub n: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OrderBook {
    pub levels: Vec<Vec<Level>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Delta {
    pub coin: String,
    pub usdc: String,
    #[serde(rename = "szi")]
    pub szi: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct L2Book {
    pub coin: String,
    pub time: u64,
    pub levels: Vec<Vec<Level>>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(non_snake_case)]
pub struct CandleSnapshot {
    pub t: u64,
    pub T: u64,
    pub s: String,
    pub i: String,
    pub o: String,
    pub c: String,
    pub h: String,
    pub l: String,
    pub v: String,
    pub n: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Universe {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u32,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Meta {
    pub universe: Vec<Universe>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserState {
    #[serde(rename = "crossMarginSummary")]
    pub cross_margin_summary: CrossMarginSummary,
    #[serde(rename = "assetPositions")]
    pub asset_positions: Vec<AssetPosition>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CrossMarginSummary {
    pub account_value: String,
    pub total_raw_usd: String,
    pub total_maint_margin: String,
    pub total_init_margin: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssetPosition {
    pub position: Position,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Position {
    pub coin: String,
    pub entry_px: Option<String>,
    pub leverage: Leverage,
    pub liquidation_px: Option<String>,
    pub margin_used: String,
    pub max_leverage: u32,
    pub position_value: String,
    pub return_on_equity: String,
    pub szi: String,
    pub unrealized_pnl: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Leverage {
    #[serde(rename = "type")]
    pub type_: String,
    pub value: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserFills {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: u64,
    pub start_position: String,
    pub dir: String,
    pub closed_pnl: String,
    pub hash: String,
    pub oid: u64,
    pub crossed: bool,
    pub fee: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FundingHistory {
    pub coin: String,
    pub time: u64,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    pub premium: String,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub usdc: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MetaAndAssetCtxs {
    pub universe: Vec<Universe>,
    #[serde(rename = "assetCtxs")]
    pub asset_ctxs: Vec<AssetCtx>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssetCtx {
    pub funding: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    #[serde(rename = "prevDayPx")]
    pub prev_day_px: String,
    #[serde(rename = "dayNtlVlm")]
    pub day_ntl_vlm: String,
    pub premium: Option<String>,
    #[serde(rename = "oraclePx")]
    pub oracle_px: String,
    #[serde(rename = "markPx")]
    pub mark_px: String,
    #[serde(rename = "midPx")]
    pub mid_px: Option<String>,
    #[serde(rename = "impactPxs")]
    pub impact_pxs: Option<Vec<String>>,
    #[serde(rename = "dayBaseVlm")]
    pub day_base_vlm: String,
}
