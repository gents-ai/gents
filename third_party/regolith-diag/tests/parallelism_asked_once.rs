//! Only `per_thread::parallelism` asks the OS for the core count.
//!
//! On Linux that answer reads the cgroup's CPU quota, about a dozen system
//! calls. An open sized its commit ring by asking each time, and a process
//! that opens many databases paid them on every open. This pins at the
//! source that every other sizing reads the answer cached there; the count
//! of OS queries is `per_thread`'s own test.

// Native-only. wasm-pack builds every test target for wasm32, and this reads
// the source tree.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

const ASK: &str = "available_parallelism(";

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_per_thread_asks_the_os_for_the_core_count() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut askers = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).expect("read a source file");
        let relative = file.strip_prefix(root).expect("under the crate root");
        for (number, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if !code.starts_with("//") && code.contains(ASK) {
                askers.push(format!("{}:{}", relative.display(), number + 1));
            }
        }
    }
    let the_one_place = ["src", "per_thread.rs"].join(std::path::MAIN_SEPARATOR_STR);
    assert_eq!(askers.len(), 1, "OS core-count queries: {askers:#?}");
    assert!(
        askers[0].starts_with(&the_one_place),
        "OS core-count queries: {askers:#?}"
    );
}
