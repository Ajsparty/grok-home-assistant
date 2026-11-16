use crate::config::{AudioOutput, Settings, VoicePrefs};
use crate::earcon::{self, Earcon};
use crate::pcm::{self, SAMPLE_RATE};
use crate::state::AppState;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};

pub enum LocalCmd {
    Play(Vec<i16>),
    Clear,
}

pub enum SatMsg {
    Pcm(Vec<i16>),
    Clear,
}

enum HubCmd {
    Mic { source: String, pcm: Vec<i16> },
    Play(Vec<i16>),
    Earcon(Earcon),
    ClearPlay,
    RegisterLocal(SyncSender<LocalCmd>),
    RegisterSatellite { id: String, tx: mpsc::Sender<SatMsg> },
    UnregisterSatellite(String),
    Wake(String),
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: mpsc::Sender<HubCmd>,
}

impl AudioHandle {
    pub fn mic(&self, source: impl Into<String>, pcm: Vec<i16>) {
        let _ = self.tx.try_send(HubCmd::Mic {
            source: source.into(),
            pcm,
        });
    }

    pub fn play(&self, pcm: Vec<i16>) {
        if !pcm.is_empty() {
            let _ = self.tx.try_send(HubCmd::Play(pcm));
        }
    }

    pub fn clear(&self) {
        let _ = self.tx.try_send(HubCmd::ClearPlay);
    }

    pub fn earcon(&self, kind: Earcon) {
        let _ = self.tx.try_send(HubCmd::Earcon(kind));
    }

    pub fn register_local(&self, tx: SyncSender<LocalCmd>) {
        let _ = self.tx.try_send(HubCmd::RegisterLocal(tx));
    }

    pub fn register_satellite(&self, id: String, tx: mpsc::Sender<SatMsg>) {
        let _ = self.tx.try_send(HubCmd::RegisterSatellite { id, tx });
    }

    pub fn unregister_satellite(&self, id: String) {
        let _ = self.tx.try_send(HubCmd::UnregisterSatellite(id));
    }

    pub fn wake_from(&self, source: impl Into<String>) {
        let _ = self.tx.try_send(HubCmd::Wake(source.into()));
    }
}

pub struct VoiceLinks {
    pub handle: AudioHandle,
    pub mic_rx: mpsc::Receiver<Vec<i16>>,
    pub barge_rx: mpsc::Receiver<()>,
    pub wake: std::sync::Arc<Notify>,
}

pub fn spawn(settings: Settings, app: AppState, mut shutdown: watch::Receiver<bool>) -> VoiceLinks {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
    let (mic_tx, mic_rx) = mpsc::channel(32);
    let (barge_tx, barge_rx) = mpsc::channel(4);
    let wake = std::sync::Arc::new(Notify::new());
    let wake_for_hub = wake.clone();
    tokio::spawn(async move {
        let mut hub = Hub {
            app,
            prefs: settings.voice_prefs,
            mic_tx,
            barge_tx,
            wake: wake_for_hub,
            local: None,
            satellites: HashMap::new(),
            reference: VecDeque::new(),
            active: "local".to_string(),
            loud_for: Duration::ZERO,
            last_loud: Instant::now() - Duration::from_secs(5),
            notified: false,
            playing_until: Instant::now(),
            drop_play_until: Instant::now(),
            barge_latched: false,
        };
        loop {
            let step = tokio::select! {
                cmd = cmd_rx.recv() => Step::Cmd(cmd),
                _ = shutdown.changed() => Step::Shutdown,
            };
            match step {
                Step::Cmd(Some(cmd)) => hub.handle(cmd),
                Step::Cmd(None) => break,
                Step::Shutdown => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });
    VoiceLinks {
        handle: AudioHandle { tx: cmd_tx },
        mic_rx,
        barge_rx,
        wake,
    }
}

enum Step {
    Cmd(Option<HubCmd>),
    Shutdown,
}

struct Hub {
    app: AppState,
    prefs: Arc<Mutex<VoicePrefs>>,
    mic_tx: mpsc::Sender<Vec<i16>>,
    barge_tx: mpsc::Sender<()>,
    wake: std::sync::Arc<Notify>,
    local: Option<SyncSender<LocalCmd>>,
    satellites: HashMap<String, mpsc::Sender<SatMsg>>,
    reference: VecDeque<f32>,
    active: String,
    loud_for: Duration,
    last_loud: Instant,
    notified: bool,
    playing_until: Instant,
    drop_play_until: Instant,
    barge_latched: bool,
}

impl Hub {
    fn handle(&mut self, cmd: HubCmd) {
        match cmd {
            HubCmd::Mic { source, pcm } => self.on_mic(source, pcm),
            HubCmd::Play(pcm) => self.on_play(pcm),
            HubCmd::Earcon(kind) => self.on_play(earcon::synth(kind)),
            HubCmd::ClearPlay => self.clear_outputs(),
            HubCmd::RegisterLocal(tx) => self.local = Some(tx),
            HubCmd::RegisterSatellite { id, tx } => {
                log::info!("satellite joined: {id}");
                self.satellites.insert(id, tx);
                self.publish_satellites();
            }
            HubCmd::UnregisterSatellite(id) => {
                self.satellites.remove(&id);
                if self.active == id {
                    self.active = "local".to_string();
                    self.app.set_active_source("local");
                }
                self.publish_satellites();
            }
            HubCmd::Wake(source) => self.force_wake(source),
        }
    }

    fn publish_satellites(&self) {
        let mut names: Vec<String> = self.satellites.keys().cloned().collect();
        names.sort();
        self.app.set_satellites(names);
    }

    fn on_mic(&mut self, source: String, pcm: Vec<i16>) {
        if pcm.is_empty() {
            return;
        }
        let prefs = self.prefs.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
        let mut floats = pcm::i16_to_f32(&pcm);
        let playing = Instant::now() < self.playing_until;
        if playing && prefs.echo_gain > 0.0 {
            let reference: Vec<f32> = self.reference.iter().copied().collect();
            pcm::echo_subtract(
                &mut floats,
                &reference,
                prefs.echo_delay_samples(),
                prefs.echo_gain,
            );
        }
        let level = pcm::rms_f32(&floats);
        self.app.set_mic_rms(level);
        self.note_level(&source, level, pcm.len(), prefs.wake_rms, prefs.wake_hold());
        if playing && !self.barge_latched && level >= prefs.barge_rms && prefs.barge_in {
            self.barge();
        }
        if !self.app.session_live() {
            return;
        }
        let still_playing = Instant::now() < self.playing_until;
        if still_playing && !self.barge_latched {
            return;
        }
        let outgoing = if playing {
            pcm::f32_to_i16(&floats)
        } else {
            pcm
        };
        let _ = self.mic_tx.try_send(outgoing);
    }

    fn note_level(&mut self, source: &str, level: f32, samples: usize, wake_rms: f32, wake_hold: Duration) {
        let frame = Duration::from_secs_f32(samples as f32 / SAMPLE_RATE as f32);
        if level >= wake_rms {
            if self.last_loud.elapsed() > Duration::from_millis(400) {
                self.loud_for = Duration::ZERO;
            }
            self.loud_for += frame;
            self.last_loud = Instant::now();
            self.active = source.to_string();
            self.app.set_active_source(source);
            self.app.touch_sound();
            if self.loud_for >= wake_hold && !self.app.session_live() && !self.notified {
                self.notified = true;
                self.wake.notify_one();
            }
        } else if level < wake_rms * 0.5 && self.last_loud.elapsed() > Duration::from_millis(400)
        {
            self.loud_for = Duration::ZERO;
            if !self.app.session_live() {
                self.notified = false;
            }
        }
    }

    fn force_wake(&mut self, source: String) {
        self.active = source.clone();
        self.app.set_active_source(&source);
        self.app.touch_sound();
        self.notified = true;
        self.wake.notify_one();
    }

    fn on_play(&mut self, pcm: Vec<i16>) {
        if pcm.is_empty() || Instant::now() < self.drop_play_until {
            return;
        }
        self.barge_latched = false;
        let seconds = pcm.len() as f32 / SAMPLE_RATE as f32;
        let now = Instant::now();
        let base = if self.playing_until > now {
            self.playing_until
        } else {
            now
        };
        self.playing_until = base + Duration::from_secs_f32(seconds);
        for sample in pcm::i16_to_f32(&pcm) {
            self.reference.push_back(sample);
        }
        let cap = SAMPLE_RATE as usize;
        while self.reference.len() > cap {
            self.reference.pop_front();
        }
        let output = self
            .prefs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .audio_output_kind();
        self.emit(pcm, output);
    }

    fn barge(&mut self) {
        self.barge_latched = true;
        self.playing_until = Instant::now();
        self.drop_play_until = Instant::now() + Duration::from_millis(350);
        self.reference.clear();
        self.clear_outputs();
        let _ = self.barge_tx.try_send(());
    }

    fn clear_outputs(&self) {
        if let Some(tx) = &self.local {
            let _ = tx.try_send(LocalCmd::Clear);
        }
        for tx in self.satellites.values() {
            let _ = tx.try_send(SatMsg::Clear);
        }
    }

    fn emit(&self, pcm: Vec<i16>, output: AudioOutput) {
        let satellite = self.satellites.get(&self.active);
        let to_local = match output {
            AudioOutput::Local | AudioOutput::Both => true,
            AudioOutput::Satellite => false,
            AudioOutput::Auto => satellite.is_none(),
        };
        let to_satellite = match output {
            AudioOutput::Satellite | AudioOutput::Both => true,
            AudioOutput::Local => false,
            AudioOutput::Auto => satellite.is_some(),
        };
        if to_local {
            if let Some(tx) = &self.local {
                let _ = tx.try_send(LocalCmd::Play(pcm.clone()));
            }
        }
        if to_satellite {
            if let Some(tx) = satellite {
                let _ = tx.try_send(SatMsg::Pcm(pcm));
            } else {
                for tx in self.satellites.values() {
                    let _ = tx.try_send(SatMsg::Pcm(pcm.clone()));
                }
            }
        }
    }
}
