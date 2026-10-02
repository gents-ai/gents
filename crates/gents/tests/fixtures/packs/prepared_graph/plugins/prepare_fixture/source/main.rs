// Prepare plugin for the prepared_graph fixture: turns the `git_diff` host
// fact into one evidence document and the job input, carrying the diff's
// head sha through so a test can prove the host step ran. No JSON crate:
// the only field read is one string.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("reading stdin");
    let head = extract_string_field(&input, "head_sha").expect("host.git_diff.head_sha");
    let nonce = extract_string_field(&input, "nonce").expect("nonce");

    let output = format!(
        concat!(
            "{{\"input\":{{\"repository_path\":\".\",\"head_ref\":\"{head}\",",
            "\"evidence_id\":\"{nonce}\",\"summary\":\"prepared at {head}\"}},",
            "\"documents\":[{{\"collection\":\"FixtureEvidence\",\"fields\":",
            "{{\"evidence_id\":\"{nonce}\",\"head_ref\":\"{head}\",\"note\":\"prepared\"}}}}]}}"
        ),
        head = head,
        nonce = nonce
    );
    std::io::stdout()
        .write_all(output.as_bytes())
        .expect("writing stdout");
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let rest = &json[json.find(&needle)? + needle.len()..];
    Some(rest[..rest.find('"')?].to_owned())
}
