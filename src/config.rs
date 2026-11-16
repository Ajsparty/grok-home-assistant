use anyhow::Result;
use std::env;

#[derive(Debug, Clone)]
pub struct Settings {
    pub xai_api_key: String,
    pub voice: String,
    pub ha_url: Option<String>,
    pub ha_token: Option<String>,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            xai_api_key: env::var("XAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("XAI_API_KEY is required"))?,
            voice: env::var("VOICE").unwrap_or_else(|_| "eve".to_string()),
            ha_url: env::var("HA_URL").ok(),
            ha_token: env::var("HA_TOKEN").ok(),
        })
    }
}
