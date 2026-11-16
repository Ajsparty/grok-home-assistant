mod config;
mod grok_client;

use anyhow::Result;
use log::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::init();

    info!("Starting Grok Home Voice Assistant (Rust)");

    let settings = config::Settings::from_env()?;
    info!("Voice: {}", settings.voice);

    let mut client = grok_client::GrokVoiceClient::new(settings).await?;
    client.run().await?;

    Ok(())
}
