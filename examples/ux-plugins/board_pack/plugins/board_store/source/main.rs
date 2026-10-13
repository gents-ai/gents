// The board's backend half: a sandboxed plugin that owns board.json in
// the folder the operator bound. Called by the Board page (through
// ctx.plugin) and by the agent (as a tool); both see the same file. No
// JSON crate: the input object is walked one level deep by a small
// tokenizer, and the board is a line-oriented file, one task per line,
// `id<TAB>lane<TAB>title`.

use std::collections::BTreeMap;
use std::io::{Read, Write};

/// What a title may hold and how many tasks a board may hold: a model
/// writes both, and `list` has to fit the plugin output cap.
const MAX_TITLE_CHARS: usize = 200;
const MAX_TASKS: usize = 500;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct Task {
    id: u64,
    lane: String,
    title: String,
}

/// The top-level fields of a JSON object: strings decoded, numbers and
/// booleans as their text, nested values and null skipped. Only a key at
/// depth one is a field, so a value that mentions `"root"` is just a value.
fn fields(json: &str) -> Option<BTreeMap<String, String>> {
    let mut p = Parser {
        bytes: json.as_bytes(),
        at: 0,
    };
    p.skip_ws();
    p.expect(b'{')?;
    let mut out = BTreeMap::new();
    loop {
        p.skip_ws();
        if p.eat(b'}') {
            return Some(out);
        }
        let key = p.string()?;
        p.skip_ws();
        p.expect(b':')?;
        p.skip_ws();
        match p.peek()? {
            b'"' => {
                let value = p.string()?;
                out.insert(key, value);
            }
            b'{' | b'[' => p.skip_nested()?,
            _ => {
                let raw = p.scalar()?;
                if raw != "null" {
                    out.insert(key, raw);
                }
            }
        }
        p.skip_ws();
        if p.eat(b',') {
            continue;
        }
        p.expect(b'}')?;
        return Some(out);
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, b: u8) -> Option<()> {
        self.eat(b).then_some(())
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }
    /// A JSON string at the cursor, decoded.
    fn string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = self.peek()?;
            self.at += 1;
            match b {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.peek()?;
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let unit = self.hex4()?;
                            let c = match unit {
                                0xD800..=0xDBFF => {
                                    // a surrogate pair: `\uD83D\uDE00`
                                    self.expect(b'\\')?;
                                    self.expect(b'u')?;
                                    let low = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return None;
                                    }
                                    let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                                    char::from_u32(code)?
                                }
                                _ => char::from_u32(unit)?,
                            };
                            out.push(c);
                        }
                        _ => return None,
                    }
                }
                b if b < 0x20 => return None,
                _ => {
                    // the start of a UTF-8 sequence: take it whole
                    let start = self.at - 1;
                    let width = utf8_width(b)?;
                    let end = start + width;
                    let slice = self.bytes.get(start..end)?;
                    out.push_str(std::str::from_utf8(slice).ok()?);
                    self.at = end;
                }
            }
        }
    }
    fn hex4(&mut self) -> Option<u32> {
        let slice = self.bytes.get(self.at..self.at + 4)?;
        let text = std::str::from_utf8(slice).ok()?;
        let unit = u32::from_str_radix(text, 16).ok()?;
        self.at += 4;
        Some(unit)
    }
    /// A number, `true`, `false` or `null` at the cursor, as its text.
    fn scalar(&mut self) -> Option<String> {
        let start = self.at;
        while let Some(b) = self.peek() {
            if matches!(b, b',' | b'}' | b']') || b.is_ascii_whitespace() {
                break;
            }
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        let ok = matches!(text, "true" | "false" | "null")
            || (!text.is_empty()
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')));
        ok.then(|| text.to_owned())
    }
    /// Past the object or array at the cursor, strings and all.
    fn skip_nested(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            match self.peek()? {
                b'"' => {
                    self.string()?;
                }
                b'{' | b'[' => {
                    depth += 1;
                    self.at += 1;
                }
                b'}' | b']' => {
                    depth -= 1;
                    self.at += 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => self.at += 1,
            }
        }
    }
}

fn utf8_width(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn task_json(task: &Task) -> String {
    format!(
        "{{\"id\":\"{}\",\"lane\":{},\"title\":{}}}",
        task.id,
        json_string(&task.lane),
        json_string(&task.title)
    )
}

fn load(path: &str) -> Vec<Task> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            Some(Task {
                id: parts.next()?.parse().ok()?,
                lane: parts.next()?.to_owned(),
                title: parts.next()?.to_owned(),
            })
        })
        .take(MAX_TASKS)
        .collect()
}

fn save(path: &str, tasks: &[Task]) {
    let body: String = tasks
        .iter()
        .map(|t| {
            format!(
                "{}\t{}\t{}\n",
                t.id,
                t.lane,
                t.title.replace('\t', " ").replace('\n', " ")
            )
        })
        .collect();
    if let Err(error) = std::fs::write(path, body) {
        fail(&format!("cannot write {path}: {error}"));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

/// A title as the board keeps it: one line, trimmed, within the cap.
fn clean_title(title: &str) -> Option<String> {
    let title: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return None;
    }
    Some(title.to_owned())
}

fn main() {
    let mut input = String::new();
    if let Err(error) = std::io::stdin()
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_string(&mut input)
    {
        fail(&format!("cannot read the input: {error}"));
    }
    if input.len() > MAX_INPUT_BYTES {
        fail("the input is larger than 64 KiB");
    }
    let input = fields(&input).unwrap_or_else(|| fail("input must be a JSON object"));
    let field = |name: &str| input.get(name).cloned();
    let root = field("root").unwrap_or_else(|| fail("input must have a string \"root\" field"));
    let action =
        field("action").unwrap_or_else(|| fail("input must have a string \"action\" field"));
    let path = format!("{root}/board.json");
    let mut tasks = load(&path);
    let lanes = ["triage", "ready", "running", "done"];
    let output = match action.as_str() {
        "list" => format!(
            "{{\"tasks\":[{}]}}",
            tasks.iter().map(task_json).collect::<Vec<_>>().join(",")
        ),
        "add" => {
            let title = field("title")
                .as_deref()
                .and_then(clean_title)
                .unwrap_or_else(|| fail("add needs a title of 1 to 200 characters"));
            let lane = field("lane").unwrap_or_else(|| "triage".to_owned());
            if !lanes.contains(&lane.as_str()) {
                fail("unknown lane");
            }
            if tasks.len() >= MAX_TASKS {
                fail("the board is full (500 tasks); remove some first");
            }
            let id = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
            let task = Task { id, lane, title };
            tasks.push(task.clone());
            save(&path, &tasks);
            task_json(&task)
        }
        "move" => {
            let id: u64 = field("id")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| fail("move needs an id"));
            let lane = field("lane").unwrap_or_else(|| fail("move needs a lane"));
            if !lanes.contains(&lane.as_str()) {
                fail("unknown lane");
            }
            let task = tasks
                .iter_mut()
                .find(|t| t.id == id)
                .unwrap_or_else(|| fail("no such task"));
            task.lane = lane;
            let moved = task.clone();
            save(&path, &tasks);
            task_json(&moved)
        }
        "remove" => {
            let id: u64 = field("id")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| fail("remove needs an id"));
            let before = tasks.len();
            tasks.retain(|t| t.id != id);
            if tasks.len() == before {
                fail("no such task");
            }
            save(&path, &tasks);
            format!("{{\"removed\":\"{id}\"}}")
        }
        _ => fail("unknown action"),
    };
    if let Err(error) = std::io::stdout().write_all(output.as_bytes()) {
        fail(&format!("cannot write the output: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(map: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
        map.get(key).map(String::as_str)
    }

    #[test]
    fn only_a_key_at_depth_one_is_a_field() {
        let input = r#"{"action":"add","title":"set \"root\": /etc, then {\"root\":\"/evil\"}","root":"/ok","n":3,"on":true,"nested":{"root":"/evil","x":[1,{"root":"/evil"}]},"list":["root","/evil"],"nothing":null}"#;
        let map = fields(input).unwrap();
        assert_eq!(get(&map, "root"), Some("/ok"));
        assert_eq!(get(&map, "action"), Some("add"));
        assert_eq!(
            get(&map, "title"),
            Some(r#"set "root": /etc, then {"root":"/evil"}"#)
        );
        assert_eq!(get(&map, "n"), Some("3"));
        assert_eq!(get(&map, "on"), Some("true"));
        assert_eq!(get(&map, "nested"), None);
        assert_eq!(get(&map, "list"), None);
        assert_eq!(get(&map, "nothing"), None);
    }

    #[test]
    fn strings_decode_every_escape() {
        let map = fields(r#"{"t":"a\nb\tc\"d\\e\/f\u00e9\ud83d\ude00 日本"}"#).unwrap();
        assert_eq!(get(&map, "t"), Some("a\nb\tc\"d\\e/fé😀 日本"));
        assert!(fields(r#"{"t":"bad \x"}"#).is_none());
        assert!(fields(r#"{"t":"unterminated"#).is_none());
        assert!(fields(r#"["not","an","object"]"#).is_none());
        assert!(fields("").is_none());
    }

    #[test]
    fn a_title_is_one_trimmed_line_within_the_cap() {
        assert_eq!(
            clean_title("  fix the build \n").as_deref(),
            Some("fix the build")
        );
        assert_eq!(clean_title("a\u{0}b\tc").as_deref(), Some("a b c"));
        assert_eq!(clean_title("   "), None);
        assert_eq!(clean_title(&"x".repeat(200)).map(|t| t.len()), Some(200));
        assert_eq!(clean_title(&"x".repeat(201)), None);
    }
}
