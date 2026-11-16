pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_and_case_fold() {
        assert_eq!(normalize("Hey, Grok!"), "hey grok");
    }
}
