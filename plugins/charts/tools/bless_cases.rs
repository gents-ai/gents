//! Records the expected output of plugin cases by running them natively.
//! A case is {"input": ..., "bind": "<folder>", "expect": ...}; this fills in
//! `expect` where it is missing (or everywhere with --all). Review what it
//! writes before committing: look at the pictures and read the SVG, then let
//! `gents pack test` reproduce it through the real WebAssembly host.
//!
//! Usage: cargo run --release --example bless_cases -- [--all] <tests-folder>...

use std::path::Path;

use serde_json::Value;

fn bless(path: &Path, all: bool) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut case: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if case.get("expect").is_some() && !all {
        return Ok(false);
    }
    let mut input = case.get("input").cloned().unwrap_or(Value::Null);
    if let Some(bind) = case.get("bind").and_then(Value::as_str) {
        let dir = path.parent().unwrap_or(Path::new(".")).join(bind);
        let dir = dir
            .canonicalize()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        input["path"] = Value::String(dir.to_string_lossy().into_owned());
    }
    let out = charts::run(&input.to_string()).map_err(|e| e.0)?;
    case["expect"] = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let mut pretty = serde_json::to_string_pretty(&case).map_err(|e| e.to_string())?;
    pretty.push('\n');
    std::fs::write(path, pretty).map_err(|e| e.to_string())?;
    Ok(true)
}

fn main() {
    let mut all = false;
    let mut dirs = Vec::new();
    for a in std::env::args().skip(1) {
        if a == "--all" {
            all = true;
        } else {
            dirs.push(a);
        }
    }
    let mut failed = false;
    for dir in dirs {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "json"))
        {
            match bless(f, all) {
                Ok(true) => println!("blessed {}", f.display()),
                Ok(false) => {}
                Err(e) => {
                    failed = true;
                    eprintln!("{}: {e}", f.display());
                }
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
