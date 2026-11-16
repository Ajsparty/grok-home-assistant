use crate::textutil::normalize;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct PhraseGate {
    phrase: String,
    follow: Duration,
    armed_until: Option<Instant>,
}

impl PhraseGate {
    pub fn new(phrase: &str, follow: Duration) -> Self {
        Self {
            phrase: normalize(phrase),
            follow,
            armed_until: None,
        }
    }

    pub fn arm_for(&mut self, duration: Duration) {
        self.armed_until = Some(Instant::now() + duration);
    }

    pub fn retune(&mut self, phrase: &str, follow: Duration) {
        let phrase = normalize(phrase);
        if phrase != self.phrase {
            self.phrase = phrase;
            self.armed_until = None;
        }
        self.follow = follow;
    }

    pub fn remaining_secs(&self) -> u64 {
        self.armed_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs())
            .unwrap_or(0)
    }

    pub fn armed(&self) -> bool {
        self.phrase.is_empty()
            || self
                .armed_until
                .map(|until| Instant::now() < until)
                .unwrap_or(false)
    }

    /// Returns whether tools may run after hearing this transcript.
    pub fn observe(&mut self, transcript: &str) -> bool {
        if self.phrase.is_empty() {
            return true;
        }
        if normalize(transcript).contains(&self.phrase) {
            self.arm_for(self.follow);
        }
        self.armed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_arms_and_follow_up_stays_open() {
        let mut gate = PhraseGate::new("hey grok", Duration::from_secs(20));
        assert!(!gate.observe("turn the lights off"));
        assert!(gate.observe("hey grok, turn the lights off"));
        assert!(gate.observe("lights off"));
    }

    #[test]
    fn empty_phrase_is_always_armed() {
        let mut gate = PhraseGate::new("", Duration::from_secs(5));
        assert!(gate.observe("anyone there"));
    }
}
