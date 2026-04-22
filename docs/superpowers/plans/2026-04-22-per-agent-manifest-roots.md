# Per-Agent Manifest Roots Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the aggregate-JSON manifest format with per-document subdirectories and adjacent Markdown prompt sidecars, and add a manifest-root writer to `config export`.

**Architecture:** The `defra-agent-cli` loader is rewritten to scan `<collection>/<handle>/object.json` subdirectories and hydrate `./*.md` sidecar references in `AgentBehavior.system_prompt` and `Task.prompt_template`. A new writer module produces the same layout from a `DesiredStateManifest`, enforcing filesystem-safe handles. The `Collection` enum in `defra-agent` narrows `file_name` / `dir_name` so each variant has exactly one shape.

**Tech Stack:** Rust, serde, serde_json, anyhow, clap, tempfile (test-only).

**Spec:** `docs/superpowers/specs/2026-04-22-per-agent-manifest-roots-design.md`

**Issue:** sourcenetwork/defra-agent#67

**Out of scope:** `config import` (keeps accepting JSON bundle files unchanged), any sidecar-eligible field beyond `system_prompt` and `prompt_template`, Lean spec changes.

---

### Task 1: `Collection` enum narrowing + `HasUniqueId` trait

**Files:**
- Modify: `crates/defra-agent/src/collection.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs` (temporary, fix compile)

**Why together:** changing the enum signatures breaks the current loader compile. Doing it alongside a minimal compile-fix-with-`expect` keeps each commit buildable; the real loader rewrite lands in Task 4.

- [ ] **Step 1: Add invariant test to `collection.rs`**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn exactly_one_of_file_or_dir_name() {
    for variant in Collection::ALL {
        let has_file = variant.file_name().is_some();
        let has_dir = variant.dir_name().is_some();
        assert!(
            has_file ^ has_dir,
            "Collection::{variant:?} must return Some from exactly one of file_name()/dir_name()"
        );
    }
}
```

- [ ] **Step 2: Run the test to see it fails (file_name still returns `&str`, not `Option`)**

Run: `cargo test -p defra-agent --lib collection::tests::exactly_one_of_file_or_dir_name -- --nocapture`
Expected: FAIL to compile (`.is_some()` called on `&str`).

- [ ] **Step 3: Narrow `file_name` and broaden `dir_name` in `collection.rs`**

Replace the two methods:

```rust
impl Collection {
    /// Top-level file name, only for collections that don't use a directory form.
    pub fn file_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => Some("agent-principal.json"),
            _ => None,
        }
    }

    /// Directory name for the per-doc subdirectory form.
    pub fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => None,
            Collection::AgentBehavior => Some("agent-behaviors"),
            Collection::ToolSelection => Some("tool-selections"),
            Collection::InferenceBackend => Some("inference-backends"),
            Collection::InferenceProfile => Some("inference-profiles"),
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::Task => Some("tasks"),
            Collection::Schedule => Some("schedules"),
        }
    }
}
```

Also update the existing `all_collections_have_distinct_file_names` test in the same file — change the type to `BTreeSet<Option<&str>>` so it still compiles:

```rust
#[test]
fn all_collections_have_distinct_file_or_dir_names() {
    use std::collections::BTreeSet;
    let names: BTreeSet<&str> = Collection::ALL
        .iter()
        .map(|c| c.file_name().or(c.dir_name()).expect("every variant has one"))
        .collect();
    assert_eq!(names.len(), Collection::ALL.len());
}
```

Delete the old `all_collections_have_distinct_file_names` test (replaced by the one above).

- [ ] **Step 4: Fix `load.rs` compile (temporary)**

In `crates/defra-agent-cli/src/desired_state/load.rs`, every `Collection::X.file_name()` call now returns `Option<&str>`. Suffix each one with `.expect("shimmed in Task 1, rewritten in Task 4")`. There are seven such sites around lines 74–120.

Also update the two `.dir_name().expect("tasks has a dir form")` / `.expect("schedules has a dir form")` call sites — these already expect `Option`, so they stay as-is.

- [ ] **Step 5: Add `HasUniqueId` trait in `desired_state/mod.rs`**

Append to `mod.rs`:

```rust
/// Trait implemented by `Desired*` structs that live in a per-document
/// directory form. Used by the loader to cross-check directory names
/// against the unique-id field inside `object.json`.
pub(crate) trait HasUniqueId {
    fn unique_id(&self) -> &str;
}

impl HasUniqueId for DesiredAgentBehavior {
    fn unique_id(&self) -> &str { &self.behavior_id }
}
impl HasUniqueId for DesiredToolSelection {
    fn unique_id(&self) -> &str { &self.selection_id }
}
impl HasUniqueId for DesiredInferenceBackend {
    fn unique_id(&self) -> &str { &self.backend_id }
}
impl HasUniqueId for DesiredInferenceProfile {
    fn unique_id(&self) -> &str { &self.profile_id }
}
impl HasUniqueId for DesiredToolServiceRegistry {
    fn unique_id(&self) -> &str { &self.service_id }
}
impl HasUniqueId for DesiredTask {
    fn unique_id(&self) -> &str { &self.task_id }
}
impl HasUniqueId for DesiredSchedule {
    fn unique_id(&self) -> &str { &self.schedule_id }
}
```

- [ ] **Step 6: Build and run all affected tests**

Run: `cargo build --all-targets && cargo test -p defra-agent --lib collection`
Expected: build succeeds; both enum tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/src/collection.rs \
        crates/defra-agent-cli/src/desired_state/mod.rs \
        crates/defra-agent-cli/src/desired_state/load.rs
git commit -m "refactor: narrow Collection::file_name/dir_name + HasUniqueId trait (#67)"
```

---

### Task 2: Loader — sidecar hydration helper

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs` (append helper)
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs` (append tests)

- [ ] **Step 1: Write failing tests in `desired_state/tests.rs`**

Append to the existing `mod tests`:

```rust
#[test]
fn hydrate_sidecar_replaces_dot_slash_path_with_file_contents() {
    use tempfile::tempdir;
    use std::fs;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("prompt.md"), "You are a helpful agent.").unwrap();

    let mut value = Some("./prompt.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_leaves_literal_string_untouched() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("You are a helpful agent.".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_ignores_absolute_path() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("/etc/hosts".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("/etc/hosts"));
}

#[test]
fn hydrate_sidecar_ignores_parent_relative_path() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("../elsewhere.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("../elsewhere.md"));
}

#[test]
fn hydrate_sidecar_errors_when_file_missing() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("./missing.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("sidecar path does not resolve"), "got: {err}");
    assert!(err.contains("missing.md"), "got: {err}");
}

#[test]
fn hydrate_sidecar_errors_on_non_utf8() {
    use tempfile::tempdir;
    use std::fs;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("bad.md"), &[0xff, 0xfe, 0xfd]).unwrap();
    let mut value = Some("./bad.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("not valid UTF-8"), "got: {err}");
}

#[test]
fn hydrate_sidecar_is_noop_on_none() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value: Option<String> = None;
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert!(value.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail to compile (`hydrate_sidecar` undefined)**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::hydrate_sidecar`
Expected: FAIL to compile (`cannot find function 'hydrate_sidecar'`).

- [ ] **Step 3: Implement `hydrate_sidecar` in `load.rs`**

Append to `crates/defra-agent-cli/src/desired_state/load.rs`:

```rust
/// Hydrate a sidecar-eligible string field. If `value` is `Some(s)` where
/// `s` starts with `./`, treat the rest as a path relative to `json_dir`,
/// read the file as UTF-8, and replace `*value` with the file contents.
/// Any other case (None, absolute path, `../` prefix, literal string) is
/// a no-op.
pub(crate) fn hydrate_sidecar(
    value: &mut Option<String>,
    json_dir: &Path,
) -> Result<(), String> {
    let Some(current) = value.as_deref() else { return Ok(()) };
    if !current.starts_with("./") {
        return Ok(());
    }
    let rel = &current[2..];
    let sidecar_path = json_dir.join(rel);
    let bytes = fs::read(&sidecar_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "sidecar path does not resolve: {} (referenced from {})",
                sidecar_path.display(),
                json_dir.display()
            )
        } else {
            format!("reading {} failed: {error}", sidecar_path.display())
        }
    })?;
    let body = String::from_utf8(bytes).map_err(|_| {
        format!("sidecar is not valid UTF-8: {}", sidecar_path.display())
    })?;
    *value = Some(body);
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify all seven pass**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::hydrate_sidecar`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/load.rs \
        crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "feat: add hydrate_sidecar helper for prompt file references (#67)"
```

---

### Task 3: Loader — per-doc directory scan

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

- [ ] **Step 1: Write failing tests**

Append to `desired_state/tests.rs`:

```rust
mod load_per_doc_collection {
    use std::fs;
    use tempfile::tempdir;
    use crate::desired_state::load::load_per_doc_collection;
    use crate::desired_state::{DesiredAgentBehavior, HasUniqueId};
    use defra_agent::Collection;

    fn write_behavior_dir(root: &std::path::Path, handle: &str, behavior_id: &str) {
        let dir = root.join("agent-behaviors").join(handle);
        fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "behavior_id": behavior_id,
            "agent_did": "did:key:example",
            "enabled": true,
        });
        fs::write(dir.join("object.json"), serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    }

    #[test]
    fn loads_one_document_per_subdir() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "default", "default");
        write_behavior_dir(tmp.path(), "other", "other");

        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(result.len(), 2);
        let ids: Vec<&str> = result.iter().map(DesiredAgentBehavior::unique_id).collect();
        assert!(ids.contains(&"default") && ids.contains(&"other"));
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let tmp = tempdir().unwrap();
        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty());
        assert!(result.is_empty());
    }

    #[test]
    fn missing_object_json_is_error() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("agent-behaviors").join("default")).unwrap();
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("is missing object.json"), "got: {:?}", errors);
    }

    #[test]
    fn handle_mismatch_is_error() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "on-disk-name", "id-inside-json");
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not match behavior_id"), "got: {:?}", errors);
        assert!(errors[0].contains("on-disk-name"));
        assert!(errors[0].contains("id-inside-json"));
    }

    #[test]
    fn duplicate_unique_id_is_error() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "alpha", "shared");
        write_behavior_dir(tmp.path(), "beta", "shared");
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(
            errors.iter().any(|e| e.contains("duplicate behavior_id 'shared'")),
            "got: {:?}",
            errors
        );
    }

    #[test]
    fn unknown_sibling_files_are_ignored() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "default", "default");
        fs::write(
            tmp.path().join("agent-behaviors").join("default").join("README.md"),
            "notes",
        )
        .unwrap();
        fs::write(
            tmp.path().join("agent-behaviors").join("default").join(".DS_Store"),
            "",
        )
        .unwrap();
        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn non_directory_collection_path_is_error() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("agent-behaviors"), "not a dir").unwrap();
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(
            errors.iter().any(|e| e.contains("is not a directory")),
            "got: {:?}",
            errors
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::load_per_doc_collection`
Expected: FAIL (`load_per_doc_collection` undefined).

- [ ] **Step 3: Implement `load_per_doc_collection` in `load.rs`**

Append:

```rust
use super::HasUniqueId;

/// Scan `<root>/<collection.dir_name()>/` for per-document subdirectories
/// of the form `<handle>/object.json` and parse each into `T`.
///
/// Errors are accumulated into `errors`; the function always returns a
/// `Vec<T>` containing every document it could successfully parse.
pub(crate) fn load_per_doc_collection<T>(
    root: &Path,
    collection: Collection,
    errors: &mut Vec<String>,
) -> Vec<T>
where
    T: for<'de> Deserialize<'de> + HasUniqueId,
{
    let dir_name = collection
        .dir_name()
        .expect("load_per_doc_collection called with a non-directory collection");
    let collection_path = root.join(dir_name);
    if !collection_path.exists() {
        return Vec::new();
    }
    if !collection_path.is_dir() {
        errors.push(format!(
            "manifest collection path is not a directory: {}",
            collection_path.display()
        ));
        return Vec::new();
    }

    let entries = match fs::read_dir(&collection_path) {
        Ok(iter) => iter,
        Err(error) => {
            errors.push(format!(
                "reading {} failed: {error}",
                collection_path.display()
            ));
            return Vec::new();
        }
    };

    let mut subdirs: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!(
                    "reading {} failed: {error}",
                    collection_path.display()
                ));
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        subdirs.push((name.to_string(), path));
    }
    subdirs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut docs: Vec<T> = Vec::with_capacity(subdirs.len());
    let mut id_to_handle: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for (handle, subdir_path) in subdirs {
        let object_path = subdir_path.join("object.json");
        if !object_path.exists() {
            errors.push(format!(
                "per-doc dir is missing object.json: {}",
                subdir_path.display()
            ));
            continue;
        }
        let bytes = match fs::read(&object_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                errors.push(format!("reading {} failed: {error}", object_path.display()));
                continue;
            }
        };
        let parsed: T = match serde_json::from_slice(&bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                errors.push(format!("invalid {}: {error}", object_path.display()));
                continue;
            }
        };
        if parsed.unique_id() != handle {
            errors.push(format!(
                "directory name '{handle}' does not match {} '{}' in {}",
                collection.unique_field(),
                parsed.unique_id(),
                object_path.display()
            ));
            continue;
        }
        if let Some(prior) = id_to_handle.get(parsed.unique_id()) {
            errors.push(format!(
                "duplicate {} '{}' across {}/ and {}/",
                collection.unique_field(),
                parsed.unique_id(),
                prior,
                handle
            ));
            continue;
        }
        id_to_handle.insert(parsed.unique_id().to_string(), handle);
        docs.push(parsed);
    }
    docs
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::load_per_doc_collection`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/load.rs \
        crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "feat: add load_per_doc_collection with mismatch and duplicate checks (#67)"
```

---

### Task 4: Loader — rewrite `load_manifest_root` and tests

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

- [ ] **Step 1: Rewrite `load_manifest_root` in `load.rs`**

Replace the body of `load_manifest_root` (lines 19–173 in the current file). The new version:

```rust
pub(crate) fn load_manifest_root(
    root: &Path,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    let root_display = root.display().to_string();
    let mut errors = Vec::new();

    if !root.exists() {
        errors.push(format!("manifest root does not exist: {root_display}"));
        return (None, empty_report(root_display, errors));
    }
    if !root.is_dir() {
        errors.push(format!("manifest root is not a directory: {root_display}"));
        return (None, empty_report(root_display, errors));
    }

    let principal = load_agent_principal(root, &mut errors);

    let mut agent_behaviors: Vec<DesiredAgentBehavior> =
        load_per_doc_collection(root, Collection::AgentBehavior, &mut errors);
    let tool_selections: Vec<DesiredToolSelection> =
        load_per_doc_collection(root, Collection::ToolSelection, &mut errors);
    let inference_backends: Vec<DesiredInferenceBackend> =
        load_per_doc_collection(root, Collection::InferenceBackend, &mut errors);
    let inference_profiles: Vec<DesiredInferenceProfile> =
        load_per_doc_collection(root, Collection::InferenceProfile, &mut errors);
    let tool_service_registries: Vec<DesiredToolServiceRegistry> =
        load_per_doc_collection(root, Collection::ToolServiceRegistry, &mut errors);
    let mut tasks: Vec<DesiredTask> =
        load_per_doc_collection(root, Collection::Task, &mut errors);
    let schedules: Vec<DesiredSchedule> =
        load_per_doc_collection(root, Collection::Schedule, &mut errors);

    // Hydrate sidecars AFTER collection parse but BEFORE normalize/validate.
    for behavior in &mut agent_behaviors {
        let dir = per_doc_dir(root, Collection::AgentBehavior, behavior.unique_id());
        if let Err(error) = hydrate_sidecar(&mut behavior.system_prompt, &dir) {
            errors.push(error);
        }
    }
    for task in &mut tasks {
        let dir = per_doc_dir(root, Collection::Task, task.unique_id());
        let mut wrapped = Some(std::mem::take(&mut task.prompt_template));
        if let Err(error) = hydrate_sidecar(&mut wrapped, &dir) {
            errors.push(error);
        }
        task.prompt_template = wrapped.unwrap_or_default();
    }

    let counts = DesiredStateCounts {
        agent_principal: usize::from(principal.is_some()),
        agent_behaviors: agent_behaviors.len(),
        tool_selections: tool_selections.len(),
        inference_backends: inference_backends.len(),
        inference_profiles: inference_profiles.len(),
        tool_service_registries: tool_service_registries.len(),
        tasks: tasks.len(),
        schedules: schedules.len(),
    };

    let agent_did = principal.as_ref().map(|p| p.agent_did.clone());

    let manifest = principal.map(|principal| {
        let mut manifest = DesiredStateManifest {
            agent_principal: principal,
            agent_behaviors,
            tool_selections,
            inference_backends,
            inference_profiles,
            tool_service_registries,
            tasks,
            schedules,
        };
        normalize_manifest(&mut manifest);
        validate_manifest(&manifest, &mut errors);
        manifest
    });

    (
        manifest,
        DesiredStateValidationReport {
            status: if errors.is_empty() { "validated" } else { "invalid" },
            ok: errors.is_empty(),
            root: root_display,
            agent_did,
            counts,
            errors,
        },
    )
}

fn empty_report(
    root_display: String,
    errors: Vec<String>,
) -> DesiredStateValidationReport {
    DesiredStateValidationReport {
        status: "invalid",
        ok: false,
        root: root_display,
        agent_did: None,
        counts: DesiredStateCounts {
            agent_principal: 0,
            agent_behaviors: 0,
            tool_selections: 0,
            inference_backends: 0,
            inference_profiles: 0,
            tool_service_registries: 0,
            tasks: 0,
            schedules: 0,
        },
        errors,
    }
}

fn load_agent_principal(
    root: &Path,
    errors: &mut Vec<String>,
) -> Option<DesiredAgentPrincipal> {
    let file_name = Collection::AgentPrincipal
        .file_name()
        .expect("AgentPrincipal has a top-level file");
    let path = root.join(file_name);
    if !path.exists() {
        errors.push(format!(
            "required manifest file is missing: {}",
            path.display()
        ));
        return None;
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!("reading {} failed: {error}", path.display()));
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("invalid {}: {error}", path.display()));
            None
        }
    }
}

fn per_doc_dir(root: &Path, collection: Collection, handle: &str) -> std::path::PathBuf {
    let dir_name = collection
        .dir_name()
        .expect("per_doc_dir called with non-directory collection");
    root.join(dir_name).join(handle)
}
```

- [ ] **Step 2: Delete the now-unused helpers**

Remove from `load.rs` the following functions (they are no longer called):
- `load_required_json`
- `load_optional_json`
- `load_optional_json_collection`
- `load_json_file`
- `load_json_collection`

- [ ] **Step 3: Rewrite the existing loader tests**

In `crates/defra-agent-cli/src/desired_state/tests.rs`, find and delete any test that writes `agent-behaviors.json`, `tool-selections.json`, `inference-backends.json`, `tasks.json`, `schedules.json`, `tool-services.json`, `inference-profiles.json`, or flat `tasks/*.json` / `schedules/*.json` / `tool-services/*.json` files. The only existing test that matches this pattern is `load_manifest_root_loads_tasks_and_schedules` (search for the function and delete or rewrite it).

Replace with three new end-to-end tests that exercise the new layout:

```rust
mod load_manifest_root {
    use std::fs;
    use tempfile::tempdir;
    use crate::desired_state::load::load_manifest_root;

    fn write_minimal_root(root: &std::path::Path) {
        fs::write(
            root.join("agent-principal.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agent_did": "did:key:example",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn loads_minimal_root_with_only_principal() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        let (manifest, report) = load_manifest_root(tmp.path());
        assert!(report.ok, "expected ok, got errors: {:?}", report.errors);
        let manifest = manifest.unwrap();
        assert_eq!(manifest.agent_principal.agent_did, "did:key:example");
        assert!(manifest.agent_behaviors.is_empty());
        assert!(manifest.tasks.is_empty());
    }

    #[test]
    fn missing_principal_file_is_error() {
        let tmp = tempdir().unwrap();
        let (_, report) = load_manifest_root(tmp.path());
        assert!(!report.ok);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("agent-principal.json")),
            "got: {:?}",
            report.errors
        );
    }

    #[test]
    fn loads_behavior_with_sidecar_hydration() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        let behavior_dir = tmp.path().join("agent-behaviors").join("default");
        fs::create_dir_all(&behavior_dir).unwrap();
        fs::write(
            behavior_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "behavior_id": "default",
                "agent_did": "did:key:example",
                "system_prompt": "./system_prompt.md",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(behavior_dir.join("system_prompt.md"), "You are helpful.").unwrap();

        let (manifest, report) = load_manifest_root(tmp.path());
        assert!(report.ok, "errors: {:?}", report.errors);
        let behavior = &manifest.unwrap().agent_behaviors[0];
        assert_eq!(behavior.system_prompt.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn missing_sidecar_surfaces_error() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        let behavior_dir = tmp.path().join("agent-behaviors").join("default");
        fs::create_dir_all(&behavior_dir).unwrap();
        fs::write(
            behavior_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "behavior_id": "default",
                "agent_did": "did:key:example",
                "system_prompt": "./system_prompt.md",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, report) = load_manifest_root(tmp.path());
        assert!(!report.ok);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("sidecar path does not resolve")),
            "got: {:?}",
            report.errors
        );
    }
}
```

- [ ] **Step 4: Build and run all desired_state tests**

Run: `cargo test -p defra-agent-cli --lib desired_state`
Expected: all new and existing desired_state tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/load.rs \
        crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "feat: rewrite load_manifest_root for per-doc dirs + sidecars (#67)"
```

---

### Task 5: Integration test helpers — update `write_manifest_root_from_export`

**Files:**
- Modify: `crates/defra-agent-cli/tests/support/fs.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_validate.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_apply_local.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_apply_graphql.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_apply_running.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_apply_e2e.rs`
- Modify: `crates/defra-agent-cli/tests/cli_config_diff.rs`

**Why:** integration tests build manifest roots by writing files on disk. Most go through the central helper `write_manifest_root_from_export`; a handful write files directly. Updating the helper fixes most in one shot.

- [ ] **Step 1: Rewrite `write_manifest_root_from_export` in `tests/support/fs.rs`**

Replace the function body with a version that writes per-doc subdirectories. The top-level `agent-principal.json` stays as-is. Each other collection becomes `<dir_name>/<unique_id_value>/object.json`.

```rust
pub fn write_manifest_root_from_export(root: &Path, exported: &Value) -> Result<()> {
    write_json_file(
        &root.join("agent-principal.json"),
        &project_object_fields(
            exported
                .get("agent_principal")
                .ok_or_else(|| anyhow!("exported bundle missing agent_principal"))?,
            &[
                "agent_did",
                "display_name",
                "default_behavior_id",
                "enabled",
            ],
        )?,
    )?;

    write_per_doc_collection(
        root,
        "agent-behaviors",
        "behavior_id",
        exported
            .get("agent_behaviors")
            .ok_or_else(|| anyhow!("exported bundle missing agent_behaviors"))?,
        &[
            "behavior_id",
            "agent_did",
            "display_name",
            "system_prompt",
            "backend_id",
            "model_name",
            "tool_selection_id",
            "inference_profile_id",
            "compaction_strategy",
            "compaction_threshold",
            "enabled",
        ],
    )?;
    write_per_doc_collection(
        root,
        "tool-selections",
        "selection_id",
        exported
            .get("tool_selections")
            .ok_or_else(|| anyhow!("exported bundle missing tool_selections"))?,
        &[
            "selection_id",
            "agent_did",
            "display_name",
            "enable_file_tools",
            "file_tools_mode",
            "file_tool_root",
            "enable_bash",
            "bash_mode",
            "cli_tool_names",
            "enable_meta_tools",
            "delegate_to",
        ],
    )?;
    write_per_doc_collection(
        root,
        "inference-backends",
        "backend_id",
        exported
            .get("inference_backends")
            .ok_or_else(|| anyhow!("exported bundle missing inference_backends"))?,
        &[
            "backend_id",
            "name",
            "endpoint",
            "api_key_env_var",
            "max_concurrent",
            "max_queue_depth",
            "enabled",
            "models",
        ],
    )?;
    if let Some(profiles) = exported.get("inference_profiles") {
        write_per_doc_collection(
            root,
            "inference-profiles",
            "profile_id",
            profiles,
            &[
                "profile_id",
                "display_name",
                "context_window",
                "max_output_tokens",
                "max_turns",
                "temperature",
                "stream_batch_ms",
                "deadline_duration_secs",
            ],
        )?;
    }
    if let Some(services) = exported.get("tool_service_registries") {
        write_per_doc_collection(
            root,
            "tool-services",
            "service_id",
            services,
            &[
                "service_id",
                "display_name",
                "description",
                "hostname",
                "tailscale_ip",
                "lan_ip",
                "mcp_port",
                "mcp_path",
            ],
        )?;
    }
    if let Some(tasks) = exported.get("tasks") {
        write_per_doc_collection(
            root,
            "tasks",
            "task_id",
            tasks,
            &[
                "task_id",
                "name",
                "description",
                "behavior_id",
                "prompt_template",
                "enabled",
                "output_schema_ref",
            ],
        )?;
    }
    if let Some(schedules) = exported.get("schedules") {
        write_per_doc_collection(
            root,
            "schedules",
            "schedule_id",
            schedules,
            &[
                "schedule_id",
                "task_id",
                "interval_secs",
                "enabled",
                "concurrency",
            ],
        )?;
    }

    Ok(())
}

fn write_per_doc_collection(
    root: &Path,
    dir_name: &str,
    unique_field: &str,
    rows: &Value,
    fields: &[&str],
) -> Result<()> {
    let Some(rows) = rows.as_array() else { return Ok(()); };
    for row in rows {
        let object = project_object_fields(row, fields)?;
        let handle = object
            .get(unique_field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("row missing {unique_field}: {row}"))?;
        let dir = root.join(dir_name).join(handle);
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        write_json_file(&dir.join("object.json"), &object)?;
    }
    Ok(())
}
```

- [ ] **Step 2: Fix tests that write files directly (not via the helper)**

For each of these files, find the `root.join("<name>.json")` / `root.join("<name>").join("*.json")` call sites listed below and rewrite to the new per-doc layout. Example rewrite pattern for a behavior row:

```rust
// OLD
write_json_file(&root.join("agent-behaviors.json"), &json!([ behavior_row ]))?;

// NEW
let behavior_id = behavior_row.get("behavior_id").and_then(Value::as_str).unwrap();
let dir = root.join("agent-behaviors").join(behavior_id);
std::fs::create_dir_all(&dir)?;
write_json_file(&dir.join("object.json"), &behavior_row)?;
```

Direct-write sites to fix (from grep):

- `cli_config_apply_local.rs:38` (`inference-backends.json`)
- `cli_config_validate.rs:33, 51, 68, 160, 177, 179, 252, 270, 287, 302, 234` (a mix of aggregate files and the `tool-services/` flat-file form)
- `cli_config_apply_e2e.rs:55` (`tool-services/ops-mcp.json` — now `tool-services/ops-mcp/object.json`)
- `cli_config_diff.rs:111` (`inference-backends.json`)
- `cli_config_apply_running.rs:43` (`agent-behaviors.json`)
- `cli_config_apply_graphql.rs:45` (`inference-backends.json`)

For the `cli_config_validate.rs` tests that intentionally produce invalid roots, update each to the corresponding new error condition (e.g., tests that used a malformed `agent-behaviors.json` now use a malformed `agent-behaviors/<handle>/object.json`).

- [ ] **Step 3: Run all CLI integration tests that don't require a running DefraDB node**

Run: `cargo test -p defra-agent-cli --test cli_config_validate --test cli_help -- --nocapture`
Expected: all pass.

- [ ] **Step 4: Run the full integration suite**

Run: `cargo test -p defra-agent-cli --test '*'`
Expected: all pass (some tests may skip in offline mode — inspect output; no failures).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/tests/
git commit -m "test: port integration tests to per-doc manifest dirs (#67)"
```

---

### Task 6: Writer — module skeleton + safe-id check

**Files:**
- Create: `crates/defra-agent-cli/src/desired_state/write.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

- [ ] **Step 1: Write failing tests**

Append a `mod write_manifest_root_safe_id` block to `desired_state/tests.rs`:

```rust
mod write_manifest_root_safe_id {
    use crate::desired_state::write::check_filesystem_safe_id;

    #[test]
    fn accepts_ordinary_ids() {
        assert!(check_filesystem_safe_id("default").is_ok());
        assert!(check_filesystem_safe_id("workstation-1").is_ok());
        assert!(check_filesystem_safe_id("seed_fleet_health").is_ok());
    }

    #[test]
    fn rejects_forward_slash() {
        let err = check_filesystem_safe_id("foo/bar").unwrap_err();
        assert!(err.contains("filesystem-unsafe"), "got: {err}");
    }

    #[test]
    fn rejects_backslash_colon_and_null() {
        for bad in ["a\\b", "a:b", "a\0b"] {
            assert!(check_filesystem_safe_id(bad).is_err(), "should reject '{bad}'");
        }
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(check_filesystem_safe_id(".").is_err());
        assert!(check_filesystem_safe_id("..").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(check_filesystem_safe_id("").is_err());
    }
}
```

- [ ] **Step 2: Run to verify fail to compile**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::write_manifest_root_safe_id`
Expected: FAIL (module undefined).

- [ ] **Step 3: Create `write.rs` with the safe-id check**

Write `crates/defra-agent-cli/src/desired_state/write.rs`:

```rust
use std::fs;
use std::path::Path;

use serde_json::Value;

use defra_agent::Collection;

use super::{DesiredStateManifest, HasUniqueId};

/// Verify that `id` is a valid per-document directory handle. Rejects any
/// character that would break filesystem semantics or produce ambiguous
/// paths (`/`, `\`, `:`, null byte), the traversal specials `.` and
/// `..`, and the empty string.
pub(crate) fn check_filesystem_safe_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(
            "unique id is empty; choose a filesystem-safe id".to_string(),
        );
    }
    if id == "." || id == ".." {
        return Err(format!(
            "unique id '{id}' contains filesystem-unsafe value; choose a filesystem-safe id"
        ));
    }
    for ch in id.chars() {
        if matches!(ch, '/' | '\\' | ':' | '\0') {
            return Err(format!(
                "unique id '{id}' contains filesystem-unsafe character(s); choose a filesystem-safe id"
            ));
        }
    }
    Ok(())
}

/// Write a `DesiredStateManifest` to `root` as a manifest root directory.
/// See `docs/superpowers/specs/2026-04-22-per-agent-manifest-roots-design.md`
/// for the on-disk layout contract.
pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<(), String> {
    let _ = (root, manifest, force);
    unimplemented!("implemented in Task 7")
}
```

- [ ] **Step 4: Register the module in `desired_state/mod.rs`**

Add to the top of `mod.rs`:

```rust
pub(crate) mod write;
```

And re-export:

```rust
pub(crate) use write::write_manifest_root;
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::write_manifest_root_safe_id`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/write.rs \
        crates/defra-agent-cli/src/desired_state/mod.rs \
        crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "feat: add write module scaffold with filesystem-safe-id check (#67)"
```

---

### Task 7: Writer — full implementation (principal + per-doc + sidecars)

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/write.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

- [ ] **Step 1: Write failing tests**

These writer tests construct manifests with the three collection types that have sidecar semantics (`AgentPrincipal`, `AgentBehavior`, `Task`) and leave the other five empty. That keeps the fixture small and sidesteps the custom-deserialize and enum-field complexity of `DesiredInferenceBackend` and `DesiredToolServiceRegistry`. The full cross-collection exercise is covered by the round-trip test in Task 9.

Append to `desired_state/tests.rs`:

```rust
pub(super) mod write_manifest_root {
    use std::fs;
    use tempfile::tempdir;

    use crate::desired_state::{
        write_manifest_root, DesiredAgentBehavior, DesiredAgentPrincipal,
        DesiredStateManifest, DesiredTask,
    };

    pub(in crate::desired_state::tests) fn minimal_manifest() -> DesiredStateManifest {
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:example".to_string(),
                display_name: None,
                default_behavior_id: Some("default".to_string()),
                enabled: true,
            },
            agent_behaviors: vec![DesiredAgentBehavior {
                behavior_id: "default".to_string(),
                agent_did: "did:key:example".to_string(),
                display_name: None,
                system_prompt: Some("You are helpful.".to_string()),
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: true,
            }],
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            tasks: vec![DesiredTask {
                task_id: "seed-health".to_string(),
                name: "Seed fleet health".to_string(),
                description: None,
                behavior_id: "default".to_string(),
                prompt_template: "Check the fleet.".to_string(),
                enabled: true,
                output_schema_ref: None,
            }],
            schedules: Vec::new(),
        }
    }

    #[test]
    fn writes_principal_and_per_doc_dirs_with_sidecars() {
        let tmp = tempdir().unwrap();
        write_manifest_root(tmp.path(), &minimal_manifest(), false).unwrap();

        assert!(tmp.path().join("agent-principal.json").is_file());

        let behavior_object = tmp.path().join("agent-behaviors/default/object.json");
        assert!(behavior_object.is_file());
        let behavior_sidecar = tmp.path().join("agent-behaviors/default/system_prompt.md");
        assert!(behavior_sidecar.is_file());
        assert_eq!(fs::read_to_string(&behavior_sidecar).unwrap(), "You are helpful.");
        let behavior_body: serde_json::Value =
            serde_json::from_slice(&fs::read(&behavior_object).unwrap()).unwrap();
        assert_eq!(
            behavior_body.get("system_prompt").and_then(|v| v.as_str()),
            Some("./system_prompt.md")
        );

        let task_object = tmp.path().join("tasks/seed-health/object.json");
        assert!(task_object.is_file());
        let task_sidecar = tmp.path().join("tasks/seed-health/prompt.md");
        assert!(task_sidecar.is_file());
        assert_eq!(fs::read_to_string(&task_sidecar).unwrap(), "Check the fleet.");
        let task_body: serde_json::Value =
            serde_json::from_slice(&fs::read(&task_object).unwrap()).unwrap();
        assert_eq!(
            task_body.get("prompt_template").and_then(|v| v.as_str()),
            Some("./prompt.md")
        );
    }

    #[test]
    fn none_system_prompt_omits_sidecar_and_field() {
        let tmp = tempdir().unwrap();
        let mut m = minimal_manifest();
        m.agent_behaviors[0].system_prompt = None;
        write_manifest_root(tmp.path(), &m, false).unwrap();

        let sidecar = tmp.path().join("agent-behaviors/default/system_prompt.md");
        assert!(!sidecar.exists());
        let body: serde_json::Value = serde_json::from_slice(
            &fs::read(tmp.path().join("agent-behaviors/default/object.json")).unwrap(),
        )
        .unwrap();
        assert!(body.get("system_prompt").is_none());
    }

    #[test]
    fn rejects_behavior_with_unsafe_id() {
        let tmp = tempdir().unwrap();
        let mut m = minimal_manifest();
        m.agent_behaviors[0].behavior_id = "bad/id".to_string();
        let err = write_manifest_root(tmp.path(), &m, false).unwrap_err();
        assert!(err.contains("filesystem-unsafe"), "got: {err}");
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::write_manifest_root`
Expected: FAIL / panic (`unimplemented!`).

- [ ] **Step 3: Implement the writer body**

Replace the `unimplemented!` body in `write.rs`:

```rust
pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<(), String> {
    prepare_root(root, force)?;

    // agent-principal.json at the top level.
    let principal_value = serde_json::to_value(&manifest.agent_principal)
        .map_err(|e| format!("serializing agent_principal failed: {e}"))?;
    write_json_file(
        &root.join(
            Collection::AgentPrincipal
                .file_name()
                .expect("AgentPrincipal has a top-level file"),
        ),
        &principal_value,
    )?;

    // Per-doc collections, mirror of the loader.
    write_per_doc_collection(
        root,
        Collection::AgentBehavior,
        &manifest.agent_behaviors,
        spill_behavior_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::ToolSelection,
        &manifest.tool_selections,
        no_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::InferenceBackend,
        &manifest.inference_backends,
        no_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::InferenceProfile,
        &manifest.inference_profiles,
        no_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::ToolServiceRegistry,
        &manifest.tool_service_registries,
        no_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::Task,
        &manifest.tasks,
        spill_task_sidecar,
    )?;
    write_per_doc_collection(
        root,
        Collection::Schedule,
        &manifest.schedules,
        no_sidecar,
    )?;

    Ok(())
}

fn prepare_root(root: &Path, force: bool) -> Result<(), String> {
    if !root.exists() {
        fs::create_dir_all(root)
            .map_err(|e| format!("creating {} failed: {e}", root.display()))?;
        return Ok(());
    }
    let is_empty = fs::read_dir(root)
        .map_err(|e| format!("reading {} failed: {e}", root.display()))?
        .next()
        .is_none();
    if is_empty {
        return Ok(());
    }
    if !force {
        return Err(format!(
            "manifest root is non-empty; pass --force to overwrite: {}",
            root.display()
        ));
    }
    fs::remove_dir_all(root)
        .map_err(|e| format!("clearing {} failed: {e}", root.display()))?;
    fs::create_dir_all(root)
        .map_err(|e| format!("creating {} failed: {e}", root.display()))?;
    Ok(())
}

fn write_per_doc_collection<T>(
    root: &Path,
    collection: Collection,
    docs: &[T],
    mut spill: impl FnMut(&Path, &mut Value) -> Result<(), String>,
) -> Result<(), String>
where
    T: serde::Serialize + HasUniqueId,
{
    if docs.is_empty() {
        return Ok(());
    }
    let dir_name = collection
        .dir_name()
        .expect("write_per_doc_collection called with non-dir collection");
    let collection_dir = root.join(dir_name);
    fs::create_dir_all(&collection_dir)
        .map_err(|e| format!("creating {} failed: {e}", collection_dir.display()))?;

    for doc in docs {
        let handle = doc.unique_id();
        check_filesystem_safe_id(handle)?;
        let doc_dir = collection_dir.join(handle);
        fs::create_dir_all(&doc_dir)
            .map_err(|e| format!("creating {} failed: {e}", doc_dir.display()))?;

        let mut body = serde_json::to_value(doc)
            .map_err(|e| format!("serializing {} '{handle}' failed: {e}", collection))?;
        spill(&doc_dir, &mut body)?;
        write_json_file(&doc_dir.join("object.json"), &body)?;
    }
    Ok(())
}

fn no_sidecar(_dir: &Path, _value: &mut Value) -> Result<(), String> {
    Ok(())
}

fn spill_behavior_sidecar(doc_dir: &Path, body: &mut Value) -> Result<(), String> {
    spill_string_field(doc_dir, body, "system_prompt", "system_prompt.md")
}

fn spill_task_sidecar(doc_dir: &Path, body: &mut Value) -> Result<(), String> {
    spill_string_field(doc_dir, body, "prompt_template", "prompt.md")
}

fn spill_string_field(
    doc_dir: &Path,
    body: &mut Value,
    field: &str,
    sidecar_name: &str,
) -> Result<(), String> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| format!("expected object body for sidecar spill, got {body}"))?;
    let Some(current) = object.get(field).and_then(Value::as_str).map(str::to_owned) else {
        return Ok(());
    };
    if current.is_empty() {
        return Ok(());
    }
    fs::write(doc_dir.join(sidecar_name), &current).map_err(|e| {
        format!(
            "writing {} failed: {e}",
            doc_dir.join(sidecar_name).display()
        )
    })?;
    object.insert(field.to_string(), Value::String(format!("./{sidecar_name}")));
    Ok(())
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("serializing {} failed: {e}", path.display()))?;
    bytes.push(b'\n');
    fs::write(path, &bytes).map_err(|e| format!("writing {} failed: {e}", path.display()))
}
```

- [ ] **Step 4: Run writer tests to verify pass**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::write_manifest_root`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/write.rs \
        crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "feat: implement manifest root writer with sidecar spilling (#67)"
```

---

### Task 8: Writer — `--force` overwrite test

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

The writer's `prepare_root` already handles `force`, from Task 7. This task is the specific test coverage for the stray-files-gone behavior.

- [ ] **Step 1: Write failing test**

Append to the `mod write_manifest_root` block from Task 7:

```rust
#[test]
fn refuses_to_overwrite_without_force() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("leftover.txt"), "junk").unwrap();
    let err = write_manifest_root(tmp.path(), &minimal_manifest(), false).unwrap_err();
    assert!(err.contains("--force"), "got: {err}");
}

#[test]
fn force_removes_stray_files_from_previous_export() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("leftover.txt"), "junk").unwrap();
    std::fs::create_dir_all(tmp.path().join("agent-behaviors").join("old-name")).unwrap();
    std::fs::write(
        tmp.path().join("agent-behaviors/old-name/object.json"),
        b"{}",
    )
    .unwrap();

    write_manifest_root(tmp.path(), &minimal_manifest(), true).unwrap();

    // Leftover is gone.
    assert!(!tmp.path().join("leftover.txt").exists());
    // Old behavior dir is gone.
    assert!(!tmp.path().join("agent-behaviors/old-name").exists());
    // New content is present.
    assert!(tmp.path().join("agent-behaviors/default/object.json").is_file());
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::write_manifest_root`
Expected: all pass (these two are the new additions; earlier three still pass).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "test: cover --force overwrite behavior for manifest writer (#67)"
```

---

### Task 9: Round-trip test

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/tests.rs`

- [ ] **Step 1: Write failing round-trip test**

Append as a new sibling test (top level of `mod tests`, not inside `mod write_manifest_root`):

```rust
#[test]
fn round_trip_load_write_load_is_identity() {
    use crate::desired_state::{load::load_manifest_root, write_manifest_root};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let original = self::write_manifest_root::minimal_manifest();

    write_manifest_root(tmp.path(), &original, false).unwrap();
    let (loaded, report) = load_manifest_root(tmp.path());
    assert!(report.ok, "errors: {:?}", report.errors);
    let loaded = loaded.unwrap();

    assert_eq!(loaded.agent_principal, original.agent_principal);
    assert_eq!(loaded.agent_behaviors, original.agent_behaviors);
    assert_eq!(loaded.tool_selections, original.tool_selections);
    assert_eq!(loaded.inference_backends, original.inference_backends);
    assert_eq!(loaded.inference_profiles, original.inference_profiles);
    assert_eq!(loaded.tool_service_registries, original.tool_service_registries);
    assert_eq!(loaded.tasks, original.tasks);
    assert_eq!(loaded.schedules, original.schedules);
}
```

`minimal_manifest` is already `pub(in crate::desired_state::tests) fn` from Task 7, so this call sees it. The round-trip uses the minimal fixture from Task 7 (principal + one behavior with sidecar + one task with sidecar); the cross-collection coverage comes from the integration suite in Tasks 5 and 11.

- [ ] **Step 2: Run the test**

Run: `cargo test -p defra-agent-cli --lib desired_state::tests::round_trip`
Expected: PASS. If any `assert_eq!` fails, the loader and writer disagree on that field — fix the mismatch (e.g., a field that's `None` on one side and `Some("")` on the other after JSON round-trip). The round-trip test is the canonical acceptance criterion.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/tests.rs
git commit -m "test: add load/write/load round-trip invariant for manifest root (#67)"
```

---

### Task 10: CLI wiring — `ConfigExportArgs` flags + rewrite `config_export`

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs`
- Modify: `crates/defra-agent-cli/src/commands/config/export.rs`

- [ ] **Step 1: Update `ConfigExportArgs` in `args.rs`**

Replace the struct at line 595 with:

```rust
#[derive(clap::Args)]
pub(crate) struct ConfigExportArgs {
    #[arg(long, value_name = "ROOT", help = "Directory to write the manifest root into")]
    pub(crate) root: PathBuf,
    #[arg(long, default_value_t = false, help = "Overwrite the root dir if it is non-empty")]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
}
```

- [ ] **Step 2: Rewrite `config_export` in `commands/config/export.rs`**

Replace the function body:

```rust
use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::{build_config_export_bundle, resolve_agent_did, resolve_config_access};

pub(super) async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    let manifest = desired_state::manifest_from_export_bundle(&bundle)?;
    desired_state::write_manifest_root(&args.root, &manifest, args.force)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("wrote manifest root to {}", args.root.display());
    Ok(())
}
```

If `print_json` was the only remaining caller in this file, drop the `use crate::print_json;` line. Do not drop it from elsewhere.

- [ ] **Step 3: Build the binary**

Run: `cargo build -p defra-agent-cli`
Expected: succeeds. If it fails because `manifest_from_export_bundle` is not re-exported from `desired_state`, add the re-export to `desired_state/mod.rs`.

- [ ] **Step 4: Smoke-test the CLI against `--help`**

Run: `cargo run -p defra-agent-cli -- config export --help`
Expected: output shows `--root <ROOT>` (required) and `--force`.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/cli/args.rs \
        crates/defra-agent-cli/src/commands/config/export.rs
git commit -m "feat: config export writes manifest root with --root/--force (#67)"
```

---

### Task 11: Integration tests — rewrite `cli_config_export_import.rs` and add `cli_config_native_root.rs`

**Files:**
- Modify: `crates/defra-agent-cli/tests/cli_config_export_import.rs`
- Create: `crates/defra-agent-cli/tests/cli_config_native_root.rs`

- [ ] **Step 1: Rewrite `cli_config_export_import.rs` tests that exercise `config export`**

Read the file (`wc -l` was 350). Find every invocation of `defra-agent config export` (no args → JSON stdout) and replace with `defra-agent config export --root <tmpdir>`. The follow-up assertions that parse JSON stdout should instead read `<tmpdir>/agent-principal.json` and the per-doc `object.json` files using the existing `read_json_file` helper.

Tests that pipe `export` output into `import` must be updated to:
- Run `config export --root <tmpdir>`.
- Then run `config apply --root <tmpdir>` (not `config import`, which stays on the JSON bundle format). If the test specifically exercises `config import`, leave it alone — it uses its own JSON bundle fixture independent of `config export`.

If there is round-trip coverage tied specifically to the JSON bundle format, keep it in a `mod legacy_bundle_roundtrip` section that builds its fixtures inline (so `config export` output is not needed).

- [ ] **Step 2: Create `cli_config_native_root.rs`**

Write a new integration test that exercises each new error kind through the CLI:

```rust
use std::fs;
use std::process::Command;

use anyhow::Result;
use serde_json::Value;
use tempfile::tempdir;

mod support;
use support::fs::{read_json_file, write_json_file};

fn defra_agent() -> Command {
    Command::new(env!("CARGO_BIN_EXE_defra-agent"))
}

fn run_validate(root: &std::path::Path) -> Result<Value> {
    let output = defra_agent()
        .args(["config", "validate", "--root"])
        .arg(root)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    Ok(serde_json::from_str(&stdout)?)
}

fn write_principal(root: &std::path::Path) -> Result<()> {
    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({ "agent_did": "did:key:example", "enabled": true }),
    )
}

#[test]
fn validate_accepts_minimal_per_doc_root() -> Result<()> {
    let tmp = tempdir()?;
    write_principal(tmp.path())?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn validate_rejects_handle_mismatch() -> Result<()> {
    let tmp = tempdir()?;
    write_principal(tmp.path())?;
    let dir = tmp.path().join("agent-behaviors").join("on-disk");
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": "inside-json",
            "agent_did": "did:key:example",
            "enabled": true,
        }),
    )?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false));
    let joined = report
        .get("errors")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("does not match behavior_id"), "got: {joined}");
    Ok(())
}

#[test]
fn validate_rejects_missing_sidecar() -> Result<()> {
    let tmp = tempdir()?;
    write_principal(tmp.path())?;
    let dir = tmp.path().join("agent-behaviors").join("default");
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": "default",
            "agent_did": "did:key:example",
            "system_prompt": "./system_prompt.md",
            "enabled": true,
        }),
    )?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false));
    let joined = report
        .get("errors")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("sidecar path does not resolve"),
        "got: {joined}"
    );
    Ok(())
}

#[test]
fn validate_accepts_stray_readme_in_doc_dir() -> Result<()> {
    let tmp = tempdir()?;
    write_principal(tmp.path())?;
    let dir = tmp.path().join("agent-behaviors").join("default");
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": "default",
            "agent_did": "did:key:example",
            "enabled": true,
        }),
    )?;
    fs::write(dir.join("README.md"), "notes")?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    Ok(())
}
```

- [ ] **Step 3: Run both integration files**

Run: `cargo test -p defra-agent-cli --test cli_config_native_root --test cli_config_export_import`
Expected: all pass.

- [ ] **Step 4: Run the full integration suite to catch anything else**

Run: `cargo test -p defra-agent-cli --test '*'`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_config_native_root.rs \
        crates/defra-agent-cli/tests/cli_config_export_import.rs
git commit -m "test: add native-root integration tests + port export/import (#67)"
```

---

### Final verification

- [ ] **Run the full test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Run clippy to catch style nits**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Verify the spec's acceptance criteria manually**

  - `config validate --root <dir>` accepts the directory shape — **Task 4 + Task 11**
  - `config diff --root <dir>` and `apply --root <dir>` hydrate prompts transparently — **Task 4** (loader rewrite is shared with diff/apply)
  - `config export --root <dir>` produces a round-trippable manifest root — **Task 10**
  - Exporting and reapplying a root is lossless — **Task 9** (round-trip test)

- [ ] **Confirm the worktree branch is ready to open as a PR**

Run: `git -C <worktree-path> log main..HEAD --oneline`
Expected: one commit per task, plus the design-doc commit made in brainstorming.
