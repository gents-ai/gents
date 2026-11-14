use gents_protocol::request_input::CapturedContext;
use gents_protocol::session::{SessionTitle, SessionTitleSource};

pub const GENERATION_PREAMBLE: &str = "Generate concise conversation titles. Return only a short, human-readable 3-8 word phrase in sentence case, with normal spaces. Preserve proper names and technical terms. Never call tools. Never explain.";
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

pub fn generated_title(raw: &str, fallback: &str) -> String {
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

pub fn generation_prompt(request: &str, recent: &[String]) -> String {
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

pub fn generation_input(prose: &str, contexts: &[CapturedContext]) -> String {
    if !prose.trim().is_empty() || contexts.is_empty() {
        return prose.to_owned();
    }
    let captured = contexts
        .iter()
        .map(|context| context.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if captured.trim().is_empty() {
        "Attached context".to_owned()
    } else {
        captured
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::request_input::{
        CapturedContextKind, CapturedContextSource, CapturedPosition, CapturedRange,
    };

    #[test]
    fn generated_title_text_cases_bind_native_presentation() {
        gents_lean_vocab::families::title_text_generation::replay::<
            CapturedContextKind,
            CapturedContextSource,
        >(|case| {
            let captures: Vec<CapturedContext> = case
                .captures
                .iter()
                .map(|capture| CapturedContext {
                    kind: capture.kind,
                    source: capture.source,
                    captured_at: capture.captured_at.clone(),
                    uri: capture.uri.clone(),
                    terminal_name: capture.terminal_name.clone(),
                    workspace_uri: capture.workspace_uri.clone(),
                    version: capture
                        .version
                        .map(|version| u32::try_from(version).unwrap()),
                    unsaved: capture.unsaved,
                    range: capture.range.as_ref().map(|range| CapturedRange {
                        start: CapturedPosition {
                            line: u32::try_from(range.start.line).unwrap(),
                            character: u32::try_from(range.start.character).unwrap(),
                        },
                        end: CapturedPosition {
                            line: u32::try_from(range.end.line).unwrap(),
                            character: u32::try_from(range.end.character).unwrap(),
                        },
                    }),
                    content: capture.content.clone(),
                })
                .collect();
            let input = generation_input(&case.fallback, &captures);
            let generated = generated_title(&case.raw, &input);
            assert!(generated.chars().count() <= MAX_CHARACTERS);
            assert!(generated.split_whitespace().count() <= MAX_WORDS);
            let source = if case.generated_source {
                SessionTitleSource::Generated
            } else {
                SessionTitleSource::User
            };
            let display = display_session_title(&SessionTitle {
                text: case.raw.clone(),
                source,
            });
            gents_lean_vocab::families::title_text_generation::GenerationOutput {
                preamble: GENERATION_PREAMBLE.to_owned(),
                prompt: generation_prompt(&input, &case.recent),
                input,
                generated,
                display,
            }
        });
    }
}
