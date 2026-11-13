use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("crates/gents/proofs/lakefile.lean").exists())
        .expect("repository root should contain crates/gents/proofs/lakefile.lean")
        .to_path_buf()
}

fn production_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack = [
        "gents",
        "gents-loop",
        "gents-inference",
        "gents-session",
        "gents-tools",
        "gents-toolcalls",
        "gents-automation",
        "gents-observe",
    ]
    .into_iter()
    .map(|name| root.join("crates").join(name).join("src"))
    .collect::<Vec<_>>();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("path under repo root")
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = relative.rsplit('/').next().unwrap_or("");
            if relative.ends_with("/tests.rs")
                || relative.contains("/tests/")
                || file_name.ends_with("_tests.rs")
                || file_name.ends_with("_test.rs")
                || relative.contains("test_support")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            out.push((relative, source));
        }
    }
    out.sort();
    out
}

fn files_containing(needles: &[&str]) -> BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|(_, source)| needles.iter().any(|needle| source.contains(needle)))
        .map(|(path, _)| path)
        .collect()
}

#[test]
fn provider_invocations_are_confined_to_the_owned_loop_seam() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream.rs".to_string(),
        "crates/gents-inference/src/llm/rig_compat.rs".to_string(),
    ]);
    let found = files_containing(&[".completion_request(", ".completion(", ".stream("]);
    assert_eq!(
        found, expected,
        "provider invocation escaped the owned-loop seam; every completion request \
         must be born in run_loop_stream, which sanitizes loaded history at entry"
    );
}

#[test]
fn owned_loop_entry_points_are_registered() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream/one_shot.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/inference.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title/review.rs".to_string(),
        "crates/gents-loop/src/compaction_rolling.rs".to_string(),
        "crates/gents/src/runtime/oneshot.rs".to_string(),
    ]);
    let found = files_containing(&[
        "run_loop_stream(",
        "run_loop_stream::<",
        "run_loop_to_text(",
        "run_loop_to_text::<",
        "run_loop_to_typed(",
        "run_loop_to_typed::<",
    ]);
    assert_eq!(
        found, expected,
        "the set of owned-loop entry points changed; a new completion path must be \
         reviewed against the sanitize-at-entry chokepoint"
    );
}

#[test]
fn empty_skill_catalog_contributes_no_reminders() {
    use gents::turn::prompt::LayeredPromptBuilder;

    let builder =
        LayeredPromptBuilder::for_agent("literal preamble", "fence", &["bash"], false, &[]);

    assert!(
        builder.selected_skill_reminders(&[]).is_empty(),
        "an empty whitelist must not produce skill reminders"
    );

    assert!(
        builder
            .selected_skill_reminders(&["not-installed".to_string()])
            .is_empty(),
        "an unresolvable selection must not produce skill reminders"
    );
    assert!(
        builder
            .selected_skill_reminders(&["  ".to_string()])
            .is_empty(),
        "a blank selection entry must not produce skill reminders"
    );
}
