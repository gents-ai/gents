//! Docs conformance: the grounding documents' checkable claims are
//! invariants, and invariants get fenced.
//!
//! CLAUDE.md and README.md describe the present tree. Prose can't be proven,
//! but paths, links, and architectural claims can — so the checkable subset
//! fails CI instead of rotting silently. When one of these tests fails, fix
//! the tree or fix the document; both are real outcomes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // crates/defra-agent -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Backticked tokens in the grounding docs that look like repo paths must
/// exist in the tree (path rot is the most common failure mode of a
/// grounding document).
#[test]
fn grounding_doc_paths_resolve() {
    let root = repo_root();
    let mut missing = Vec::new();

    for doc in ["CLAUDE.md", "README.md"] {
        let text = read(&root.join(doc));
        for token in text.split('`').skip(1).step_by(2) {
            // A repo path: contains a slash, no spaces, and starts with a
            // known top-level directory. Module paths (a::b) and commands
            // are skipped.
            let looks_like_path = token.contains('/')
                && !token.contains(' ')
                && !token.contains('*')
                && !token.contains("::")
                && !token.starts_with("http")
                && ["crates/", "apps/", "docs/", "scripts/", "release/"]
                    .iter()
                    .any(|prefix| token.starts_with(prefix));
            if looks_like_path && !root.join(token).exists() {
                missing.push(format!("{doc}: `{token}`"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "grounding docs reference paths that do not exist (fix the doc or the tree):\n{}",
        missing.join("\n")
    );
}

/// Relative markdown links in README.md and docs/ must resolve.
#[test]
fn markdown_links_resolve() {
    let root = repo_root();
    let mut docs = vec![root.join("README.md")];
    if let Ok(entries) = std::fs::read_dir(root.join("docs")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "md") {
                docs.push(entry.path());
            }
        }
    }

    let mut broken = Vec::new();
    for doc in docs {
        let text = read(&doc);
        let base = doc.parent().unwrap_or(&root);
        // Hand-rolled `](target)` extraction — not worth a regex dependency.
        let mut rest = text.as_str();
        while let Some(start) = rest.find("](") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find(')') else { break };
            let mut target = &rest[..end];
            if let Some(anchor) = target.find('#') {
                target = &target[..anchor];
            }
            rest = &rest[end + 1..];
            if target.is_empty()
                || target.starts_with("http")
                || target.starts_with("mailto:")
                || target.contains(char::is_whitespace)
            {
                continue;
            }
            if !base.join(target).exists() {
                broken.push(format!("{}: ({target})", doc.display()));
            }
        }
    }

    assert!(
        broken.is_empty(),
        "markdown links that do not resolve:\n{}",
        broken.join("\n")
    );
}

/// CLAUDE.md claims the proofs hold zero `sorry`s. Fence it.
#[test]
fn proofs_contain_no_sorrys() {
    let root = repo_root();
    let proofs = root.join("crates/defra-agent/proofs/Proofs");
    let mut offenders = Vec::new();
    visit_lean(&proofs, &mut offenders);
    assert!(
        offenders.is_empty(),
        "`sorry` found in proofs (CLAUDE.md claims zero):\n{}",
        offenders.join("\n")
    );
}

fn visit_lean(dir: &Path, offenders: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_lean(&path, offenders);
        } else if path.extension().is_some_and(|ext| ext == "lean") {
            let text = read(&path);
            for (number, line) in text.lines().enumerate() {
                // Word-boundary match; skip comment mentions.
                let code = line.split("--").next().unwrap_or("");
                if code
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| w == "sorry")
                {
                    offenders.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
    }
}

/// CLAUDE.md claims rig's type vocabulary crosses into the runtime through
/// ONE seam (`llm::rig_compat`). Fence the claim: rig message/tool/hook
/// vocabulary may appear only in the converter module, the named seam files
/// (which consume rig stream items per decision D3), and test code that
/// feeds the seam.
#[test]
fn rig_vocabulary_confined_to_the_seam() {
    let root = repo_root();
    let allowed: BTreeSet<&str> = [
        // The seam itself.
        "crates/defra-agent/src/llm/rig_compat.rs",
        // This fence (the marker strings below would self-match).
        "crates/defra-agent/tests/docs_conformance.rs",
        // Stream consumers: yield/accept rig items by design (D3).
        "crates/defra-agent/src/agent/loop_stream.rs",
        "crates/defra-agent/src/agent/stream_processor.rs",
        // Tests that construct rig items to feed the seam, and the golden
        // byte-compat suite (rig is a dev-dependency there until Layer A).
        "crates/defra-agent/src/agent/loop_stream/tests.rs",
        "crates/defra-agent/src/agent/stream_processor/tests.rs",
        "crates/defra-agent/src/compaction/tests.rs",
        "crates/defra-agent-protocol/src/message.rs",
    ]
    .into_iter()
    .collect();

    let markers = [
        "rig::completion::message::",
        "rig::completion::Message",
        "rig::one_or_many",
        // The owned tool-trait surface (Tool/ToolDyn/ToolError); rig's
        // ToolSetError remains legitimate Layer-A error vocabulary.
        "rig::tool::ToolDyn",
        "rig::tool::ToolError",
        "rig::tool::Tool ",
        "rig::tool::Tool;",
        "rig::tool::Tool,",
        "rig::tool::Tool>",
        "rig::agent::HookAction",
        "rig::agent::ToolCallHookAction",
        "rig::message::ToolChoice",
    ];

    let mut violations = Vec::new();
    visit_rust(
        &root.join("crates"),
        &root,
        &markers,
        &allowed,
        &mut violations,
    );
    visit_rust(
        &root.join("apps"),
        &root,
        &markers,
        &allowed,
        &mut violations,
    );

    assert!(
        violations.is_empty(),
        "rig type vocabulary escaped the seam (CLAUDE.md claims one seam; \
         either route through llm::rig_compat or update the allowlist AND the doc):\n{}",
        violations.join("\n")
    );
}

fn visit_rust(
    dir: &Path,
    root: &Path,
    markers: &[&str],
    allowed: &BTreeSet<&str>,
    violations: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            visit_rust(&path, root, markers, allowed, violations);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            if allowed.contains(rel.as_str()) {
                continue;
            }
            let text = read(&path);
            for (number, line) in text.lines().enumerate() {
                // Doc comments may mention rig history; only flag code.
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if markers.iter().any(|marker| line.contains(marker)) {
                    violations.push(format!("{rel}:{}", number + 1));
                }
            }
        }
    }
}

/// CLAUDE.md claims the protocol crate's persisted vocabulary is rig-free at
/// runtime. Fence it: rig-core must not appear in its [dependencies].
#[test]
fn protocol_crate_runtime_is_rig_free() {
    let root = repo_root();
    let manifest = read(&root.join("crates/defra-agent-protocol/Cargo.toml"));
    let dependencies = manifest
        .split("[dev-dependencies]")
        .next()
        .unwrap_or(&manifest);
    let has_runtime_rig = dependencies
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .any(|line| line.trim_start().starts_with("rig-core"));
    assert!(
        !has_runtime_rig,
        "defra-agent-protocol gained a runtime rig-core dependency; \
         the persisted vocabulary must stay rig-free (CLAUDE.md claim)"
    );
}
