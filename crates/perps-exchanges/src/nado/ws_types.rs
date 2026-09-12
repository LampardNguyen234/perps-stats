use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct NadoWsSubscribeRequest {
    pub id: u64,
    pub method: &'static str,
    pub stream: NadoWsStream,
}

#[derive(Debug, Clone, Serialize)]
pub struct NadoWsStream {
    #[serde(rename = "type")]
    pub stream_type: &'static str,
    pub product_id: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NadoWsPingRequest {
    pub id: u64,
    pub method: &'static str,
    pub client_time: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NadoWsControlResponse {
    pub id: u64,
    pub result: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NadoWsBookDepth {
    #[serde(rename = "type")]
    pub stream_type: String,
    pub product_id: u32,
    pub min_timestamp: String,
    pub max_timestamp: String,
    pub last_max_timestamp: String,
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}
