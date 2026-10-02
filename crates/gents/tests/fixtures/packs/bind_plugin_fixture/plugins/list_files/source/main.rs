// Lists the file names directly under whatever directory the host bound
// into `root` (see `PackPlugin::bind_dir`), sorted. No JSON crate: the
// input this plugin is ever called with is exactly `{"root": "<path>"}`,
// so a hand-written extractor for one string field is the whole job.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("reading stdin");
    let root = extract_string_field(&input, "root").expect("input must have a string \"root\" field");

    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("reading the bound directory")
        .map(|entry| {
            entry
                .expect("reading a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();

    let mut output = String::from("{\"files\":[");
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(name);
        output.push('"');
    }
    output.push_str("]}");
    std::io::stdout()
        .write_all(output.as_bytes())
        .expect("writing stdout");
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let after_key = &json[json.find(&needle)? + needle.len()..];
    let after_colon = &after_key[after_key.find(':')? + 1..];
    let start = after_colon.find('"')? + 1;
    let rest = &after_colon[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}
