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

/// Resolve a hover/definition position. An omitted line searches every line
/// for `symbol` instead of defaulting to line 1.
pub fn position_for_symbol(
    text: &str,
    encoding: PositionEncoding,
    line_1: Option<u32>,
    symbol: &str,
) -> Option<LspPosition> {
    match line_1 {
        Some(line) => offset_to_position(text, encoding, line, symbol),
        None if symbol.is_empty() => None,
        None => {
            for line in 1..=u32::try_from(text.lines().count()).unwrap_or(u32::MAX) {
                if let Some(pos) = offset_to_position(text, encoding, line, symbol) {
                    return Some(pos);
                }
            }
            None
        }
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

pub fn position_to_byte_offset(
    line: &str,
    encoding: PositionEncoding,
    character: u32,
) -> Result<usize, String> {
    match encoding {
        PositionEncoding::Utf8 => {
            let offset = (character as usize).min(line.len());
            if line.is_char_boundary(offset) {
                Ok(offset)
            } else {
                Err(format!(
                    "UTF-8 position {character} falls inside a multibyte character"
                ))
            }
        }
        PositionEncoding::Utf16 => {
            let mut units = 0u32;
            for (idx, ch) in line.char_indices() {
                if units >= character {
                    return Ok(idx);
                }
                units += ch.len_utf16() as u32;
            }
            Ok(line.len())
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
            position_to_byte_offset(line, PositionEncoding::Utf16, pos.character).unwrap(),
            line.find("there").unwrap()
        );
    }

    #[test]
    fn utf8_mid_codepoint_is_rejected() {
        let line = "a😀b";
        let err = position_to_byte_offset(line, PositionEncoding::Utf8, 2).unwrap_err();
        assert!(err.contains("multibyte"), "{err}");
    }

    #[test]
    fn symbol_hash_selects_nth_match() {
        let text = "let add = add(1, add(2, 3));\n";
        let first = offset_to_position(text, PositionEncoding::Utf8, 1, "add").unwrap();
        let second = offset_to_position(text, PositionEncoding::Utf8, 1, "add#2").unwrap();
        assert_eq!(first.character, 4);
        assert_eq!(second.character, 10);
    }

    #[test]
    fn omitted_line_searches_the_file() {
        let text = "fn skip() {}\npub fn meet(self, other: Self) -> Self { self }\n";
        let pos = position_for_symbol(text, PositionEncoding::Utf8, None, "meet").unwrap();
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 7);
    }
}
