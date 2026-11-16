#[derive(Clone, Debug, PartialEq)]
pub enum MediaAction {
    Play,
    Pause,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    SetVolume(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FastAction {
    Turn { on: bool, target: String },
    Media(MediaAction),
    Timer { seconds: u64, label: String },
}

pub fn parse(text: &str) -> Option<FastAction> {
    let text = crate::textutil::normalize(text);
    if text.is_empty() || text.split_whitespace().count() > 14 {
        return None;
    }
    if text.contains("timer") || text.contains("remind") {
        if let Some(action) = parse_timer(&text) {
            return Some(action);
        }
    }
    if let Some(action) = parse_media(&text) {
        return Some(action);
    }
    parse_turn(&text)
}

pub fn is_sensitive(action: &FastAction, needles: &[String]) -> bool {
    let FastAction::Turn { target, .. } = action else {
        return false;
    };
    let target = target.to_lowercase();
    target.contains("lock")
        || target.contains("alarm")
        || target.contains("garage")
        || needles
            .iter()
            .any(|needle| !needle.is_empty() && target.contains(&needle.to_lowercase()))
}

fn parse_timer(text: &str) -> Option<FastAction> {
    let seconds = extract_duration(text)?;
    if seconds == 0 || seconds > 24 * 60 * 60 {
        return None;
    }
    Some(FastAction::Timer {
        seconds,
        label: timer_label(text),
    })
}

fn extract_duration(text: &str) -> Option<u64> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for index in 0..words.len().saturating_sub(1) {
        let Some(count) = parse_number(words[index]) else {
            continue;
        };
        let unit = words[index + 1].trim_end_matches('s');
        let factor = match unit {
            "second" => 1,
            "minute" => 60,
            "hour" => 3600,
            _ => continue,
        };
        return Some(count.saturating_mul(factor));
    }
    None
}

fn parse_number(word: &str) -> Option<u64> {
    match word {
        "a" | "an" => Some(1),
        _ => word.parse().ok(),
    }
}

fn timer_label(text: &str) -> String {
    for marker in [" called ", " named "] {
        if let Some(index) = text.find(marker) {
            let label = text[index + marker.len()..].trim();
            if !label.is_empty() {
                return label.to_string();
            }
        }
    }
    "timer".to_string()
}

fn parse_media(text: &str) -> Option<FastAction> {
    let action = match text {
        "pause" | "pause the music" | "pause the tv" | "pause the television" => {
            MediaAction::Pause
        }
        "play" | "resume" | "play the music" | "resume the music" => MediaAction::Play,
        "next" | "next track" | "skip" => MediaAction::Next,
        "previous" | "previous track" | "last track" => MediaAction::Previous,
        "volume up" | "turn it up" | "louder" => MediaAction::VolumeUp,
        "volume down" | "turn it down" | "quieter" => MediaAction::VolumeDown,
        _ => return volume_phrase(text).map(MediaAction::SetVolume).map(FastAction::Media),
    };
    Some(FastAction::Media(action))
}

fn volume_phrase(text: &str) -> Option<f32> {
    if !text.contains("volume") {
        return None;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let index = words.iter().position(|word| *word == "volume")?;
    let number = words
        .iter()
        .skip(index + 1)
        .find_map(|word| word.trim_end_matches('%').parse::<f32>().ok())?;
    let level = if number > 1.0 { number / 100.0 } else { number };
    (0.0..=1.0).contains(&level).then_some(level)
}

fn parse_turn(text: &str) -> Option<FastAction> {
    for (prefix, on) in [
        ("turn on ", true),
        ("turn off ", false),
        ("switch on ", true),
        ("switch off ", false),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return turn(on, rest);
        }
    }
    if let Some(rest) = text.strip_prefix("turn ") {
        if let Some(name) = rest.strip_suffix(" off") {
            return turn(false, name);
        }
        if let Some(name) = rest.strip_suffix(" on") {
            return turn(true, name);
        }
    }
    if starts_with_pronoun(text) || text.split_whitespace().count() > 5 {
        return None;
    }
    if let Some(name) = text.strip_suffix(" off") {
        return turn(false, name);
    }
    if let Some(name) = text.strip_suffix(" on") {
        return turn(true, name);
    }
    None
}

fn turn(on: bool, raw: &str) -> Option<FastAction> {
    let target = clean_target(raw);
    if target.is_empty() {
        None
    } else {
        Some(FastAction::Turn { on, target })
    }
}

fn clean_target(raw: &str) -> String {
    let mut target = raw.trim();
    loop {
        let next = ["the ", "my ", "a ", "an "]
            .iter()
            .find_map(|prefix| target.strip_prefix(prefix));
        match next {
            Some(stripped) => target = stripped.trim(),
            None => break,
        }
    }
    if let Some(stripped) = target.strip_suffix(" please") {
        target = stripped.trim();
    }
    target.to_string()
}

fn starts_with_pronoun(text: &str) -> bool {
    matches!(
        text.split_whitespace().next(),
        Some("i" | "im" | "we" | "it" | "its" | "that" | "this" | "he" | "she" | "they")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_household_commands() {
        assert_eq!(
            parse("turn off the office lamp"),
            Some(FastAction::Turn {
                on: false,
                target: "office lamp".into()
            })
        );
        assert_eq!(
            parse("kitchen lights off"),
            Some(FastAction::Turn {
                on: false,
                target: "kitchen lights".into()
            })
        );
        assert_eq!(parse("pause"), Some(FastAction::Media(MediaAction::Pause)));
        assert_eq!(
            parse("set a timer for 5 minutes called pasta"),
            Some(FastAction::Timer {
                seconds: 300,
                label: "pasta".into()
            })
        );
        assert_eq!(
            parse("remind me in 90 seconds"),
            Some(FastAction::Timer {
                seconds: 90,
                label: "timer".into()
            })
        );
        assert_eq!(parse("what is the weather"), None);
        assert_eq!(parse("i am off"), None);
    }

    #[test]
    fn garage_and_locks_stay_with_the_model() {
        let action = parse("turn off the garage door").unwrap();
        assert!(is_sensitive(&action, &["garage_door".into(), "front_door".into()]));
    }
}
