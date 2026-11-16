use std::{fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=afb.toml");
    // Gents stages this shared source once per tool and generates its package
    // manifest there. Selecting at build time keeps model arguments unchanged.
    let tool = if Path::new("afb.toml").exists() {
        let manifest = fs::read_to_string("afb.toml").expect("read generated afb.toml");
        let mut package = false;
        let mut name = None;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                package = line == "[package]";
            } else if package {
                if let Some((key, value)) = line.split_once('=') {
                    if key.trim() == "name" {
                        name = Some(value.trim().trim_matches('"').to_string());
                    }
                }
            }
        }
        name.expect("generated afb.toml has package.name")
    } else {
        // Native tests call run(tool, args) directly; cargo run defaults to fetch.
        "web_fetch".to_owned()
    };
    assert!(
        matches!(
            tool.as_str(),
            "web_search" | "web_fetch" | "web_crawl" | "browser_exec"
        ),
        "unexpected web plugin name: {tool}"
    );
    println!("cargo:rustc-env=WEB_TOOL={tool}");
}
