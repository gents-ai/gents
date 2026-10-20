/// Characters per chunk: well inside BGE-M3's 8192-token window, small enough
/// that one chunk is about one idea.
pub(crate) const CHUNK_CHARS: usize = 1_500;
/// Chunks embedded per message. Large tool output is reachable through the
/// literal search and `sessions output`; embedding all of it would dominate
/// indexing cost.
pub(crate) const MAX_CHUNKS_PER_MESSAGE: usize = 16;

/// Splits presented message text into at most `MAX_CHUNKS_PER_MESSAGE` chunks
/// of at most `CHUNK_CHARS` characters, preferring paragraph, then line, then
/// word boundaries. Blank text yields no chunks.
pub(crate) fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && chunks.len() < MAX_CHUNKS_PER_MESSAGE {
        let window_end = rest
            .char_indices()
            .nth(CHUNK_CHARS)
            .map_or(rest.len(), |(index, _)| index);
        let split = if window_end == rest.len() {
            window_end
        } else {
            let window = &rest[..window_end];
            let floor = window.len() / 2;
            ["\n\n", "\n", " "]
                .iter()
                .find_map(|boundary| window.rfind(boundary).filter(|&at| at >= floor))
                .unwrap_or(window_end)
        };
        let chunk = rest[..split].trim();
        if !chunk.is_empty() {
            chunks.push(chunk.to_owned());
        }
        rest = rest[split..].trim_start();
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk_and_blank_text_is_none() {
        assert_eq!(chunk_text("  hello world \n"), vec!["hello world"]);
        assert!(chunk_text(" \n\t").is_empty());
    }

    #[test]
    fn long_text_splits_on_boundaries_within_the_bounds() {
        let paragraph = "word ".repeat(200);
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= CHUNK_CHARS);
            assert!(!chunk.starts_with(' ') && !chunk.ends_with(' '));
        }
        assert!(chunks.iter().all(|chunk| !chunk.contains("wo rd")));
    }

    #[test]
    fn unbroken_multibyte_text_splits_on_character_boundaries() {
        let text = "é".repeat(CHUNK_CHARS * 2 + 7);
        let chunks = chunk_text(&text);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), CHUNK_CHARS);
        assert_eq!(chunks[2].chars().count(), 7);
    }

    #[test]
    fn chunks_per_message_are_capped() {
        let text = "x".repeat(CHUNK_CHARS * (MAX_CHUNKS_PER_MESSAGE + 3));
        assert_eq!(chunk_text(&text).len(), MAX_CHUNKS_PER_MESSAGE);
    }
}
