use crate::announce::Announcement;
use crate::audio_hub::AudioHandle;
use crate::earcon::Earcon;
use crate::fast_path;
use crate::pcm::{self, SAMPLE_RATE};
use crate::phrase::PhraseGate;
use crate::tools::ToolHost;
use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

enum SessionEnd {
    Hangup,
    Rotate,
    Shutdown,
}

struct PendingCall {
    call_id: String,
    name: String,
    arguments: Value,
}

pub async fn run(
    host: ToolHost,
    mut mic_rx: mpsc::Receiver<Vec<i16>>,
    mut barge_rx: mpsc::Receiver<()>,
    mut announce_rx: mpsc::Receiver<Announcement>,
    audio: AudioHandle,
    wake: std::sync::Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    let mut resume_now = false;
    let mut retries = 0u8;
    let mut queued: Option<Announcement> = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        if host.settings.live().is_energy() && !resume_now {
            loop {
                if *shutdown.borrow() {
                    return Ok(());
                }
                let woke = tokio::select! {
                    _ = wake.notified() => true,
                    announcement = announce_rx.recv() => {
                        match announcement {
                            Some(announcement) => {
                                queued = Some(announcement);
                                true
                            }
                            None => return Ok(()),
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => !host.settings.live().is_energy(),
                    _ = shutdown.changed() => false,
                };
                if *shutdown.borrow() {
                    return Ok(());
                }
                if woke {
                    break;
                }
            }
        }
        resume_now = false;
        let started = Instant::now();
        host.app.set_voice_note("Connecting…");
        match run_session(
            &host,
            &mut mic_rx,
            &mut barge_rx,
            &mut announce_rx,
            &audio,
            &mut shutdown,
            queued.take(),
        )
        .await
        {
            Ok(SessionEnd::Shutdown) => return Ok(()),
            Ok(SessionEnd::Rotate) => {
                resume_now = true;
                retries = 0;
                backoff = Duration::from_secs(1);
            }
            Ok(SessionEnd::Hangup) => {
                host.app
                    .set_voice_note("The room went quiet, so the socket closed.");
                retries = 0;
                backoff = Duration::from_secs(1);
            }
            Err(err) => {
                log::warn!("voice session ended: {err:#}");
                host.app
                    .set_voice_note(&voice_failure(&err, &host.settings.xai_api_key));
                if started.elapsed() > Duration::from_secs(10) {
                    backoff = Duration::from_secs(1);
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.changed() => {}
                }
                if *shutdown.borrow() {
                    return Ok(());
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                retries += 1;
                resume_now = retries < 5;
            }
        }
    }
    Ok(())
}

async fn run_session(
    host: &ToolHost,
    mic_rx: &mut mpsc::Receiver<Vec<i16>>,
    barge_rx: &mut mpsc::Receiver<()>,
    announce_rx: &mut mpsc::Receiver<Announcement>,
    audio: &AudioHandle,
    shutdown: &mut watch::Receiver<bool>,
    queued: Option<Announcement>,
) -> Result<SessionEnd> {
    let resume = load_resume(host);
    let url = realtime_url(host, resume.as_deref())?;
    log::info!("connecting voice session for {}", host.settings.live().room);
    let request = bearer_request(&url, &host.settings.xai_api_key)?;
    let connect = tokio::time::timeout(Duration::from_secs(20), connect_async(request)).await;
    let (socket, _) = connect.map_err(|_| anyhow!("timed out connecting to the voice api"))??;
    let (mut write, mut read) = socket.split();
    write
        .send(text_message(session_update(host)))
        .await?;
    host.app.set_session_live(true);
    host.app.set_voice_connected(true);
    host.app.touch_sound();
    let _guard = SessionFlag { app: &host.app };
    if let Some(announcement) = queued {
        inject(&mut write, &announcement.text).await?;
    }
    let opening = host.settings.live();
    let mut gate = PhraseGate::new(&opening.wake_phrase, opening.follow_up());
    let mut ignore_audio = false;
    let mut assistant_speaking = false;
    let mut configured = false;
    let mut pending: Vec<PendingCall> = Vec::new();
    let mut flush_at: Option<Instant> = None;
    let mut latest_user = String::new();
    let mut last_fast = String::new();
    let mut last_fast_at = Instant::now() - Duration::from_secs(10);
    let connected_at = Instant::now();

    loop {
        if *shutdown.borrow() {
            return Ok(SessionEnd::Shutdown);
        }
        if let Some(deadline) = flush_at {
            if gate.armed() || Instant::now() >= deadline {
                flush_calls(&mut write, host, &gate, &mut pending).await?;
                flush_at = None;
            }
        }
        let idle = flush_at
            .map(|_| Duration::from_millis(40))
            .unwrap_or(Duration::from_secs(1));
        let prefs = host.settings.live();
        gate.retune(&prefs.wake_phrase, prefs.follow_up());
        publish_phrase(host, &gate);
        if prefs.is_energy()
            && flush_at.is_none()
            && pending.is_empty()
            && !assistant_speaking
            && connected_at.elapsed() > Duration::from_secs(3)
            && host.app.quiet_for() > prefs.silence()
        {
            log::info!("room is quiet, closing the voice session");
            let _ = write.send(Message::Close(None)).await;
            return Ok(SessionEnd::Hangup);
        }
        if connected_at.elapsed() > host.settings.session_rotate {
            log::info!("rotating the voice session before the two hour cap");
            let _ = write.send(Message::Close(None)).await;
            return Ok(SessionEnd::Rotate);
        }

        let step = tokio::select! {
            message = read.next() => Step::Socket(message),
            frame = mic_rx.recv() => Step::Mic(frame),
            barge = barge_rx.recv() => Step::Barge(barge),
            announcement = announce_rx.recv() => Step::Announce(announcement),
            _ = tokio::time::sleep(idle) => Step::Tick,
            _ = shutdown.changed() => Step::Shutdown,
        };
        match step {
            Step::Shutdown => {
                if *shutdown.borrow() {
                    return Ok(SessionEnd::Shutdown);
                }
            }
            Step::Tick => {}
            Step::Announce(None) | Step::Mic(None) | Step::Barge(None) => {
                return Err(anyhow!("audio channel closed"));
            }
            Step::Announce(Some(announcement)) => {
                gate.arm_for(host.settings.live().follow_up());
                publish_phrase(host, &gate);
                inject(&mut write, &announcement.text).await?;
                host.app.touch_sound();
            }
            Step::Barge(Some(())) => {
                ignore_audio = true;
                assistant_speaking = false;
                audio.clear();
                let _ = write.send(text_message(json!({"type": "response.cancel"}))).await;
            }
            Step::Mic(Some(frame)) => {
                if pcm::rms_i16(&frame) >= host.settings.live().wake_rms {
                    host.app.touch_sound();
                }
                if configured {
                    let payload = json!({
                        "type": "input_audio_buffer.append",
                        "audio": base64::engine::general_purpose::STANDARD.encode(pcm::i16_to_le(&frame)),
                    });
                    write.send(text_message(payload)).await?;
                }
            }
            Step::Socket(None) => return Err(anyhow!("voice socket closed")),
            Step::Socket(Some(Err(err))) => return Err(err.into()),
            Step::Socket(Some(Ok(Message::Close(_)))) => return Err(anyhow!("voice socket closed")),
            Step::Socket(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => {}
            Step::Socket(Some(Ok(Message::Binary(bytes)))) => {
                if !ignore_audio {
                    assistant_speaking = true;
                    host.app.touch_sound();
                    audio.play(pcm::i16_from_le(&bytes));
                }
            }
            Step::Socket(Some(Ok(Message::Text(payload)))) => {
                let event: Value = match serde_json::from_str(&payload) {
                    Ok(event) => event,
                    Err(_) => continue,
                };
                let kind = event.get("type").and_then(|value| value.as_str()).unwrap_or("");
                if let Some(id) = conversation_id(&event) {
                    save_resume(host, &id);
                    host.app.set_conversation_id(&id);
                }
                match kind {
                    "session.created" | "session.updated" => {
                        configured = true;
                        host.app.set_voice_note("");
                        log::info!("voice session ready ({})", host.settings.live().voice);
                    }
                    "response.output_audio.delta" | "response.audio.delta" => {
                        if ignore_audio {
                            continue;
                        }
                        if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(delta) {
                                assistant_speaking = true;
                                host.app.touch_sound();
                                audio.play(pcm::i16_from_le(&bytes));
                            }
                        }
                    }
                    "conversation.item.input_audio_transcription.completed"
                    | "conversation.item.input_audio_transcription.updated" => {
                        if let Some(text) = transcript_of(&event) {
                            host.app.set_transcript(&text);
                            gate.observe(&text);
                            publish_phrase(host, &gate);
                            if kind.ends_with("completed") {
                                latest_user = text.clone();
                                maybe_fast(host, audio, &gate, &text, &mut last_fast, &mut last_fast_at).await;
                            } else {
                                latest_user = text;
                            }
                        }
                    }
                    "input_audio_buffer.speech_started" if assistant_speaking => {
                        ignore_audio = true;
                        assistant_speaking = false;
                        audio.clear();
                        host.app.touch_sound();
                        let _ = write.send(text_message(json!({"type": "response.cancel"}))).await;
                    }
                    "response.function_call_arguments.done" => {
                        let name = event.get("name").and_then(|value| value.as_str()).unwrap_or("");
                        let call_id = event.get("call_id").and_then(|value| value.as_str()).unwrap_or("");
                        let arguments = event
                            .get("arguments")
                            .and_then(|value| value.as_str())
                            .and_then(|text| serde_json::from_str(text).ok())
                            .unwrap_or_else(|| json!({}));
                        pending.push(PendingCall {
                            call_id: call_id.to_string(),
                            name: name.to_string(),
                            arguments,
                        });
                    }
                    "response.done" => {
                        assistant_speaking = false;
                        ignore_audio = false;
                        if !latest_user.is_empty() {
                            maybe_fast(host, audio, &gate, &latest_user, &mut last_fast, &mut last_fast_at).await;
                        }
                        if pending.is_empty() {
                            flush_at = None;
                        } else if gate.armed() {
                            flush_calls(&mut write, host, &gate, &mut pending).await?;
                            flush_at = None;
                        } else {
                            flush_at = Some(Instant::now() + Duration::from_millis(350));
                        }
                    }
                    "response.cancelled" => {
                        assistant_speaking = false;
                        ignore_audio = false;
                        pending.clear();
                        flush_at = None;
                    }
                    "error" => log::error!("voice api error: {event}"),
                    _ => {}
                }
            }
        }
    }
}

type VoiceSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type VoiceWriter = SplitSink<VoiceSocket, Message>;

async fn flush_calls(
    write: &mut VoiceWriter,
    host: &ToolHost,
    gate: &PhraseGate,
    pending: &mut Vec<PendingCall>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let calls = std::mem::take(pending);
    let armed = gate.armed();
    for call in calls {
        let result = if armed {
            host.execute(&call.name, call.arguments).await
        } else {
            json!({
                "ok": false,
                "error": "Wake phrase not heard. Ask the household to say the wake phrase."
            })
        };
        let output = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
        write
            .send(text_message(json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": output,
                }
            })))
            .await
            .map_err(|err| anyhow!("failed to send tool result: {err}"))?;
    }
    write
        .send(text_message(json!({"type": "response.create"})))
        .await
        .map_err(|err| anyhow!("failed to continue after tools: {err}"))?;
    Ok(())
}

async fn maybe_fast(
    host: &ToolHost,
    audio: &AudioHandle,
    gate: &PhraseGate,
    text: &str,
    last_fast: &mut String,
    last_fast_at: &mut Instant,
) {
    if text.is_empty() || (last_fast == text && last_fast_at.elapsed() < Duration::from_secs(3)) {
        return;
    }
    *last_fast = text.to_string();
    *last_fast_at = Instant::now();
    if !gate.armed() {
        return;
    }
    let Some(action) = fast_path::parse(text) else {
        return;
    };
    if fast_path::is_sensitive(&action, &host.settings.confirm_match) {
        return;
    }
    let result = host.run_fast(action).await;
    if result.get("ok").and_then(|value| value.as_bool()) == Some(true) {
        audio.earcon(Earcon::Ack);
        log::info!("fast path: {result}");
    }
}

async fn inject(write: &mut VoiceWriter, text: &str) -> Result<()> {
    write
        .send(text_message(json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        })))
        .await
        .map_err(|err| anyhow!("failed to inject announcement: {err}"))?;
    write
        .send(text_message(json!({"type": "response.create"})))
        .await
        .map_err(|err| anyhow!("failed to speak announcement: {err}"))?;
    Ok(())
}

fn session_update(host: &ToolHost) -> Value {
    let people = host.app.snapshot(Vec::new());
    let people_home = people
        .get("people_home")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });
    host.app.refresh_quiet(&host.settings);
    let aliases = host.alias_map();
    let prefs = host.settings.live();
    let mut session = json!({
        "voice": prefs.voice,
        "instructions": crate::tools::instructions(
            &host.settings,
            people_home.as_deref(),
            host.app.is_quiet(),
        ),
        "turn_detection": { "type": "server_vad" },
        "tools": crate::tools::tool_definitions(&host.settings),
        "resumption": { "enabled": true },
        "audio": {
            "input": {
                "format": { "type": "audio/pcm", "rate": SAMPLE_RATE },
                "transcription": {
                    "model": "grok-transcribe",
                    "keyterms": crate::tools::keyterms(&host.settings, &aliases),
                }
            },
            "output": {
                "format": { "type": "audio/pcm", "rate": SAMPLE_RATE }
            }
        }
    });
    if let Some(language) = prefs.language() {
        session["audio"]["input"]["transcription"]["language_hint"] = json!(language);
    }
    json!({ "type": "session.update", "session": session })
}

fn realtime_url(host: &ToolHost, conversation_id: Option<&str>) -> Result<Url> {
    let mut url = Url::parse("wss://api.x.ai/v1/realtime")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("model", &host.settings.voice_model);
        if let Some(id) = conversation_id {
            pairs.append_pair("conversation_id", id);
        }
    }
    Ok(url)
}

fn bearer_request(
    url: &Url,
    api_key: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut request = url.as_str().into_client_request()?;
    let value = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&format!(
        "Bearer {api_key}"
    ))?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        value,
    );
    Ok(request)
}

fn text_message(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

fn transcript_of(event: &Value) -> Option<String> {
    event
        .get("transcript")
        .and_then(|value| value.as_str())
        .or_else(|| event.pointer("/item/content/0/transcript").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn conversation_id(event: &Value) -> Option<String> {
    event
        .pointer("/conversation/id")
        .and_then(|value| value.as_str())
        .or_else(|| event.get("conversation_id").and_then(|value| value.as_str()))
        .map(str::to_string)
}

fn resume_path(host: &ToolHost) -> std::path::PathBuf {
    host.settings.data_dir.join("conversation.json")
}

fn load_resume(host: &ToolHost) -> Option<String> {
    let text = std::fs::read_to_string(resume_path(host)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let saved = value.get("saved_at").and_then(|item| item.as_u64())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if now.saturating_sub(saved) > 25 * 60 {
        return None;
    }
    value.get("id").and_then(|item| item.as_str()).map(str::to_string)
}

fn save_resume(host: &ToolHost, id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let path = resume_path(host);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, json!({ "id": id, "saved_at": now }).to_string());
}

struct SessionFlag<'a> {
    app: &'a crate::state::AppState,
}

impl Drop for SessionFlag<'_> {
    fn drop(&mut self) {
        self.app.set_session_live(false);
        self.app.set_voice_connected(false);
        self.app.set_phrase(false, 0);
    }
}

fn publish_phrase(host: &ToolHost, gate: &PhraseGate) {
    let left = if gate.armed() { gate.remaining_secs() } else { 0 };
    host.app.set_phrase(gate.armed(), left);
}

fn voice_failure(err: &anyhow::Error, api_key: &str) -> String {
    let mut text = format!("{err:#}").replace(['\n', '\r'], " ");
    if !api_key.is_empty() {
        text = text.replace(api_key, "");
    }
    let lower = text.to_lowercase();
    if lower.contains("bearer") {
        return "The voice service rejected the connection. Check XAI_API_KEY.".into();
    }
    if lower.contains("400") {
        return "The voice service rejected the connection. Check XAI_API_KEY.".into();
    }
    if lower.contains("401") || lower.contains("403") {
        return "The voice service refused the API key.".into();
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return "The voice service did not answer in time.".into();
    }
    text.chars().take(180).collect()
}

enum Step {
    Socket(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Mic(Option<Vec<i16>>),
    Barge(Option<()>),
    Announce(Option<Announcement>),
    Tick,
    Shutdown,
}
