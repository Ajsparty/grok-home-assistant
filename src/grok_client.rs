use crate::config::Settings;
use anyhow::Result;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use log::{error, info, warn};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

pub struct GrokVoiceClient {
    settings: Settings,
}

impl GrokVoiceClient {
    pub async fn new(settings: Settings) -> Result<Self> {
        Ok(Self { settings })
    }

    pub async fn run(&mut self) -> Result<()> {
        let url = Url::parse("wss://api.x.ai/v1/realtime?model=grok-voice-latest")?;
        info!("Connecting to Grok Voice Agent API...");

        let (ws_stream, _) = connect_async(url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Configure session
        let session_update = json!({
            "type": "session.update",
            "session": {
                "voice": self.settings.voice,
                "instructions": "You are a helpful, concise home voice assistant. Control smart home devices when requested. Confirm actions briefly.",
                "turn_detection": { "type": "server_vad" },
                "tools": [{ "type": "web_search" }]
            }
        });

        write
            .send(Message::Text(session_update.to_string().into()))
            .await?;

        info!("Session configured. Listening for events...");

        // In a full implementation, spawn tasks for:
        // - Microphone capture → input_audio_buffer.append
        // - Playing response.output_audio.delta chunks
        // For now, we demonstrate the connection and event loop.

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) {
                        let event_type = event["type"].as_str().unwrap_or("");
                        match event_type {
                            "response.output_audio.delta" => {
                                if let Some(delta) = event["delta"].as_str() {
                                    // Decode and play audio
                                    if let Ok(audio) = base64::engine::general_purpose::STANDARD
                                        .decode(delta)
                                    {
                                        info!("Received audio chunk: {} bytes", audio.len());
                                        // TODO: play via cpal
                                    }
                                }
                            }
                            "response.output_audio_transcript.delta" => {
                                if let Some(delta) = event["delta"].as_str() {
                                    print!("{}", delta);
                                }
                            }
                            "error" => {
                                error!("API error: {}", event);
                            }
                            _ => {
                                // Other events (session.created, etc.)
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("Connection closed");
                    break;
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}
