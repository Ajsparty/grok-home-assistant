use anyhow::{anyhow, Result};
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

pub async fn describe(api_key: &str, model: &str, jpeg: &[u8], prompt: &str) -> Result<String> {
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(jpeg);
    let response = client
        .post("https://api.x.ai/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "temperature": 0.2,
            "max_tokens": 80,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{encoded}")}}
                ]
            }]
        }))
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(|value| value.as_str())
            .unwrap_or("vision request failed")
            .to_string();
        return Err(anyhow!(message));
    }
    content_text(&body).ok_or_else(|| anyhow!("vision response had no text"))
}

fn content_text(body: &Value) -> Option<String> {
    let content = body.pointer("/choices/0/message/content")?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }
    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn short_error(err: &anyhow::Error) -> String {
    let mut text = err.to_string();
    if text.chars().count() > 180 {
        text = text.chars().take(180).collect();
    }
    text
}
