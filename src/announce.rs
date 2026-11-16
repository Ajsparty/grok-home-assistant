use crate::config::Settings;
use crate::earcon::Earcon;
use crate::state::AppState;
use tokio::sync::mpsc;

use crate::audio_hub::AudioHandle;

#[derive(Clone, Debug)]
pub struct Announcement {
    pub text: String,
}

pub fn speak_prompt(event: &str) -> String {
    format!(
        "A household event just occurred. Tell the people in the room in one short sentence, then stop. Do not call tools. Event: {event}"
    )
}

#[derive(Clone)]
pub struct Speaker {
    pub settings: Settings,
    pub app: AppState,
    pub audio: AudioHandle,
    pub tx: mpsc::Sender<Announcement>,
}

impl Speaker {
    pub fn alarm(&self, label: &str) {
        let text = format!("Alarm: {label}");
        self.app.push_event(&text);
        self.audio.earcon(Earcon::Alert);
        self.speak(text);
    }

    pub fn timer_finished(&self, label: &str) {
        let text = format!("Timer finished: {label}");
        self.app.push_event(&text);
        self.audio.earcon(Earcon::Timer);
        self.app.refresh_quiet(&self.settings);
        if !self.app.is_quiet() && !self.suppressed_by_presence() {
            self.speak(text);
        }
    }

    pub fn alert(&self, text: String, critical: bool) {
        self.app.push_event(&text);
        self.app.refresh_quiet(&self.settings);
        let speak = critical || (!self.app.is_quiet() && !self.suppressed_by_presence());
        if speak {
            self.audio.earcon(if critical { Earcon::Alert } else { Earcon::Listen });
            self.speak(text);
        }
    }

    fn suppressed_by_presence(&self) -> bool {
        self.app.nobody_home() && !self.settings.live().announce_when_empty
    }

    fn speak(&self, text: String) {
        if self.tx.try_send(Announcement { text: speak_prompt(&text) }).is_err() {
            log::warn!("dropped spoken announcement: {text}");
        }
    }
}
