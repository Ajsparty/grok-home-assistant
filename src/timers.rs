use crate::announce::Speaker;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Timer {
    pub id: u64,
    pub label: String,
    pub fire_at_unix: u64,
}

#[derive(Clone)]
pub struct TimerStore {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    next_id: u64,
    timers: Vec<Timer>,
}

impl TimerStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let timers = read_timers(&path);
        let next_id = timers.iter().map(|timer| timer.id).max().unwrap_or(0) + 1;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path,
                next_id,
                timers,
            })),
        })
    }

    pub fn add(&self, label: impl Into<String>, seconds: u64) -> Timer {
        let mut guard = self.lock();
        let id = guard.next_id;
        guard.next_id += 1;
        let timer = Timer {
            id,
            label: label.into(),
            fire_at_unix: now_unix() + seconds,
        };
        guard.timers.push(timer.clone());
        write_timers(&guard.path, &guard.timers);
        timer
    }

    pub fn cancel(&self, id: Option<u64>, label: Option<&str>) -> Option<Timer> {
        let mut guard = self.lock();
        let position = guard.timers.iter().position(|timer| {
            id == Some(timer.id)
                || label
                    .map(|wanted| timer.label.eq_ignore_ascii_case(wanted))
                    .unwrap_or(false)
        })?;
        let removed = guard.timers.remove(position);
        write_timers(&guard.path, &guard.timers);
        Some(removed)
    }

    pub fn list(&self) -> Vec<Timer> {
        let mut timers = self.lock().timers.clone();
        timers.sort_by_key(|timer| timer.fire_at_unix);
        timers
    }

    pub fn spawn(self, speaker: Speaker, mut shutdown: watch::Receiver<bool>) {
        tokio::spawn(async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let due = self.take_due();
                for timer in due {
                    log::info!("timer fired: {}", timer.label);
                    speaker.timer_finished(&timer.label);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    _ = shutdown.changed() => {}
                }
                if *shutdown.borrow() {
                    break;
                }
            }
        });
    }

    fn take_due(&self) -> Vec<Timer> {
        let mut guard = self.lock();
        let now = now_unix();
        let mut due = Vec::new();
        guard.timers.retain(|timer| {
            if timer.fire_at_unix <= now {
                due.push(timer.clone());
                false
            } else {
                true
            }
        });
        if !due.is_empty() {
            write_timers(&guard.path, &guard.timers);
        }
        due
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn read_timers(path: &Path) -> Vec<Timer> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(timers) = serde_json::from_str::<Vec<Timer>>(&text) else {
        return Vec::new();
    };
    let now = now_unix();
    timers.into_iter().filter(|timer| timer.fire_at_unix > now).collect()
}

fn write_timers(path: &Path, timers: &[Timer]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(body) = serde_json::to_vec_pretty(timers) else {
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
    fn timers_round_trip_through_a_file() {
        let path = std::env::temp_dir().join(format!(
            "grok-timers-{}-{}.json",
            std::process::id(),
            now_unix()
        ));
        let store = TimerStore::load(&path).unwrap();
        let timer = store.add("pasta", 120);
        let loaded = TimerStore::load(&path).unwrap();
        assert_eq!(loaded.list()[0].label, "pasta");
        assert!(loaded.cancel(Some(timer.id), None).is_some());
        assert!(loaded.list().is_empty());
        let _ = std::fs::remove_file(path);
    }
}
