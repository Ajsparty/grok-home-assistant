use crate::audio_hub::{AudioHandle, SatMsg};
use crate::pcm::{self, SAMPLE_RATE};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

pub fn hello_message(id: &str, room: &str) -> String {
    json!({ "type": "hello", "id": id, "room": room }).to_string()
}

pub fn clear_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(|kind| kind == "clear"))
        .unwrap_or(false)
}

pub async fn serve(bind: Option<String>, audio: AudioHandle, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let Some(bind) = bind else {
        let _ = shutdown.changed().await;
        return Ok(());
    };
    let listener = TcpListener::bind(&bind).await?;
    log::info!("satellites listening on {bind}");
    loop {
        if *shutdown.borrow() {
            break;
        }
        let accepted = tokio::select! {
            result = listener.accept() => result,
            _ = shutdown.changed() => break,
        };
        let (socket, peer) = match accepted {
            Ok(pair) => pair,
            Err(err) => {
                log::warn!("satellite accept failed: {err}");
                continue;
            }
        };
        let audio = audio.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(socket, peer.to_string(), audio).await {
                log::info!("satellite {peer} closed: {err:#}");
            }
        });
    }
    Ok(())
}

async fn handle(socket: tokio::net::TcpStream, peer: String, audio: AudioHandle) -> Result<()> {
    let websocket = accept_async(socket).await?;
    let (mut write, mut read) = websocket.split();
    let (tx, mut rx) = mpsc::channel::<SatMsg>(16);
    let mut id = peer.clone();
    let mut registered = false;
    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(message) = incoming else { break };
                match message? {
                    Message::Text(text) => {
                        let event: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                        match event.get("type").and_then(|kind| kind.as_str()) {
                            Some("hello") => {
                                if let Some(name) = event.get("id").and_then(|value| value.as_str()) {
                                    if registered {
                                        audio.unregister_satellite(id.clone());
                                    }
                                    id = name.to_string();
                                }
                                audio.register_satellite(id.clone(), tx.clone());
                                registered = true;
                                write.send(Message::Text(json!({"type":"hello","ok":true,"sample_rate": SAMPLE_RATE}).to_string().into())).await?;
                            }
                            Some("wake") => audio.wake_from(id.clone()),
                            Some("ping") => {
                                write.send(Message::Text(json!({"type":"pong"}).to_string().into())).await?;
                            }
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        if !registered {
                            audio.register_satellite(id.clone(), tx.clone());
                            registered = true;
                        }
                        if bytes.len() > SAMPLE_RATE as usize * 2 * 2 {
                            continue;
                        }
                        audio.mic(id.clone(), pcm::i16_from_le(&bytes));
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            outgoing = rx.recv() => {
                match outgoing {
                    Some(SatMsg::Pcm(pcm)) => {
                        write.send(Message::Binary(pcm::i16_to_le(&pcm).into())).await?;
                    }
                    Some(SatMsg::Clear) => {
                        write.send(Message::Text(json!({"type":"clear"}).to_string().into())).await?;
                    }
                    None => break,
                }
            }
        }
    }
    if registered {
        audio.unregister_satellite(id);
    }
    Ok(())
}


