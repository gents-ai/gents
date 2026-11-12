#![expect(clippy::print_stdout, reason = "command-line report")]

mod generator;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use generator::{GENERATED, ROOT, Source};

fn rust_files(dir: &Path, base: &Path, out: &mut Vec<Source>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                rust_files(&path, base, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(Source {
                path: path.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                text: fs::read_to_string(&path).unwrap(),
            });
        }
    }
}

pub fn decoder_sources(src: &Path) -> Vec<Source> {
    let mut sources = Vec::new();
    rust_files(src, src, &mut sources);
    sources.retain(|source| !source.path.ends_with("tests.rs"));
    sources
}

fn identifiers(text: &str) -> BTreeSet<&str> {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|word| !word.is_empty())
        .collect()
}

fn layer_keys(sources: &[Source], crates_dir: &Path, layer: &[&str]) -> BTreeSet<String> {
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for source in sources {
        let file = syn::parse_file(&source.text).unwrap();
        for item in file.items {
            match item {
                syn::Item::Struct(item) if item.ident == ROOT => {
                    for field in item.fields {
                        let key = field.ident.unwrap().to_string();
                        names.insert(key.clone(), key);
                    }
                }
                syn::Item::Fn(function) => {
                    let body = quote::ToTokens::to_token_stream(&function.block).to_string();
                    if let Some(rest) = body.strip_prefix("{ & lean_contract_snapshot () . ") {
                        let key = rest.trim_end_matches(" }").to_owned();
                        names.insert(function.sig.ident.to_string(), key);
                    }
                }
                _ => {}
            }
        }
    }
    let mut consumers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut crates: Vec<PathBuf> = fs::read_dir(crates_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    crates.sort();
    for path in crates {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == "gents-lean-vocab" {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&path, &path, &mut files);
        for file in files {
            for word in identifiers(&file.text) {
                if let Some(key) = names.get(word) {
                    consumers.entry(key.clone()).or_default().insert(name.clone());
                }
            }
        }
    }
    consumers
        .into_iter()
        .filter(|(_, crates)| crates.iter().all(|name| layer.contains(&name.as_str())))
        .map(|(key, _)| key)
        .collect()
}

fn main() {
    let started = std::time::Instant::now();
    let mut args = std::env::args().skip(1);
    let mut snapshot = None;
    let mut src = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut write = false;
    let mut report = None;
    let mut layer = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--snapshot" => snapshot = args.next(),
            "--src" => src = PathBuf::from(args.next().expect("--src DIR")),
            "--report" => report = args.next(),
            "--layer" => layer = args.next(),
            "--write" => write = true,
            other => panic!(
                "unknown argument {other}; usage: --snapshot FILE [--src DIR] [--layer crate,crate] [--report FILE] [--write]"
            ),
        }
    }
    let snapshot = fs::read_to_string(snapshot.expect("--snapshot FILE")).expect("snapshot file");
    let snapshot = serde_json::from_str(&snapshot).expect("snapshot JSON");
    let sources = decoder_sources(&src);
    let only = layer.map(|layer: String| {
        let crates_dir = src.parent().unwrap().parent().unwrap().to_path_buf();
        layer_keys(&sources, &crates_dir, &layer.split(',').collect::<Vec<_>>())
    });
    let output = generator::generate(&snapshot, &sources, only.as_ref());

    let mut deleted = 0;
    let mut emptied = Vec::new();
    for (path, ranges) in &output.removals {
        deleted += ranges.iter().map(|(start, end)| end - start + 1).sum::<usize>();
        if !write {
            continue;
        }
        let text = &sources.iter().find(|source| &source.path == path).unwrap().text;
        match generator::prune(text, ranges, path != "lib.rs") {
            Some(text) => fs::write(src.join(path), text).unwrap(),
            None => {
                fs::remove_file(src.join(path)).unwrap();
                emptied.push(path.trim_end_matches(".rs").to_owned());
            }
        }
    }
    if write {
        fs::write(src.join(GENERATED), &output.generated).unwrap();
        let lib = fs::read_to_string(src.join("lib.rs")).unwrap();
        let mut lines: Vec<String> = lib
            .lines()
            .filter(|line| {
                !emptied.iter().any(|module| {
                    [format!("mod {module};"), format!("pub mod {module};"), format!("pub use {module}::*;")]
                        .contains(&(*line).to_owned())
                })
            })
            .map(str::to_owned)
            .collect();
        if !lines.iter().any(|line| line == "mod generated;") {
            let at = lines
                .iter()
                .position(|line| line.starts_with("mod ") || line.starts_with("pub mod "))
                .expect("lib.rs declares modules");
            lines.splice(
                at..at,
                ["#[rustfmt::skip]", "mod generated;", "pub use generated::*;", ""].map(str::to_owned),
            );
        }
        fs::write(src.join("lib.rs"), lines.join("\n") + "\n").unwrap();
    }
    let mut summary = output.report.clone();
    summary["hand_written_lines_removed"] = deleted.into();
    summary["generated_lines"] = output.generated.lines().count().into();
    summary["emptied_modules"] = emptied.into();
    summary["layer_keys"] = only.map_or(0, |only| only.len()).into();
    summary["elapsed_ms"] = (started.elapsed().as_millis() as u64).into();
    if let Some(path) = report {
        fs::write(path, serde_json::to_string_pretty(&summary).unwrap()).unwrap();
    }
    for (key, value) in summary.as_object().unwrap() {
        match value {
            serde_json::Value::Object(map) => {
                println!("{key}: {}", map.len());
                if key == "findings" {
                    for (class, list) in map {
                        println!("  {class}: {}", list.as_array().map_or(0, Vec::len));
                    }
                }
            }
            serde_json::Value::Array(list) => println!("{key}: {}", list.len()),
            other => println!("{key}: {other}"),
        }
    }
}
