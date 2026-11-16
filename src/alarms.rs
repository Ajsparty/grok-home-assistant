use crate::announce::Speaker;
use anyhow::Result;
use chrono::{Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alarm {
    pub id: u64,
    pub label: String,
    pub hour: u8,
    pub minute: u8,
    pub days: Vec<u8>,
    pub enabled: bool,
    pub last_fired: String,
}

#[derive(Clone)]
pub struct AlarmStore {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    next_id: u64,
    alarms: Vec<Alarm>,
}

impl AlarmStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let alarms = read_alarms(&path);
        let next_id = alarms.iter().map(|alarm| alarm.id).max().unwrap_or(0) + 1;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path,
                next_id,
                alarms,
            })),
        })
    }

    pub fn list(&self) -> Vec<Alarm> {
        let mut alarms = self.lock().alarms.clone();
        alarms.sort_by_key(|alarm| (alarm.hour, alarm.minute, alarm.id));
        alarms
    }

    pub fn add(&self, label: &str, hour: u8, minute: u8, days: Vec<u8>) -> Result<Alarm, String> {
        if hour > 23 || minute > 59 {
            return Err("use a real time".into());
        }
        let alarm_days = clean_days(days)?;
        let label = clean_label(label);
        let mut guard = self.lock();
        if guard.alarms.len() >= 30 {
            return Err("thirty alarms is the limit".into());
        }
        let id = guard.next_id;
        guard.next_id += 1;
        let alarm = Alarm {
            id,
            label,
            hour,
            minute,
            days: alarm_days,
            enabled: true,
            last_fired: String::new(),
        };
        guard.alarms.push(alarm.clone());
        write_alarms(&guard.path, &guard.alarms);
        Ok(alarm)
    }

    pub fn cancel(&self, id: u64) -> Option<Alarm> {
        let mut guard = self.lock();
        let position = guard.alarms.iter().position(|alarm| alarm.id == id)?;
        let removed = guard.alarms.remove(position);
        write_alarms(&guard.path, &guard.alarms);
        Some(removed)
    }

    pub fn set_enabled(&self, id: u64, enabled: bool) -> Option<Alarm> {
        let mut guard = self.lock();
        let alarm = guard.alarms.iter_mut().find(|alarm| alarm.id == id)?;
        alarm.enabled = enabled;
        let cloned = alarm.clone();
        write_alarms(&guard.path, &guard.alarms);
        Some(cloned)
    }

    pub fn spawn(self, speaker: Speaker, mut shutdown: watch::Receiver<bool>) {
        tokio::spawn(async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                for alarm in self.take_due() {
                    log::info!("alarm fired: {}", alarm.label);
                    speaker.alarm(&alarm.label);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {}
                    _ = shutdown.changed() => {}
                }
                if *shutdown.borrow() {
                    break;
                }
            }
        });
    }

    fn take_due(&self) -> Vec<Alarm> {
        let now = Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let weekday = now.weekday().num_days_from_monday() as u8;
        let mut guard = self.lock();
        let mut due = Vec::new();
        for alarm in &mut guard.alarms {
            if is_due(alarm, weekday, now.hour(), now.minute(), &today) {
                alarm.last_fired = today.clone();
                due.push(alarm.clone());
            }
        }
        if !due.is_empty() {
            write_alarms(&guard.path, &guard.alarms);
        }
        due
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

pub fn is_due(alarm: &Alarm, weekday: u8, hour: u32, minute: u32, today: &str) -> bool {
    alarm.enabled
        && u32::from(alarm.hour) == hour
        && u32::from(alarm.minute) == minute
        && alarm.last_fired != today
        && (alarm.days.is_empty() || alarm.days.contains(&weekday))
}

fn clean_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        "Alarm".into()
    } else {
        trimmed.chars().take(60).collect()
    }
}

fn clean_days(days: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut days = days;
    days.retain(|day| *day <= 6);
    days.sort_unstable();
    days.dedup();
    Ok(days)
}

fn read_alarms(path: &Path) -> Vec<Alarm> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_alarms(path: &Path, alarms: &[Alarm]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(body) = serde_json::to_vec_pretty(alarms) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_err() {
        log::warn!("could not write {}", tmp.display());
        return;
    }
    let _ = std::fs::remove_file(path);
    if std::fs::rename(&tmp, path).is_err() {
        log::warn!("could not replace {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alarm_fires_once_on_its_weekday() {
        let alarm = Alarm {
            id: 1,
            label: "Wake".into(),
            hour: 6,
            minute: 30,
            days: vec![0],
            enabled: true,
            last_fired: String::new(),
        };
        assert!(is_due(&alarm, 0, 6, 30, "2026-11-16"));
        assert!(!is_due(&alarm, 1, 6, 30, "2026-11-16"));
        let mut fired = alarm.clone();
        fired.last_fired = "2026-11-16".into();
        assert!(!is_due(&fired, 0, 6, 30, "2026-11-16"));
    }
}
