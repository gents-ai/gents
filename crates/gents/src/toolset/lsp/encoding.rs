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
    if server_encodings
        .iter()
        .any(|e| e.eq_ignore_ascii_case("utf-8"))
    {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

pub fn offset_to_position(
    text: &str,
    encoding: PositionEncoding,
    line_1: u32,
    symbol: &str,
) -> Option<LspPosition> {
    let line = text.lines().nth(line_1.saturating_sub(1) as usize)?;
    let (needle, nth) = split_symbol_nth(symbol);
    if needle.is_empty() {
        return Some(LspPosition {
            line: line_1.saturating_sub(1),
            character: 0,
        });
    }
    let mut from = 0;
    let mut byte = None;
    for _ in 0..nth {
        let rel = line.get(from..)?.find(needle)?;
        let abs = from + rel;
        byte = Some(abs);
        from = abs + needle.len();
    }
    let byte = byte?;
    let character = match encoding {
        PositionEncoding::Utf8 => byte as u32,
        PositionEncoding::Utf16 => line[..byte].encode_utf16().count() as u32,
    };
    Some(LspPosition {
        line: line_1.saturating_sub(1),
        character,
    })
}

fn split_symbol_nth(symbol: &str) -> (&str, usize) {
    if let Some((name, rest)) = symbol.rsplit_once('#') {
        if !name.is_empty() {
            if let Ok(n) = rest.parse::<usize>() {
                if n >= 1 {
                    return (name, n);
                }
            }
        }
    }
    (symbol, 1)
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
        let pos =
            offset_to_position(&format!("{line}\n"), PositionEncoding::Utf16, 1, "there").unwrap();
        // "hi " = 3, emoji = 2 UTF-16 units, space = 1 → 6
        assert_eq!(pos.character, 6);
        assert_eq!(
            position_to_byte_offset(line, PositionEncoding::Utf16, pos.character),
            line.find("there").unwrap()
        );
    }

    #[test]
    fn symbol_hash_selects_nth_match() {
        let text = "let add = add(1, add(2, 3));\n";
        let first = offset_to_position(text, PositionEncoding::Utf8, 1, "add").unwrap();
        let second = offset_to_position(text, PositionEncoding::Utf8, 1, "add#2").unwrap();
        assert_eq!(first.character, 4);
        assert_eq!(second.character, 10);
    }
}
