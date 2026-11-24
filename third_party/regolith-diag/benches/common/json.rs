//! Minimal JSON value, reader and writer for the bench harness.
//!
//! The workspace carries no serde dependency and PR 1 adds none, so the run
//! file assembler parses and re-emits JSON itself. Objects keep insertion
//! order, so a run file comes out in schema order rather than sorted, and a
//! family emitted by a bench is re-serialized with its own field order intact.

#![allow(dead_code)]

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn s(v: &str) -> Json {
        Json::Str(v.to_string())
    }

    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Replace in place when the key exists, so field order is stable.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Obj(o) = self {
            match o.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => o.push((key.to_string(), value)),
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Obj(o) => o.iter().position(|(k, _)| k == key).map(|i| o.remove(i).1),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn is_obj(&self) -> bool {
        matches!(self, Json::Obj(_))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Num(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }
}

pub fn to_string_pretty(v: &Json) -> String {
    let mut out = String::new();
    write_value(v, 0, &mut out);
    out.push('\n');
    out
}

fn write_value(v: &Json, indent: usize, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&fmt_num(*n)),
        Json::Str(s) => write_string(s, out),
        Json::Arr(a) if a.is_empty() => out.push_str("[]"),
        Json::Arr(a) if a.iter().all(is_scalar) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(e, indent, out);
            }
            out.push(']');
        }
        Json::Arr(a) => {
            out.push_str("[\n");
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(indent + 2, out);
                write_value(e, indent + 2, out);
            }
            out.push('\n');
            pad(indent, out);
            out.push(']');
        }
        Json::Obj(o) if o.is_empty() => out.push_str("{}"),
        Json::Obj(o) => {
            out.push_str("{\n");
            for (i, (k, e)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(indent + 2, out);
                write_string(k, out);
                out.push_str(": ");
                write_value(e, indent + 2, out);
            }
            out.push('\n');
            pad(indent, out);
            out.push('}');
        }
    }
}

fn is_scalar(v: &Json) -> bool {
    !matches!(v, Json::Arr(_) | Json::Obj(_))
}

fn pad(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn fmt_num(n: f64) -> String {
    // JSON has no NaN or Infinity literal; nothing built here produces one.
    if !n.is_finite() {
        return "null".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 9e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn parse(src: &str) -> Result<Json, String> {
    let mut p = Parser {
        b: src.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing data after the JSON value"));
    }
    Ok(v)
}

const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("byte {}: {what}", self.i)
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn byte(&mut self) -> Result<u8, String> {
        let c = self
            .peek()
            .ok_or_else(|| self.err("unexpected end of input"))?;
        self.i += 1;
        Ok(c)
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected {:?}", c as char)))
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("unrecognized literal"))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        if self.depth >= MAX_DEPTH {
            return Err(self.err("nesting deeper than 64 levels"));
        }
        match self
            .peek()
            .ok_or_else(|| self.err("unexpected end of input"))?
        {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Json::Str(self.string()?)),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'n' => self.literal("null", Json::Null),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        self.depth += 1;
        let mut fields = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(b':')?;
            self.ws();
            let v = self.value()?;
            fields.push((k, v));
            self.ws();
            match self.byte()? {
                b',' => continue,
                b'}' => break,
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
        self.depth -= 1;
        Ok(Json::Obj(fields))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        self.depth += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            match self.byte()? {
                b',' => continue,
                b']' => break,
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
        self.depth -= 1;
        Ok(Json::Arr(items))
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| self.err("number is not valid UTF-8"))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("byte {start}: {text:?} is not a JSON number"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.byte()?;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.byte()?;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.err("unrecognized string escape")),
                    }
                }
                _ => {
                    // Multi-byte UTF-8 passes through verbatim: the input is a
                    // &str, so any continuation bytes are already well formed.
                    let start = self.i - 1;
                    while self.peek().is_some_and(|b| b & 0xC0 == 0x80) {
                        self.i += 1;
                    }
                    match std::str::from_utf8(&self.b[start..self.i]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => return Err(self.err("invalid UTF-8 in string")),
                    }
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        let scalar = if (0xD800..0xDC00).contains(&hi) {
            if self.peek() != Some(b'\\') || self.b.get(self.i + 1) != Some(&b'u') {
                return Err(self.err("high surrogate without a low surrogate"));
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.err("high surrogate followed by a non-surrogate"));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(scalar).ok_or_else(|| self.err("escape is not a Unicode scalar value"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self.byte()?;
            let d = (c as char)
                .to_digit(16)
                .ok_or_else(|| self.err("malformed \\u escape"))?;
            v = v * 16 + d;
        }
        Ok(v)
    }
}
