//! ECMAScript `JSON.stringify(JSON.parse(text))` over JSON text.
//!
//! Claude Code replays a tool call's input as the re-serialization of the text
//! the model streamed. That differs from a `serde_json::Value` round trip in
//! four ways this module reproduces: object keys keep first-occurrence order
//! (a duplicate key keeps the last value at the first key's position), strings
//! are UTF-16 so a lone surrogate escape survives as the same lowercase
//! `\udXXX` escape, numbers are IEEE doubles printed as `Number::toString`
//! (`-0` is `0`, `1e999` is `null`), and nothing is spaced.
//!
//! Invariant: the output is valid JSON that [`serde_json`] parses, except that
//! a lone-surrogate escape is outside what a Rust `String` holds.

use serde_json::value::RawValue;

use super::WireError;

/// Nesting beyond this is refused rather than recursed into.
const MAX_DEPTH: usize = 512;

/// `JSON.stringify(JSON.parse(text))`.
pub fn js_restringify(text: &str) -> Result<Box<RawValue>, WireError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    parser.space();
    let mut out = String::with_capacity(text.len());
    parser.value(&mut out, 0)?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(RawValue::from_string(out)?)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn error(&self, reason: &'static str) -> WireError {
        WireError::InvalidJson {
            offset: self.at,
            reason,
        }
    }

    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let hit = self.bytes.get(self.at) == Some(&byte);
        self.at += usize::from(hit);
        hit
    }

    fn literal(&mut self, word: &str, out: &mut String, emit: &str) -> Result<(), WireError> {
        if !self.bytes[self.at..].starts_with(word.as_bytes()) {
            return Err(self.error("unexpected token"));
        }
        self.at += word.len();
        out.push_str(emit);
        Ok(())
    }

    fn value(&mut self, out: &mut String, depth: usize) -> Result<(), WireError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        match self.bytes.get(self.at) {
            Some(b'{') => self.object(out, depth),
            Some(b'[') => self.array(out, depth),
            Some(b'"') => {
                let units = self.string()?;
                push_json_string(out, &units);
                Ok(())
            }
            Some(b't') => self.literal("true", out, "true"),
            Some(b'f') => self.literal("false", out, "false"),
            Some(b'n') => self.literal("null", out, "null"),
            Some(b'-' | b'0'..=b'9') => self.number(out),
            _ => Err(self.error("unexpected token")),
        }
    }

    fn array(&mut self, out: &mut String, depth: usize) -> Result<(), WireError> {
        self.at += 1;
        out.push('[');
        self.space();
        if !self.eat(b']') {
            loop {
                self.space();
                self.value(out, depth + 1)?;
                self.space();
                if self.eat(b']') {
                    break;
                }
                if !self.eat(b',') {
                    return Err(self.error("expected ',' or ']'"));
                }
                out.push(',');
            }
        }
        out.push(']');
        Ok(())
    }

    fn object(&mut self, out: &mut String, depth: usize) -> Result<(), WireError> {
        self.at += 1;
        let mut entries: Vec<(Vec<u16>, String)> = Vec::new();
        self.space();
        if !self.eat(b'}') {
            loop {
                self.space();
                if self.bytes.get(self.at) != Some(&b'"') {
                    return Err(self.error("expected a string key"));
                }
                let key = self.string()?;
                self.space();
                if !self.eat(b':') {
                    return Err(self.error("expected ':'"));
                }
                self.space();
                let mut value = String::new();
                self.value(&mut value, depth + 1)?;
                match entries.iter_mut().find(|(seen, _)| *seen == key) {
                    Some(entry) => entry.1 = value,
                    None => entries.push((key, value)),
                }
                self.space();
                if self.eat(b'}') {
                    break;
                }
                if !self.eat(b',') {
                    return Err(self.error("expected ',' or '}'"));
                }
            }
        }
        out.push('{');
        for (index, (key, value)) in entries.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_string(out, key);
            out.push(':');
            out.push_str(value);
        }
        out.push('}');
        Ok(())
    }

    /// A string literal as UTF-16 code units.
    fn string(&mut self) -> Result<Vec<u16>, WireError> {
        self.at += 1;
        let mut units = Vec::new();
        loop {
            let run_start = self.at;
            while !matches!(self.bytes.get(self.at), Some(b'"' | b'\\') | None) {
                if self.bytes[self.at] < 0x20 {
                    return Err(self.error("control character in string"));
                }
                self.at += 1;
            }
            // The input is a `&str` and the run ends on an ASCII byte, so it is UTF-8.
            let run = std::str::from_utf8(&self.bytes[run_start..self.at])
                .map_err(|_| self.error("invalid UTF-8"))?;
            units.extend(run.encode_utf16());
            match self.bytes.get(self.at) {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(units);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let unit = match self.bytes.get(self.at) {
                        Some(b'"') => u16::from(b'"'),
                        Some(b'\\') => u16::from(b'\\'),
                        Some(b'/') => u16::from(b'/'),
                        Some(b'b') => 0x08,
                        Some(b'f') => 0x0c,
                        Some(b'n') => 0x0a,
                        Some(b'r') => 0x0d,
                        Some(b't') => 0x09,
                        Some(b'u') => {
                            self.at += 1;
                            units.push(self.hex4()?);
                            continue;
                        }
                        _ => return Err(self.error("invalid escape")),
                    };
                    self.at += 1;
                    units.push(unit);
                }
                _ => return Err(self.error("unterminated string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, WireError> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| self.error("truncated \\u escape"))?;
        let mut unit = 0u16;
        for &digit in digits {
            let value = char::from(digit)
                .to_digit(16)
                .ok_or_else(|| self.error("invalid \\u escape"))?;
            unit = (unit << 4) | value as u16;
        }
        self.at += 4;
        Ok(unit)
    }

    fn number(&mut self, out: &mut String) -> Result<(), WireError> {
        let start = self.at;
        self.eat(b'-');
        match self.bytes.get(self.at) {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("invalid number")),
        }
        if self.eat(b'.') {
            if !matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number"));
            }
            self.digits();
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number"));
            }
            self.digits();
        }
        let literal = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| self.error("invalid number"))?;
        let value = literal
            .parse::<f64>()
            .map_err(|_| self.error("invalid number"))?;
        push_number(out, value);
        Ok(())
    }

    fn digits(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }
}

/// `JSON.stringify` of a string: well-formed pairs are literal, a lone
/// surrogate is a lowercase `\udXXX` escape.
fn push_json_string(out: &mut String, units: &[u16]) {
    out.push('"');
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok('\u{8}') => out.push_str("\\b"),
            Ok('\u{c}') => out.push_str("\\f"),
            Ok('\n') => out.push_str("\\n"),
            Ok('\r') => out.push_str("\\r"),
            Ok('\t') => out.push_str("\\t"),
            Ok(c) if c < ' ' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            Ok(c) => out.push(c),
            Err(lone) => out.push_str(&format!("\\u{:04x}", lone.unpaired_surrogate())),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString` (radix 10) as `JSON.stringify` prints it:
/// a non-finite number is `null`.
fn push_number(out: &mut String, value: f64) {
    if !value.is_finite() {
        out.push_str("null");
        return;
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    if value < 0.0 {
        out.push('-');
    }
    // `{:e}` prints the shortest digits that round-trip: `d[.ddd]e<exp>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let count = digits.len() as i32;
    // The value is 0.<digits> * 10^point.
    let point = exponent + 1;
    if count <= point && point <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (point - count) as usize));
    } else if 0 < point && point <= 21 {
        out.push_str(&digits[..point as usize]);
        out.push('.');
        out.push_str(&digits[point as usize..]);
    } else if -6 < point && point <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-point) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if count > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if point - 1 < 0 { '-' } else { '+' });
        out.push_str(&(point - 1).abs().to_string());
    }
}

#[cfg(test)]
#[path = "js_json_tests.rs"]
mod tests;
