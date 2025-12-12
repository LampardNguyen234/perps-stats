pub mod backend;
pub mod config;
pub mod memory;
pub mod parser;

// Allow unused since these are part of the public API for future phases
#[allow(unused_imports)]
pub use backend::{RateLimitDecision, RateLimitError, RateLimiterBackend, WindowState};
pub use config::RateLimitConfig;
pub use parser::build_from_flags;

// Also export parse_json_config for public use
#[allow(unused_imports)]
pub use parser::parse_json_config;
