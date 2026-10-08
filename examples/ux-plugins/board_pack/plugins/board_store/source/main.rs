// The board's backend half: a sandboxed plugin that owns board.json in
// the folder the operator bound. Called by the Board page (through
// ctx.plugin) and by the agent (as a tool); both see the same file. No
// JSON crate, so the board is a line-oriented file parsed by hand: one
// task per line, `id<TAB>lane<TAB>title`.

use std::io::{Read, Write};

#[derive(Clone)]
struct Task {
    id: u64,
    lane: String,
    title: String,
}

fn field(json: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\"");
    let after_key = &json[json.find(&needle)? + needle.len()..];
    let after_colon = &after_key[after_key.find(':')? + 1..];
    let start = after_colon.find('"')? + 1;
    let rest = &after_colon[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
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
        .collect()
}

fn save(path: &str, tasks: &[Task]) {
    let body: String = tasks
        .iter()
        .map(|t| format!("{}\t{}\t{}\n", t.id, t.lane, t.title.replace('\t', " ").replace('\n', " ")))
        .collect();
    std::fs::write(path, body).expect("writing board.json");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("reading stdin");
    let root = field(&input, "root").unwrap_or_else(|| fail("input must have a string \"root\" field"));
    let action = field(&input, "action").unwrap_or_else(|| fail("input must have a string \"action\" field"));
    let path = format!("{root}/board.json");
    let mut tasks = load(&path);
    let lanes = ["triage", "ready", "running", "done"];
    let output = match action.as_str() {
        "list" => format!(
            "{{\"tasks\":[{}]}}",
            tasks.iter().map(task_json).collect::<Vec<_>>().join(",")
        ),
        "add" => {
            let title = field(&input, "title").unwrap_or_else(|| fail("add needs a title"));
            let lane = field(&input, "lane").unwrap_or_else(|| "triage".to_owned());
            if !lanes.contains(&lane.as_str()) {
                fail("unknown lane");
            }
            let id = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
            let task = Task { id, lane, title };
            tasks.push(task.clone());
            save(&path, &tasks);
            task_json(&task)
        }
        "move" => {
            let id: u64 = field(&input, "id").and_then(|s| s.parse().ok()).unwrap_or_else(|| fail("move needs an id"));
            let lane = field(&input, "lane").unwrap_or_else(|| fail("move needs a lane"));
            if !lanes.contains(&lane.as_str()) {
                fail("unknown lane");
            }
            let task = tasks.iter_mut().find(|t| t.id == id).unwrap_or_else(|| fail("no such task"));
            task.lane = lane;
            let moved = task.clone();
            save(&path, &tasks);
            task_json(&moved)
        }
        "remove" => {
            let id: u64 = field(&input, "id").and_then(|s| s.parse().ok()).unwrap_or_else(|| fail("remove needs an id"));
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
    std::io::stdout().write_all(output.as_bytes()).expect("writing stdout");
}
