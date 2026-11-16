use getrandom::getrandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct ConsoleGuard {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    from_env: bool,
    enabled: bool,
    salt: Vec<u8>,
    hash: Vec<u8>,
    sessions: HashSet<String>,
    failures: u32,
    wait_until: Option<Instant>,
}

#[derive(Serialize, Deserialize)]
struct LockFile {
    salt_hex: String,
    hash_hex: String,
}

impl ConsoleGuard {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let env_password = std::env::var("DASHBOARD_PASSWORD")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && !value.contains("your_"));
        let (from_env, enabled, salt, hash) = if let Some(password) = env_password {
            let salt = random_bytes(16);
            (true, true, salt.clone(), hash_password(&salt, &password))
        } else if let Some(saved) = read_lock(&path) {
            (false, true, saved.0, saved.1)
        } else {
            (false, false, Vec::new(), Vec::new())
        };
        Self {
            inner: Arc::new(Mutex::new(Inner {
                path,
                from_env,
                enabled,
                salt,
                hash,
                sessions: HashSet::new(),
                failures: 0,
                wait_until: None,
            })),
        }
    }

    pub fn status(&self) -> (bool, bool) {
        let guard = self.lock();
        (guard.enabled, guard.from_env)
    }

    pub fn login(&self, password: &str) -> Result<String, &'static str> {
        let mut guard = self.lock();
        if !guard.enabled {
            return Err("no password is set");
        }
        if guard.wait_until.is_some_and(|until| Instant::now() < until) {
            return Err("too many tries, wait a moment");
        }
        if !same(&guard.hash, &hash_password(&guard.salt, password)) {
            guard.failures += 1;
            if guard.failures >= 8 {
                guard.wait_until = Some(Instant::now() + Duration::from_secs(15));
                guard.failures = 0;
            }
            return Err("wrong password");
        }
        guard.failures = 0;
        guard.wait_until = None;
        let token = hex(&random_bytes(32));
        guard.sessions.insert(token.clone());
        Ok(token)
    }

    pub fn logout(&self, token: &str) {
        self.lock().sessions.remove(token);
    }

    pub fn allows(&self, token: Option<&str>) -> bool {
        let guard = self.lock();
        if !guard.enabled {
            return true;
        }
        token.is_some_and(|token| guard.sessions.contains(token))
    }

    pub fn set_password(&self, current: Option<&str>, next: &str) -> Result<(), String> {
        let mut guard = self.lock();
        if guard.from_env {
            return Err("DASHBOARD_PASSWORD is set in the environment".into());
        }
        if guard.enabled && !current.is_some_and(|value| same(&guard.hash, &hash_password(&guard.salt, value))) {
            return Err("the current password does not match".into());
        }
        if next.chars().count() < 8 {
            return Err("use at least 8 characters".into());
        }
        if next.chars().count() > 80 {
            return Err("that password is too long".into());
        }
        let salt = random_bytes(16);
        let hash = hash_password(&salt, next);
        write_lock(
            &guard.path,
            &LockFile {
                salt_hex: hex(&salt),
                hash_hex: hex(&hash),
            },
        )?;
        guard.salt = salt;
        guard.hash = hash;
        guard.enabled = true;
        guard.sessions.clear();
        Ok(())
    }

    pub fn clear_password(&self, current: &str) -> Result<(), String> {
        let mut guard = self.lock();
        if guard.from_env {
            return Err("DASHBOARD_PASSWORD is set in the environment".into());
        }
        if !guard.enabled {
            return Ok(());
        }
        if !same(&guard.hash, &hash_password(&guard.salt, current)) {
            return Err("the current password does not match".into());
        }
        let _ = std::fs::remove_file(&guard.path);
        guard.enabled = false;
        guard.salt.clear();
        guard.hash.clear();
        guard.sessions.clear();
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

fn hash_password(salt: &[u8], password: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(password.as_bytes());
    hasher.finalize().to_vec()
}

fn same(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    if getrandom(&mut bytes).is_err() {
        let ticks = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(1);
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = ((ticks >> ((index % 8) * 8)) as u8).wrapping_add(index as u8);
        }
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = from_hex(bytes[index])?;
        let low = from_hex(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Some(out)
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn read_lock(path: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let file: LockFile = serde_json::from_str(&text).ok()?;
    Some((unhex(&file.salt_hex)?, unhex(&file.hash_hex)?))
}

fn write_lock(path: &Path, file: &LockFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let body = serde_json::to_vec_pretty(file).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|err| err.to_string())?;
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_password_matches_and_a_different_one_does_not() {
        let salt = b"salt-value-fixed";
        let hash = hash_password(salt, "correct horse");
        assert!(same(&hash, &hash_password(salt, "correct horse")));
        assert!(!same(&hash, &hash_password(salt, "wrong horse")));
    }
}
