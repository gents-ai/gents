use std::fmt::Write as _;

use sha2::{Digest, Sha256};

fn digest(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let mut result = String::new();
    for byte in digest.finalize() {
        write!(&mut result, "{byte:02x}").expect("writing to a string cannot fail");
    }
    result
}

pub(crate) fn root_prefix(node: &str) -> String {
    format!("gents-plugin-root-v1:{}:", digest(&[node]))
}

pub(crate) fn index(node: &str) -> String {
    format!("gents-plugin-index-v1:{}", digest(&[node]))
}

pub(crate) fn root(node: &str, coordinate: &str, pin: Option<&str>) -> String {
    let kind = if pin.is_some() { "digest" } else { "current" };
    format!(
        "{}{}",
        root_prefix(node),
        digest(&[coordinate, kind, pin.unwrap_or_default()])
    )
}

pub(crate) fn member_prefix(node: &str) -> String {
    format!("gents-plugin-member-v1:{}:", digest(&[node]))
}

pub(crate) fn member(node: &str, root: &str, tools: &str, context: &str, agent: &str) -> String {
    format!(
        "{}{}",
        member_prefix(node),
        digest(&[root, tools, context, agent])
    )
}

pub(crate) fn object(source: &str) -> String {
    let mut result = String::from("source.");
    for byte in source.as_bytes() {
        write!(&mut result, "{byte:02x}").expect("writing to a string cannot fail");
    }
    result
}
