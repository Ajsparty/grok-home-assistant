use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AuditLog {
    file: Arc<Mutex<Option<std::fs::File>>>,
}

impl AuditLog {
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        if file.is_none() {
            log::warn!("audit log is not writable at {}", path.display());
        }
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }

    pub fn record(&self, line: &Value) {
        let Ok(mut slot) = self.file.lock() else {
            return;
        };
        let Some(file) = slot.as_mut() else {
            log::info!("audit {line}");
            return;
        };
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

pub fn tail(path: &Path, limit: usize) -> Vec<Value> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = length.saturating_sub(256 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<Value> = text
        .lines()
        .skip(if start == 0 { 0 } else { 1 })
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if lines.len() > limit {
        lines.drain(0..lines.len() - limit);
    }
    lines
}
