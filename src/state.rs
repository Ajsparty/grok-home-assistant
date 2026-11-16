use crate::config::{in_quiet_hours, Settings};
use chrono::Local;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Mutex<Inner>>,
}

struct Inner {
    voice_connected: bool,
    session_live: bool,
    conversation_id: Option<String>,
    room: String,
    mic_rms: f32,
    last_tool: Option<String>,
    last_tool_at: Option<String>,
    people_home: Option<Vec<String>>,
    quiet_hours: bool,
    pending_confirmation: Option<String>,
    active_source: String,
    last_transcript: Option<String>,
    recent_events: VecDeque<String>,
    voice_note: String,
    phrase_armed: bool,
    follow_left_secs: u64,
    satellites: Vec<String>,
    doorbell_caption: String,
    last_sound: Instant,
    connected_since: Option<Instant>,
    started: Instant,
}

impl AppState {
    pub fn new(room: impl Into<String>) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(Inner {
                voice_connected: false,
                session_live: false,
                conversation_id: None,
                room: room.into(),
                mic_rms: 0.0,
                last_tool: None,
                last_tool_at: None,
                people_home: None,
                quiet_hours: false,
                pending_confirmation: None,
                active_source: "local".to_string(),
                last_transcript: None,
                recent_events: VecDeque::new(),
                voice_note: String::new(),
                phrase_armed: false,
                follow_left_secs: 0,
                satellites: Vec::new(),
                doorbell_caption: String::new(),
                last_sound: Instant::now(),
                connected_since: None,
                started: Instant::now(),
            })),
        }
    }

    pub fn set_session_live(&self, live: bool) {
        self.lock().session_live = live;
    }

    pub fn session_live(&self) -> bool {
        self.lock().session_live
    }

    pub fn set_voice_connected(&self, connected: bool) {
        let mut guard = self.lock();
        guard.voice_connected = connected;
        guard.connected_since = if connected { Some(Instant::now()) } else { None };
    }

    pub fn set_conversation_id(&self, id: &str) {
        self.lock().conversation_id = Some(id.to_string());
    }

    pub fn set_room(&self, room: &str) {
        self.lock().room = room.to_string();
    }

    pub fn set_mic_rms(&self, rms: f32) {
        self.lock().mic_rms = rms;
    }

    pub fn set_active_source(&self, source: &str) {
        self.lock().active_source = source.to_string();
    }

    pub fn set_transcript(&self, text: &str) {
        self.lock().last_transcript = Some(text.to_string());
    }

    pub fn set_last_tool(&self, name: &str) {
        let mut guard = self.lock();
        guard.last_tool = Some(name.to_string());
        guard.last_tool_at = Some(Local::now().format("%H:%M:%S").to_string());
    }

    pub fn set_people(&self, people: Option<Vec<String>>) {
        self.lock().people_home = people;
    }

    pub fn nobody_home(&self) -> bool {
        matches!(&self.lock().people_home, Some(people) if people.is_empty())
    }

    pub fn set_voice_note(&self, note: &str) {
        let mut text: String = note.chars().take(180).collect();
        text = text.replace('\n', " ");
        self.lock().voice_note = text;
    }

    pub fn set_phrase(&self, armed: bool, follow_left_secs: u64) {
        let mut guard = self.lock();
        guard.phrase_armed = armed;
        guard.follow_left_secs = follow_left_secs;
    }

    pub fn set_satellites(&self, names: Vec<String>) {
        self.lock().satellites = names;
    }

    pub fn set_doorbell(&self, caption: &str) {
        self.lock().doorbell_caption = caption.chars().take(240).collect();
    }

    pub fn set_pending(&self, summary: Option<String>) {
        self.lock().pending_confirmation = summary;
    }

    pub fn push_event(&self, text: &str) {
        let mut guard = self.lock();
        guard
            .recent_events
            .push_front(format!("{} {text}", Local::now().format("%H:%M")));
        while guard.recent_events.len() > 30 {
            guard.recent_events.pop_back();
        }
    }

    pub fn recent_events(&self) -> Vec<String> {
        self.lock().recent_events.iter().cloned().collect()
    }

    pub fn touch_sound(&self) {
        self.lock().last_sound = Instant::now();
    }

    pub fn quiet_for(&self) -> Duration {
        self.lock().last_sound.elapsed()
    }

    pub fn refresh_quiet(&self, settings: &Settings) {
        let prefs = settings.live();
        let quiet = in_quiet_hours(
            Local::now().time(),
            prefs.quiet_start_time(),
            prefs.quiet_end_time(),
            prefs.quiet_hours,
        );
        self.lock().quiet_hours = quiet;
    }

    pub fn is_quiet(&self) -> bool {
        self.lock().quiet_hours
    }

    pub fn snapshot(&self, timers: Vec<Value>) -> Value {
        let guard = self.lock();
        json!({
            "status": "ok",
            "uptime_secs": guard.started.elapsed().as_secs(),
            "voice_connected": guard.voice_connected,
            "session_secs": guard.connected_since.map(|since| since.elapsed().as_secs()),
            "conversation_id": guard.conversation_id,
            "room": guard.room,
            "active_source": guard.active_source,
            "mic_rms": (guard.mic_rms * 1000.0).round() / 1000.0,
            "people_home": guard.people_home,
            "quiet_hours": guard.quiet_hours,
            "pending_confirmation": guard.pending_confirmation,
            "last_tool": guard.last_tool,
            "last_tool_at": guard.last_tool_at,
            "last_transcript": guard.last_transcript,
            "recent_events": guard.recent_events,
            "voice_note": guard.voice_note,
            "phrase_armed": guard.phrase_armed,
            "follow_left_secs": guard.follow_left_secs,
            "satellites": guard.satellites,
            "doorbell_caption": guard.doorbell_caption,
            "timers": timers,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}
