//! `--regenerate`: one pass that writes `src/generated.rs` and
//! `families.schema.json` from a single Lean snapshot, and does nothing when no
//! input changed since the last pass that completed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::generator::{self, GENERATED, ROOT};
use crate::{apply, write_if_changed};

/// Lake's build directory is per checkout and `lake clean` removes it with the
/// outputs the stamp vouches for.
const STAMP: &str = ".lake/build/gents-vocab-regeneration.sha256";
const SCHEMA: &str = "families.schema.json";
const MAX_PASSES: usize = 4;

/// Everything a regeneration reads: the Lean sources and toolchain pins, the
/// vocabulary crate (its decoders are the generator's oracle and its outputs
/// are part of the digest), the contract loader and the lockfile.
fn inputs_digest(repo: &Path) -> String {
    fn walk(root: &Path, dir: &Path, skip: &[&str], files: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                if !skip.iter().any(|name| entry.file_name() == *name) {
                    walk(root, &path, skip, files);
                }
            } else if kind.is_file() {
                files.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut files = Vec::new();
    for (dir, skip) in [
        ("crates/gents/proofs", &[".lake"][..]),
        ("crates/gents-lean-vocab", &["target"][..]),
        ("crates/gents-lean-contract", &["target"][..]),
    ] {
        walk(repo, &repo.join(dir), skip, &mut files);
    }
    files.push(PathBuf::from("Cargo.lock"));
    let mut hash = Sha256::new();
    for file in files {
        let bytes = fs::read(repo.join(&file)).unwrap();
        hash.update(file.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    format!("{:x}", hash.finalize())
}

/// Drops the snapshot root's fields that Lean no longer emits, with their
/// accessors, so the generator does not look for them in the snapshot.
fn prune_root_fields(generated: &str, snapshot: &serde_json::Value) -> (String, Vec<String>) {
    let lines: Vec<&str> = generated.lines().collect();
    let header = format!("pub struct {ROOT} {{");
    let Some(open) = lines.iter().position(|line| *line == header) else {
        return (generated.to_owned(), Vec::new());
    };
    let close = open
        + lines[open..]
            .iter()
            .position(|line| *line == "}")
            .expect("snapshot root struct closes");
    let mut drop = BTreeSet::new();
    let mut gone = Vec::new();
    let mut attributes = Vec::new();
    let mut index = open + 1;
    while index < close {
        let line = lines[index];
        if line.starts_with("    #[") {
            attributes.push(index);
            index += 1;
            continue;
        }
        let field = line
            .strip_prefix("    pub ")
            .and_then(|rest| rest.split_once(": "))
            .map(|(name, _)| name);
        let end = (index + 1..close)
            .find(|&next| lines[next].starts_with("    #[") || lines[next].starts_with("    pub "))
            .unwrap_or(close);
        if let Some(name) = field.filter(|name| snapshot.get(name).is_none()) {
            gone.push(name.to_owned());
            drop.extend(attributes.iter().copied());
            drop.extend(index..end);
        }
        attributes.clear();
        index = end;
    }
    for name in &gone {
        let body = format!("    &lean_contract_snapshot().{name}");
        for (at, window) in lines.windows(3).enumerate() {
            if window[0].starts_with("pub fn ")
                && window[0].ends_with('{')
                && window[1] == body
                && window[2] == "}"
            {
                drop.extend(at..at + 3);
            }
        }
    }
    if gone.is_empty() {
        return (generated.to_owned(), gone);
    }
    let mut kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !drop.contains(index))
        .map(|(_, line)| *line)
        .collect();
    if generated.ends_with('\n') {
        kept.push("");
    }
    (kept.join("\n"), gone)
}

fn ms(since: Instant) -> u128 {
    since.elapsed().as_millis()
}

pub fn run(src: &Path, snapshot_file: Option<&str>, force: bool) {
    let started = Instant::now();
    let crate_dir = src.parent().expect("src has a crate directory");
    let repo = crate_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate directory is under crates/");
    let stamp = gents_lean_contract::proofs_dir()
        .expect("Lean proofs directory")
        .join(STAMP);
    // A snapshot from a file is not derived from these inputs.
    let stamped = snapshot_file.is_none();
    if stamped && !force {
        let digest = inputs_digest(repo);
        if fs::read_to_string(&stamp).is_ok_and(|recorded| recorded == digest) {
            println!(
                "current: no Lean source, decoder or generator input changed since the last regeneration ({} ms)",
                ms(started)
            );
            return;
        }
    }

    let phase = Instant::now();
    let text = match snapshot_file {
        Some(path) => fs::read_to_string(path).expect("snapshot file"),
        None => {
            // The contract target's import closure only, not every proof.
            gents_lean_contract::load_contract_json().expect("Lean conformance snapshot")
        }
    };
    let snapshot_ms = ms(phase);
    let phase = Instant::now();
    let snapshot: serde_json::Value = serde_json::from_str(&text).expect("snapshot JSON");
    let parse_ms = ms(phase);

    let (schema, decoders) = std::thread::scope(|scope| {
        let schema = scope.spawn(|| {
            let phase = Instant::now();
            let text = gents_lean_vocab::family::render_family_schema(&snapshot)
                .unwrap_or_else(|error| panic!("family schema: {error}"));
            (text, ms(phase))
        });
        let phase = Instant::now();
        let generated_path = src.join(GENERATED);
        let (pruned, gone) =
            prune_root_fields(&fs::read_to_string(&generated_path).unwrap(), &snapshot);
        write_if_changed(&generated_path, &pruned);
        let mut passes = 0;
        loop {
            assert!(
                passes < MAX_PASSES,
                "decoders did not reach a fixpoint in {MAX_PASSES} passes"
            );
            passes += 1;
            let sources = generator::decoder_sources(src);
            let output = generator::generate(&snapshot, &sources, None, &BTreeSet::new());
            let settled = output.removals.is_empty();
            let written = apply(src, &sources, &output, true);
            if settled && !written.generated_changed {
                break;
            }
        }
        (
            schema.join().expect("schema thread"),
            (gone, passes, ms(phase)),
        )
    });
    let ((schema, schema_ms), (gone, passes, decoders_ms)) = (schema, decoders);
    let schema_changed = write_if_changed(&crate_dir.join(SCHEMA), &schema);

    if stamped {
        let digest = inputs_digest(repo);
        fs::create_dir_all(stamp.parent().unwrap()).unwrap();
        let staged = stamp.with_extension("staged");
        fs::write(&staged, &digest).unwrap();
        fs::rename(&staged, &stamp).unwrap();
    }
    println!("snapshot_ms: {snapshot_ms}");
    println!("parse_ms: {parse_ms}");
    println!("decoders_ms: {decoders_ms} ({passes} passes, root fields removed: {gone:?})");
    println!("schema_ms: {schema_ms} (changed: {schema_changed})");
    println!("total_ms: {}", ms(started));
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATED_FIXTURE: &str = "\
use std::sync::OnceLock;

pub fn lean_kept_cases() -> &'static [u32] {
    &lean_contract_snapshot().kept_cases
}

pub fn lean_gone_cases() -> &'static [u32] {
    &lean_contract_snapshot().gone_cases
}

pub struct Other {
    pub gone_cases: u32,
}

pub struct LeanContractSnapshot {
    pub kept_cases: Vec<u32>,
    #[serde(deserialize_with = \"required_nullable\")]
    pub gone_cases: Option<
        Vec<u32>,
    >,
    pub also_kept: String,
}
";

    #[test]
    fn root_fields_lean_no_longer_emits_are_pruned_with_their_accessors() {
        let snapshot = serde_json::json!({"kept_cases": [], "also_kept": ""});
        let (pruned, gone) = prune_root_fields(GENERATED_FIXTURE, &snapshot);
        assert_eq!(gone, ["gone_cases"]);
        assert_eq!(
            pruned,
            "\
use std::sync::OnceLock;

pub fn lean_kept_cases() -> &'static [u32] {
    &lean_contract_snapshot().kept_cases
}


pub struct Other {
    pub gone_cases: u32,
}

pub struct LeanContractSnapshot {
    pub kept_cases: Vec<u32>,
    pub also_kept: String,
}
"
        );
        let full = serde_json::json!({"kept_cases": [], "gone_cases": null, "also_kept": ""});
        assert_eq!(
            prune_root_fields(GENERATED_FIXTURE, &full),
            (GENERATED_FIXTURE.to_owned(), Vec::new())
        );
    }
}
