use anyhow::Result;
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeMode {
    Energy,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioOutput {
    Auto,
    Local,
    Satellite,
    Both,
}

/// Settings the dashboard can change. Secrets stay in the environment.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VoicePrefs {
    pub voice: String,
    pub language_hint: String,
    pub room: String,
    pub wake_mode: String,
    pub wake_phrase: String,
    pub wake_rms: f32,
    pub wake_hold_ms: u64,
    pub silence_secs: u64,
    pub follow_up_secs: u64,
    pub barge_in: bool,
    pub barge_rms: f32,
    pub echo_delay_ms: u64,
    pub echo_gain: f32,
    pub audio_output: String,
    pub quiet_hours: bool,
    pub quiet_start: String,
    pub quiet_end: String,
    pub announce_when_empty: bool,
    pub enable_web_search: bool,
    pub enable_x_search: bool,
}

impl Default for VoicePrefs {
    fn default() -> Self {
        Self {
            voice: "eve".into(),
            language_hint: String::new(),
            room: "living room".into(),
            wake_mode: "energy".into(),
            wake_phrase: "hey grok".into(),
            wake_rms: 0.05,
            wake_hold_ms: 250,
            silence_secs: 8,
            follow_up_secs: 20,
            barge_in: true,
            barge_rms: 0.12,
            echo_delay_ms: 80,
            echo_gain: 0.3,
            audio_output: "auto".into(),
            quiet_hours: true,
            quiet_start: "22:00".into(),
            quiet_end: "07:00".into(),
            announce_when_empty: false,
            enable_web_search: true,
            enable_x_search: false,
        }
    }
}

impl VoicePrefs {
    pub fn from_process_env() -> Self {
        Self {
            voice: env_string("VOICE", "eve"),
            language_hint: optional_env("LANGUAGE_HINT").unwrap_or_default(),
            room: env_string("ROOM", "living room"),
            wake_mode: env_string("WAKE_MODE", "energy"),
            wake_phrase: match env::var("WAKE_PHRASE") {
                Ok(value) => value,
                Err(_) => "hey grok".into(),
            },
            wake_rms: env_f32("WAKE_RMS", 0.05),
            wake_hold_ms: env_parse("WAKE_HOLD_MS", 250),
            silence_secs: env_parse("SILENCE_SECS", 8),
            follow_up_secs: env_parse("FOLLOW_UP_SECS", 20),
            barge_in: env_bool("BARGE_IN", true),
            barge_rms: env_f32("BARGE_RMS", 0.12),
            echo_delay_ms: env_parse("ECHO_DELAY_MS", 80),
            echo_gain: env_f32("ECHO_GAIN", 0.3),
            audio_output: env_string("AUDIO_OUTPUT", "auto"),
            quiet_hours: env_bool("QUIET_HOURS", true),
            quiet_start: env_string("QUIET_START", "22:00"),
            quiet_end: env_string("QUIET_END", "07:00"),
            announce_when_empty: env_bool("ANNOUNCE_WHEN_EMPTY", false),
            enable_web_search: env_bool("ENABLE_WEB_SEARCH", true),
            enable_x_search: env_bool("ENABLE_X_SEARCH", false),
        }
    }

    pub fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn sanitized(self) -> Self {
        let wake_mode = match parse_wake_mode(&self.wake_mode) {
            WakeMode::Always => "always",
            WakeMode::Energy => "energy",
        };
        let audio_output = match parse_audio_output(&self.audio_output) {
            AudioOutput::Local => "local",
            AudioOutput::Satellite => "satellite",
            AudioOutput::Both => "both",
            AudioOutput::Auto => "auto",
        };
        let room = {
            let trimmed = self.room.trim();
            if trimmed.is_empty() {
                "living room".to_string()
            } else {
                trimmed.chars().take(80).collect()
            }
        };
        let voice = {
            let trimmed = self.voice.trim();
            if trimmed.is_empty() {
                "eve".to_string()
            } else {
                trimmed.chars().take(64).collect()
            }
        };
        Self {
            voice,
            language_hint: self.language_hint.trim().chars().take(16).collect(),
            room,
            wake_mode: wake_mode.into(),
            wake_phrase: crate::textutil::normalize(&self.wake_phrase),
            wake_rms: self.wake_rms.clamp(0.005, 1.0),
            wake_hold_ms: self.wake_hold_ms.clamp(50, 2_000),
            silence_secs: self.silence_secs.clamp(2, 120),
            follow_up_secs: self.follow_up_secs.clamp(1, 180),
            barge_in: self.barge_in,
            barge_rms: self.barge_rms.clamp(0.01, 1.0),
            echo_delay_ms: self.echo_delay_ms.min(400),
            echo_gain: self.echo_gain.clamp(0.0, 1.5),
            audio_output: audio_output.into(),
            quiet_hours: self.quiet_hours,
            quiet_start: normalize_hhmm(&self.quiet_start, "22:00"),
            quiet_end: normalize_hhmm(&self.quiet_end, "07:00"),
            announce_when_empty: self.announce_when_empty,
            enable_web_search: self.enable_web_search,
            enable_x_search: self.enable_x_search,
        }
    }

    pub fn is_energy(&self) -> bool {
        self.wake_mode_kind() == WakeMode::Energy
    }

    pub fn wake_mode_kind(&self) -> WakeMode {
        parse_wake_mode(&self.wake_mode)
    }

    pub fn audio_output_kind(&self) -> AudioOutput {
        parse_audio_output(&self.audio_output)
    }

    pub fn echo_delay_samples(&self) -> usize {
        (crate::pcm::SAMPLE_RATE as u64 * self.echo_delay_ms / 1000) as usize
    }

    pub fn silence(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.silence_secs)
    }

    pub fn follow_up(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.follow_up_secs)
    }

    pub fn wake_hold(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.wake_hold_ms)
    }

    pub fn language(&self) -> Option<String> {
        let hint = self.language_hint.trim();
        if hint.is_empty() {
            None
        } else {
            Some(hint.to_string())
        }
    }

    pub fn quiet_start_time(&self) -> NaiveTime {
        parse_hhmm(&self.quiet_start, 22, 0)
    }

    pub fn quiet_end_time(&self) -> NaiveTime {
        parse_hhmm(&self.quiet_end, 7, 0)
    }
}

#[derive(Clone)]
pub struct Settings {
    pub xai_api_key: String,
    pub voice: String,
    pub voice_model: String,
    pub vision_model: String,
    pub language_hint: Option<String>,
    pub room: String,
    pub ha_url: Option<String>,
    pub ha_token: Option<String>,
    pub ha_mcp_url: Option<String>,
    pub ha_mcp_token: Option<String>,
    pub ha_mcp_tools: Vec<String>,
    pub collection_id: Option<String>,
    pub enable_web_search: bool,
    pub enable_x_search: bool,
    pub wake_mode: WakeMode,
    pub wake_phrase: String,
    pub wake_rms: f32,
    pub wake_hold: std::time::Duration,
    pub silence: std::time::Duration,
    pub follow_up: std::time::Duration,
    pub session_rotate: std::time::Duration,
    pub audio_output: AudioOutput,
    pub echo_delay_samples: usize,
    pub echo_gain: f32,
    pub barge_in: bool,
    pub barge_rms: f32,
    pub quiet_hours: bool,
    pub quiet_start: NaiveTime,
    pub quiet_end: NaiveTime,
    pub announce_when_empty: bool,
    pub alert_poll: std::time::Duration,
    pub alert_entities: Vec<String>,
    pub alert_match: Vec<String>,
    pub critical_match: Vec<String>,
    pub doorbell_entities: Vec<String>,
    pub doorbell_camera: Option<String>,
    pub weather_entity: Option<String>,
    pub calendar_entity: Option<String>,
    pub media_entity: Option<String>,
    pub todo_entity: Option<String>,
    pub confirm_domains: Vec<String>,
    pub confirm_match: Vec<String>,
    pub health_bind: String,
    pub satellite_bind: Option<String>,
    pub log_dir: PathBuf,
    pub data_dir: PathBuf,
    pub aliases_file: PathBuf,
    pub voice_prefs: Arc<Mutex<VoicePrefs>>,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        let health_port: u16 = env_parse("HEALTH_PORT", 8080);
        let health_host = env_string("HEALTH_BIND", "0.0.0.0");
        let satellite_port: u16 = env_parse("SATELLITE_PORT", 8765);
        let satellite_host = env_string("SATELLITE_BIND", "0.0.0.0");
        let data_dir = PathBuf::from(env_string("DATA_DIR", "data"));
        let mut prefs = VoicePrefs::from_process_env().sanitized();
        if let Some(saved) = VoicePrefs::read(&data_dir.join("dashboard.json")) {
            prefs = saved.sanitized();
        }
        Ok(Self {
            xai_api_key: env::var("XAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("XAI_API_KEY is required"))?,
            voice: prefs.voice.clone(),
            voice_model: env_string("VOICE_MODEL", "grok-voice-latest"),
            vision_model: env_string("XAI_VISION_MODEL", "grok-4"),
            language_hint: prefs.language(),
            room: prefs.room.clone(),
            ha_url: configured_env("HA_URL"),
            ha_token: configured_env("HA_TOKEN"),
            ha_mcp_url: configured_env("HA_MCP_URL"),
            ha_mcp_token: configured_env("HA_MCP_TOKEN"),
            ha_mcp_tools: csv_or("HA_MCP_ALLOWED_TOOLS", &[]),
            collection_id: configured_env("COLLECTION_ID"),
            enable_web_search: prefs.enable_web_search,
            enable_x_search: prefs.enable_x_search,
            wake_mode: prefs.wake_mode_kind(),
            wake_phrase: prefs.wake_phrase.clone(),
            wake_rms: prefs.wake_rms,
            wake_hold: prefs.wake_hold(),
            silence: prefs.silence(),
            follow_up: prefs.follow_up(),
            session_rotate: std::time::Duration::from_secs(
                env_parse::<u64>("SESSION_ROTATE_MINS", 110).clamp(5, 119) * 60,
            ),
            audio_output: prefs.audio_output_kind(),
            echo_delay_samples: prefs.echo_delay_samples(),
            echo_gain: prefs.echo_gain,
            barge_in: prefs.barge_in,
            barge_rms: prefs.barge_rms,
            quiet_hours: prefs.quiet_hours,
            quiet_start: prefs.quiet_start_time(),
            quiet_end: prefs.quiet_end_time(),
            announce_when_empty: prefs.announce_when_empty,
            alert_poll: std::time::Duration::from_secs(env_parse::<u64>("ALERT_POLL_SECS", 5).max(2)),
            alert_entities: csv_or("ALERT_ENTITIES", &[]),
            alert_match: csv_or(
                "ALERT_MATCH",
                &[
                    "doorbell",
                    "leak",
                    "smoke",
                    "carbon_monoxide",
                    "moisture",
                    "water_leak",
                ],
            ),
            critical_match: csv_or(
                "CRITICAL_MATCH",
                &["smoke", "carbon_monoxide", "leak", "moisture", "gas"],
            ),
            doorbell_entities: csv_or("DOORBELL_ENTITIES", &[]),
            doorbell_camera: optional_env("DOORBELL_CAMERA"),
            weather_entity: optional_env("WEATHER_ENTITY"),
            calendar_entity: optional_env("CALENDAR_ENTITY"),
            media_entity: optional_env("MEDIA_ENTITY"),
            todo_entity: optional_env("TODO_ENTITY"),
            confirm_domains: csv_or("CONFIRM_DOMAINS", &["lock", "alarm_control_panel"]),
            confirm_match: csv_or(
                "CONFIRM_MATCH",
                &["garage_door", "front_door", "unlock", "disarm"],
            ),
            health_bind: if health_port == 0 {
                String::new()
            } else {
                format!("{health_host}:{health_port}")
            },
            satellite_bind: if satellite_port == 0 {
                None
            } else {
                Some(format!("{satellite_host}:{satellite_port}"))
            },
            log_dir: PathBuf::from(env_string("LOG_DIR", "logs")),
            data_dir,
            aliases_file: PathBuf::from(env_string("ALIASES_FILE", "aliases.json")),
            voice_prefs: Arc::new(Mutex::new(prefs)),
        })
    }

    pub fn live(&self) -> VoicePrefs {
        self.voice_prefs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn apply_json(&self, patch: &Value) -> Result<VoicePrefs, String> {
        let mut current = serde_json::to_value(self.live()).map_err(|err| err.to_string())?;
        let Value::Object(incoming) = patch else {
            return Err("settings must be a JSON object".into());
        };
        let Value::Object(base) = &mut current else {
            return Err("could not read the current settings".into());
        };
        for (key, value) in incoming {
            base.insert(key.clone(), value.clone());
        }
        let prefs: VoicePrefs = serde_json::from_value(current).map_err(|err| err.to_string())?;
        let prefs = prefs.sanitized();
        *self
            .voice_prefs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = prefs.clone();
        self.store_prefs(&prefs)?;
        Ok(prefs)
    }

    fn store_prefs(&self, prefs: &VoicePrefs) -> Result<(), String> {
        let path = self.data_dir.join("dashboard.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let body = serde_json::to_vec_pretty(prefs).map_err(|err| err.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(|err| err.to_string())?;
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path).map_err(|err| err.to_string())
    }
}

pub fn in_quiet_hours(now: NaiveTime, start: NaiveTime, end: NaiveTime, enabled: bool) -> bool {
    if !enabled || start == end {
        return false;
    }
    if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

fn parse_wake_mode(value: &str) -> WakeMode {
    match value.to_lowercase().as_str() {
        "always" | "continuous" => WakeMode::Always,
        _ => WakeMode::Energy,
    }
}

fn parse_audio_output(value: &str) -> AudioOutput {
    match value.to_lowercase().as_str() {
        "local" => AudioOutput::Local,
        "satellite" => AudioOutput::Satellite,
        "both" => AudioOutput::Both,
        _ => AudioOutput::Auto,
    }
}

fn normalize_hhmm(value: &str, fallback: &str) -> String {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(value.trim(), "%H:%M:%S"))
        .map(|time| time.format("%H:%M").to_string())
        .unwrap_or_else(|_| fallback.to_string())
}

fn parse_hhmm(value: &str, hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::parse_from_str(value, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M:%S"))
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(hour, minute, 0).expect("valid time"))
}

fn env_string(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn optional_env(key: &str) -> Option<String> {
    env::var(key).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn configured_env(key: &str) -> Option<String> {
    optional_env(key).filter(|value| !value.starts_with("your_") && !value.contains("your_"))
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn env_f32(key: &str, default: f32) -> f32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn csv_or(key: &str, default: &[&str]) -> Vec<String> {
    match env::var(key) {
        Ok(value) => value
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect(),
        Err(_) => default.iter().map(|part| (*part).to_string()).collect(),
    }
}

#[cfg(test)]
impl Settings {
    pub(crate) fn fixture() -> Self {
        let prefs = VoicePrefs {
            room: "office".into(),
            ..VoicePrefs::default()
        };
        Self {
            xai_api_key: "test".into(),
            voice: prefs.voice.clone(),
            voice_model: "grok-voice-latest".into(),
            vision_model: "grok-4".into(),
            language_hint: prefs.language(),
            room: prefs.room.clone(),
            ha_url: None,
            ha_token: None,
            ha_mcp_url: None,
            ha_mcp_token: None,
            ha_mcp_tools: Vec::new(),
            collection_id: None,
            enable_web_search: true,
            enable_x_search: false,
            wake_mode: WakeMode::Energy,
            wake_phrase: "hey grok".into(),
            wake_rms: 0.05,
            wake_hold: std::time::Duration::from_millis(250),
            silence: std::time::Duration::from_secs(8),
            follow_up: std::time::Duration::from_secs(20),
            session_rotate: std::time::Duration::from_secs(110 * 60),
            audio_output: AudioOutput::Auto,
            echo_delay_samples: 1920,
            echo_gain: 0.3,
            barge_in: true,
            barge_rms: 0.12,
            quiet_hours: true,
            quiet_start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            quiet_end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            announce_when_empty: false,
            alert_poll: std::time::Duration::from_secs(5),
            alert_entities: Vec::new(),
            alert_match: Vec::new(),
            critical_match: Vec::new(),
            doorbell_entities: Vec::new(),
            doorbell_camera: None,
            weather_entity: None,
            calendar_entity: None,
            media_entity: None,
            todo_entity: None,
            confirm_domains: vec!["lock".into()],
            confirm_match: vec!["garage_door".into()],
            health_bind: "127.0.0.1:8080".into(),
            satellite_bind: None,
            log_dir: PathBuf::from("logs"),
            data_dir: PathBuf::from("data"),
            aliases_file: PathBuf::from("aliases.json"),
            voice_prefs: Arc::new(Mutex::new(prefs)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_prefs_clamp_the_wake_gate() {
        let mut prefs = VoicePrefs::default();
        prefs.wake_rms = 9.0;
        prefs.wake_phrase = "Hey, Grok!".into();
        prefs.silence_secs = 1;
        let prefs = prefs.sanitized();
        assert_eq!(prefs.wake_rms, 1.0);
        assert_eq!(prefs.wake_phrase, "hey grok");
        assert_eq!(prefs.silence_secs, 2);
    }

    #[test]
    fn overnight_quiet_hours() {
        let start = NaiveTime::from_hms_opt(22, 0, 0).unwrap();
        let end = NaiveTime::from_hms_opt(7, 0, 0).unwrap();
        assert!(in_quiet_hours(
            NaiveTime::from_hms_opt(23, 30, 0).unwrap(),
            start,
            end,
            true
        ));
        assert!(in_quiet_hours(
            NaiveTime::from_hms_opt(6, 15, 0).unwrap(),
            start,
            end,
            true
        ));
        assert!(!in_quiet_hours(
            NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            start,
            end,
            true
        ));
        assert!(!in_quiet_hours(
            NaiveTime::from_hms_opt(23, 0, 0).unwrap(),
            start,
            end,
            false
        ));
    }
}
