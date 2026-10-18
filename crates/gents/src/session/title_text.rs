use gents_protocol::session::{SessionTitle, SessionTitleSource};

pub(crate) const GENERATION_PREAMBLE: &str = "Generate concise conversation titles. Return only a short, human-readable 3-8 word phrase in sentence case, with normal spaces. Preserve proper names and technical terms. Never call tools. Never explain.";
const MAX_WORDS: usize = 8;
const MAX_CHARACTERS: usize = 72;

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

fn phrase(text: &str) -> String {
    let slug = text.contains('-')
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    text.chars()
        .map(|c| {
            if c.is_whitespace() || (slug && c == '-') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

pub(crate) fn generated_title(raw: &str, fallback: &str) -> String {
    let candidate = first_line(raw).trim_matches(['\'', '"', '`']);
    let source = if candidate.is_empty() {
        first_line(fallback)
    } else {
        candidate
    };
    let phrase = phrase(source);
    let mut title = String::new();
    for word in phrase
        .split(' ')
        .filter(|word| !word.is_empty())
        .take(MAX_WORDS)
    {
        let next_length =
            title.chars().count() + usize::from(!title.is_empty()) + word.chars().count();
        if next_length > MAX_CHARACTERS {
            if title.is_empty() {
                title.extend(word.chars().take(MAX_CHARACTERS));
            }
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => std::iter::once(first.to_ascii_uppercase())
            .chain(chars)
            .collect(),
        None => "Conversation".to_owned(),
    }
}

pub fn display_session_title(title: &SessionTitle) -> String {
    if title.source == SessionTitleSource::Generated {
        generated_title(&title.text, "")
    } else {
        title.text.clone()
    }
}

pub(crate) fn generation_prompt(request: &str, recent: &[String]) -> String {
    let normalized = phrase(request)
        .split(' ')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let excerpt = normalized.chars().take(600).collect::<String>();
    let titles = if recent.is_empty() {
        "none".to_owned()
    } else {
        recent
            .iter()
            .take(5)
            .map(|title| format!("- {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("First user request:\n{excerpt}\n\nRecent session titles to avoid repeating:\n{titles}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_title_text_cases_bind_native_presentation() {
        let contract = &crate::lean_vocab_test::lean_contract_snapshot().title_text_cases;
        assert_eq!(GENERATION_PREAMBLE, contract["preamble"].as_str().unwrap());
        for case in contract["cases"].as_array().unwrap() {
            let raw = case["raw"].as_str().unwrap();
            let fallback = case["fallback"].as_str().unwrap();
            let generated = generated_title(raw, fallback);
            assert_eq!(
                generated,
                case["expected_generated"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert!(generated.chars().count() <= MAX_CHARACTERS);
            assert!(generated.split_whitespace().count() <= MAX_WORDS);
            let source = if case["generated_source"].as_bool().unwrap() {
                SessionTitleSource::Generated
            } else {
                SessionTitleSource::User
            };
            assert_eq!(
                display_session_title(&SessionTitle {
                    text: raw.to_owned(),
                    source
                }),
                case["expected_display"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            let recent = case["recent"]
                .as_array()
                .unwrap()
                .iter()
                .map(|title| title.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                generation_prompt(fallback, &recent),
                case["expected_prompt"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
    }
}
