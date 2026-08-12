#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

pub fn negotiate(server_encodings: &[String]) -> PositionEncoding {
    if server_encodings.iter().any(|e| e.eq_ignore_ascii_case("utf-8")) {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

pub fn offset_to_position(text: &str, encoding: PositionEncoding, line_1: u32, symbol: &str) -> Option<LspPosition> {
    let line = text.lines().nth(line_1.saturating_sub(1) as usize)?;
    let byte = line.find(symbol)?;
    let character = match encoding {
        PositionEncoding::Utf8 => byte as u32,
        PositionEncoding::Utf16 => line[..byte].encode_utf16().count() as u32,
    };
    Some(LspPosition {
        line: line_1.saturating_sub(1),
        character,
    })
}

pub fn position_to_byte_offset(line: &str, encoding: PositionEncoding, character: u32) -> usize {
    match encoding {
        PositionEncoding::Utf8 => (character as usize).min(line.len()),
        PositionEncoding::Utf16 => {
            let mut units = 0u32;
            for (idx, ch) in line.char_indices() {
                if units >= character {
                    return idx;
                }
                units += ch.len_utf16() as u32;
            }
            line.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_emoji_column() {
        let line = "hi 😀 there";
        let pos = offset_to_position(&format!("{line}\n"), PositionEncoding::Utf16, 1, "there")
            .unwrap();
        // "hi " = 3, emoji = 2 UTF-16 units, space = 1 → 6
        assert_eq!(pos.character, 6);
        assert_eq!(
            position_to_byte_offset(line, PositionEncoding::Utf16, pos.character),
            line.find("there").unwrap()
        );
    }
}
