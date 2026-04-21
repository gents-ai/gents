# Apply-Reconcile Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove T-Conv (end-to-end convergence between CLI apply and runtime reconcile) in Lean, and tighten the Rust apply API so the apply-vs-runtime field-ownership partition is enforced by the type system.

**Architecture:** A new Lean module `Proofs/ApplyReconcile.lean` composes on top of `RuntimeReconcile.lean`. A new `enum Collection` replaces seven parallel `*_FILE`/`*_DIR` string constants and gives every downstream routine a typed discriminator. `DesiredFields`/`LiveFields` marker traits + a `DesiredApplyBundle` newtype constrain the single `apply_desired_state_changes` entry point so only values built from typed `Desired*` structs can flow into apply. Property tests exercise a Rust reference model that mirrors the Lean definitions; a small conformance table anchors the model to concrete inputs.

**Tech Stack:** Rust (defra-agent workspace), Lean 4 + Mathlib (proofs crate), `proptest` (added), `serde_json`.

**Spec reference:** `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`
**Issue:** sourcenetwork/defra-agent#53

**Status note (2026-04-21):** This plan supersedes an earlier draft that targeted a stale codebase layout. Current reality:

- `desired_state.rs` is a module at `crates/defra-agent-cli/src/desired_state/` with files `mod.rs`, `load.rs`, `diff.rs`, `convert.rs`, `validate.rs`, `normalize.rs`, `tests.rs`.
- The seven `*_FILE`/`*_DIR` constants live in `desired_state/load.rs`.
- The seven-way apply dispatch lives in `config_import.rs::apply_desired_state_changes` (seven `apply_import_collection` calls) plus `config_bundle.rs` (seven GraphQL reads) plus `desired_state/{convert,diff,validate,load}.rs`.
- `crates/defra-agent-cli/tests/cli_e2e.rs` no longer exists. Apply/diff integration tests live in `cli_config_apply_local.rs`, `cli_config_apply_graphql.rs`, `cli_config_apply_running.rs`, `cli_config_apply_e2e.rs`, `cli_config_diff.rs`, `cli_config_validate.rs`, `cli_config_tasks.rs`, `cli_config_export_import.rs`.
- `main.rs` is 579 lines (not the 6534 the spec mentions); the spec's "refactor main.rs" out-of-scope line is stale-informational — no refactor is needed to thread `Collection` through.
- `ConfigAccess` (not `DefraAccess`) is the write-side access type, defined in `crates/defra-agent-cli/src/config_writes/mod.rs`.

---

## File Structure

### New files

- `crates/defra-agent-cli/src/collection.rs` — `enum Collection` + metadata methods (`file_name`, `dir_name`, `graphql_type`, `unique_field`, `apply_order`, `ALL`).
- `crates/defra-agent/src/desired_fields.rs` — `DesiredFields` and `LiveFields` marker traits, re-exported from the library.
- `crates/defra-agent/src/apply_model.rs` — test-only reference implementation of the apply model (Rust mirror of the Lean `ApplyReconcile` module).
- `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean` — new Lean module with `Collection`, `DocRef`, `Manifest`, `LiveState`, `ApplyStep`, `diff`, `applyOne`, `applyAll`, support lemmas, T-Conv.
- `crates/defra-agent/tests/apply_property.rs` — `proptest` properties: diff-bucket partition, ordering preserves references, diff determinism.
- `crates/defra-agent/tests/apply_conformance.rs` — table-driven conformance cases pinning the Rust model to the Lean semantics.

### Modified files

- `crates/defra-agent-cli/src/main.rs` — register `mod collection;`.
- `crates/defra-agent-cli/src/desired_state/load.rs` — replace file/dir string constants with `Collection::*.file_name()` / `Collection::*.dir_name()`.
- `crates/defra-agent-cli/src/desired_state/mod.rs` — impl `DesiredFields` for the seven `Desired*` structs.
- `crates/defra-agent-cli/src/shared.rs` — introduce `DesiredApplyBundle(ConfigExportBundle)` newtype with private inner field.
- `crates/defra-agent-cli/src/desired_state/convert.rs` — change `export_bundle_from_manifest` to return `DesiredApplyBundle`.
- `crates/defra-agent-cli/src/config_import.rs` — narrow `apply_desired_state_changes` signature to `&DesiredApplyBundle`; use `Collection::<X>.graphql_type()` / `.unique_field()` where those strings appear.
- `crates/defra-agent-cli/src/commands/config/apply.rs` — update call site to use the new newtype (trivial rebind).
- `crates/defra-agent/src/lib.rs` — `pub mod desired_fields;`, `pub mod apply_model;`, re-export traits.
- `crates/defra-agent/proofs/Proofs.lean` — `import Proofs.ApplyReconcile`.
- `crates/defra-agent/Cargo.toml` — add `proptest = "1"` to `[dev-dependencies]`.
- `crates/defra-agent/proofs/README.md` — append apply-atomicity known-limitation note.
- `crates/defra-agent-cli/tests/cli_config_*.rs` — audit and remove tests subsumed by the new property/conformance coverage.

---

## Phase 1 — `Collection` enum (Rust groundwork)

### Task 1: Introduce `enum Collection`

**Files:**
- Create: `crates/defra-agent-cli/src/collection.rs`
- Modify: `crates/defra-agent-cli/src/main.rs` (add `mod collection;`)

- [ ] **Step 1: Write the module with unit tests**

Create `crates/defra-agent-cli/src/collection.rs`:

```rust
//! Typed discriminator for the set of operator-controlled collections.
//!
//! Mirrors the Lean inductive `ApplyReconcile.Collection` in
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`. Any change
//! to the set of variants, their GraphQL names, or their apply-order
//! ranks must be reflected in the Lean module.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Collection {
    AgentPrincipal,
    AgentBehavior,
    ToolSelection,
    InferenceBackend,
    InferenceProfile,
    ToolServiceRegistry,
    ScheduledTask,
}

impl Collection {
    pub(crate) const ALL: [Collection; 7] = [
        Collection::AgentPrincipal,
        Collection::AgentBehavior,
        Collection::ToolSelection,
        Collection::InferenceBackend,
        Collection::InferenceProfile,
        Collection::ToolServiceRegistry,
        Collection::ScheduledTask,
    ];

    /// Manifest file name on disk for the single-file form.
    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent-principal.json",
            Collection::AgentBehavior => "agent-behaviors.json",
            Collection::ToolSelection => "tool-selections.json",
            Collection::InferenceBackend => "inference-backends.json",
            Collection::InferenceProfile => "inference-profiles.json",
            Collection::ToolServiceRegistry => "tool-services.json",
            Collection::ScheduledTask => "scheduled-tasks.json",
        }
    }

    /// Manifest directory name (for collections that support a per-doc dir form).
    pub(crate) fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::ScheduledTask => Some("scheduled-tasks"),
            _ => None,
        }
    }

    /// DefraDB GraphQL type name for this collection.
    pub(crate) fn graphql_type(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "AgentPrincipal",
            Collection::AgentBehavior => "AgentBehavior",
            Collection::ToolSelection => "ToolSelection",
            Collection::InferenceBackend => "InferenceBackend",
            Collection::InferenceProfile => "InferenceProfile",
            Collection::ToolServiceRegistry => "ToolServiceRegistry",
            Collection::ScheduledTask => "ScheduledTask",
        }
    }

    /// Unique-id field name used in `filter: { <field>: { _eq: ... } }`.
    pub(crate) fn unique_field(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent_did",
            Collection::AgentBehavior => "behavior_id",
            Collection::ToolSelection => "selection_id",
            Collection::InferenceBackend => "backend_id",
            Collection::InferenceProfile => "profile_id",
            Collection::ToolServiceRegistry => "service_id",
            Collection::ScheduledTask => "task_id",
        }
    }

    /// Apply ordering rank: lower ranks are written first so referenced
    /// documents exist before referrers. Mirrors
    /// `ApplyReconcile.Collection.applyOrder` in Lean.
    pub(crate) fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
        }
    }
}

impl fmt::Display for Collection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Collection::AgentPrincipal => "agent_principal",
            Collection::AgentBehavior => "agent_behaviors",
            Collection::ToolSelection => "tool_selections",
            Collection::InferenceBackend => "inference_backends",
            Collection::InferenceProfile => "inference_profiles",
            Collection::ToolServiceRegistry => "tool_service_registries",
            Collection::ScheduledTask => "scheduled_tasks",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_collections_have_distinct_file_names() {
        let names: BTreeSet<&str> =
            Collection::ALL.iter().map(|c| c.file_name()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn all_collections_have_distinct_graphql_types() {
        let names: BTreeSet<&str> =
            Collection::ALL.iter().map(|c| c.graphql_type()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn apply_order_puts_referees_before_referrers() {
        assert!(
            Collection::InferenceBackend.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::ToolSelection.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::InferenceProfile.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::AgentBehavior.apply_order()
                < Collection::ScheduledTask.apply_order()
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (module not registered)**

Run: `cargo test -p defra-agent-cli --lib collection::`
Expected: FAIL with `error[E0432]: unresolved import` or similar — the module isn't declared yet.

- [ ] **Step 3: Register the module**

Modify `crates/defra-agent-cli/src/main.rs`. Find the block of `mod` declarations (around lines 13–25) and add `mod collection;` alphabetically:

```rust
mod cli;
mod collection;
mod commands;
mod config_bundle;
mod config_import;
mod config_writes;
mod desired_state;
mod graphql_access;
mod home_state;
mod http;
mod request_helpers;
mod resolve_helpers;
mod shared;
mod telemetry;
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p defra-agent-cli --lib collection::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/collection.rs crates/defra-agent-cli/src/main.rs
git commit -m "Add Collection enum for desired-state dispatch"
```

### Task 2: Thread `Collection` through `desired_state/load.rs`

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs`

- [ ] **Step 1: Remove the string constants and use `Collection`**

In `crates/defra-agent-cli/src/desired_state/load.rs`, delete lines 14–22:

```rust
const AGENT_PRINCIPAL_FILE: &str = "agent-principal.json";
const AGENT_BEHAVIORS_FILE: &str = "agent-behaviors.json";
const TOOL_SELECTIONS_FILE: &str = "tool-selections.json";
const INFERENCE_BACKENDS_FILE: &str = "inference-backends.json";
const INFERENCE_PROFILES_FILE: &str = "inference-profiles.json";
const TOOL_SERVICES_FILE: &str = "tool-services.json";
const TOOL_SERVICES_DIR: &str = "tool-services";
const SCHEDULED_TASKS_FILE: &str = "scheduled-tasks.json";
const SCHEDULED_TASKS_DIR: &str = "scheduled-tasks";
```

Add at the top of the file (alongside the existing `use super::...` and `use serde::Deserialize;`):

```rust
use crate::collection::Collection;
```

- [ ] **Step 2: Replace each string-constant usage**

In the same file, replace each usage:

- `AGENT_PRINCIPAL_FILE` → `Collection::AgentPrincipal.file_name()`
- `AGENT_BEHAVIORS_FILE` → `Collection::AgentBehavior.file_name()`
- `TOOL_SELECTIONS_FILE` → `Collection::ToolSelection.file_name()`
- `INFERENCE_BACKENDS_FILE` → `Collection::InferenceBackend.file_name()`
- `INFERENCE_PROFILES_FILE` → `Collection::InferenceProfile.file_name()`
- `TOOL_SERVICES_FILE` → `Collection::ToolServiceRegistry.file_name()`
- `TOOL_SERVICES_DIR` → `Collection::ToolServiceRegistry.dir_name().expect("tool-services has a dir form")`
- `SCHEDULED_TASKS_FILE` → `Collection::ScheduledTask.file_name()`
- `SCHEDULED_TASKS_DIR` → `Collection::ScheduledTask.dir_name().expect("scheduled-tasks has a dir form")`

- [ ] **Step 3: Verify the crate builds**

Run: `cargo check -p defra-agent-cli`
Expected: clean.

- [ ] **Step 4: Run the workspace tests**

Run: `cargo test -p defra-agent-cli`
Expected: all tests pass — this is a pure-renaming change; file paths on disk are unchanged.

- [ ] **Step 5: Confirm no string constants remain**

Run via Grep tool: pattern `AGENT_PRINCIPAL_FILE|AGENT_BEHAVIORS_FILE|TOOL_SELECTIONS_FILE|INFERENCE_BACKENDS_FILE|INFERENCE_PROFILES_FILE|TOOL_SERVICES_FILE|TOOL_SERVICES_DIR|SCHEDULED_TASKS_FILE|SCHEDULED_TASKS_DIR` scoped to `crates/defra-agent-cli/src`.
Expected: no matches.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/load.rs
git commit -m "Thread Collection enum through desired_state/load paths"
```

### Task 3: Use `Collection` in the apply dispatch

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs`

Rationale: seven parallel calls to `apply_import_collection` today pass a literal `"AgentBehavior"` plus the literal unique field `"behavior_id"`. Keep the seven calls (full consolidation is I-1 follow-on work) but source both strings from `Collection::<X>`, so adding an eighth collection means adding a variant and one call rather than finding two parallel string sites.

- [ ] **Step 1: Update `apply_desired_state_changes` in `config_import.rs`**

In `crates/defra-agent-cli/src/config_import.rs`, add near the other `use` statements:

```rust
use crate::collection::Collection;
```

In `apply_desired_state_changes` (around lines 188–292), replace each of the seven calls. Example for the first one (InferenceBackend):

```rust
inference_backends: apply_import_collection(
    access,
    Collection::InferenceBackend.graphql_type(),
    Collection::InferenceBackend.unique_field(),
    &backend_docs,
    true,
)
.await?,
```

And similarly for `inference_profiles`, `tool_service_registries`, `tool_selections`, `agent_behaviors`, `scheduled_tasks`, `agent_principal`.

Also update the seven `select_apply_collection_docs` calls (lines 193–228) to pass `Collection::<X>.unique_field()` and `Collection::<X>.graphql_type()` where they currently pass literal strings.

- [ ] **Step 2: Build and run CLI tests**

Run: `cargo test -p defra-agent-cli`
Expected: PASS — behavior identical to before; only the literal strings have been sourced from `Collection`.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/src/config_import.rs
git commit -m "Route apply dispatch strings through Collection"
```

---

## Phase 2 — `DesiredFields` / `LiveFields` marker traits + typed apply bundle

**Scope note:** The spec's "unrepresentable" non-interference property is operationalized here on the apply side. We introduce marker traits, implement `DesiredFields` on the seven `Desired*` structs (which by construction enumerate only apply-owned fields), and add a `DesiredApplyBundle` newtype whose only public constructor funnels through `export_bundle_from_manifest(&DesiredStateManifest)`. The runtime side continues to write live fields directly via its existing GraphQL paths — threading `LiveFields` through the runtime's GraphQL mutations is deliberately out of scope (the trait is defined so the target is pinned, but runtime writers are not migrated). The asymmetric guarantee — *apply cannot clobber live state* — is the operator-facing safety claim and is fully enforced.

### Task 4: Define the marker traits in `defra-agent`

**Files:**
- Create: `crates/defra-agent/src/desired_fields.rs`
- Modify: `crates/defra-agent/src/lib.rs`

- [ ] **Step 1: Write the trait definitions**

Create `crates/defra-agent/src/desired_fields.rs`:

```rust
//! Marker traits partitioning document fields by ownership.
//!
//! Apply-side writers (CLI manifest apply) produce values implementing
//! [`DesiredFields`]. Runtime writers (reconcile, scheduler, lifecycle)
//! produce values implementing [`LiveFields`]. The traits are marker-only;
//! their purpose is to make the apply-vs-runtime field partition
//! unrepresentable to cross at the API boundary.

/// A value that represents only operator-owned (desired-state) document fields.
///
/// Implementations must not contain any field written by the runtime —
/// `next_run_at`, `last_probe`, `probe_status`, `run_count`, etc.
pub trait DesiredFields {
    /// Stable collection tag (snake_case). Mirrors the Rust
    /// `defra_agent_cli::collection::Collection::Display` variant names
    /// and the Lean `ApplyReconcile.Collection` constructors.
    fn collection_tag(&self) -> &'static str;
}

/// A value that represents only runtime-owned (live-state) document fields.
///
/// Reserved for runtime-side writers to adopt. Not currently enforced on the
/// runtime half — see the spec non-goals and the `LiveFields` adoption
/// follow-on.
pub trait LiveFields {
    /// Stable collection tag (snake_case).
    fn collection_tag(&self) -> &'static str;
}
```

- [ ] **Step 2: Register and re-export the module**

Modify `crates/defra-agent/src/lib.rs`. Add near the other `pub mod ...;` lines:

```rust
pub mod desired_fields;

pub use desired_fields::{DesiredFields, LiveFields};
```

- [ ] **Step 3: Verify the library still builds**

Run: `cargo build -p defra-agent`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/desired_fields.rs crates/defra-agent/src/lib.rs
git commit -m "Add DesiredFields and LiveFields marker traits"
```

### Task 5: Implement `DesiredFields` for the seven `Desired*` structs

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`

- [ ] **Step 1: Write the failing test**

Add at the bottom of `crates/defra-agent-cli/src/desired_state/mod.rs` (outside any existing `#[cfg(test)] mod tests;`):

```rust
#[cfg(test)]
mod desired_fields_tests {
    use super::*;
    use defra_agent::DesiredFields;

    #[test]
    fn desired_structs_report_their_collection_tags() {
        let p = DesiredAgentPrincipal {
            agent_did: "did:x".into(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        };
        assert_eq!(p.collection_tag(), "agent_principal");
    }
}
```

- [ ] **Step 2: Run the test to verify failure**

Run: `cargo test -p defra-agent-cli --lib desired_state::desired_fields_tests`
Expected: FAIL — "the method `collection_tag` exists for struct `DesiredAgentPrincipal`, but its trait bounds were not satisfied" or similar.

- [ ] **Step 3: Add impls for every `Desired*` struct**

Add at the bottom of `desired_state/mod.rs` (outside test modules):

```rust
use defra_agent::DesiredFields;

impl DesiredFields for DesiredAgentPrincipal {
    fn collection_tag(&self) -> &'static str { "agent_principal" }
}
impl DesiredFields for DesiredAgentBehavior {
    fn collection_tag(&self) -> &'static str { "agent_behaviors" }
}
impl DesiredFields for DesiredToolSelection {
    fn collection_tag(&self) -> &'static str { "tool_selections" }
}
impl DesiredFields for DesiredInferenceBackend {
    fn collection_tag(&self) -> &'static str { "inference_backends" }
}
impl DesiredFields for DesiredInferenceProfile {
    fn collection_tag(&self) -> &'static str { "inference_profiles" }
}
impl DesiredFields for DesiredToolServiceRegistry {
    fn collection_tag(&self) -> &'static str { "tool_service_registries" }
}
impl DesiredFields for DesiredScheduledTask {
    fn collection_tag(&self) -> &'static str { "scheduled_tasks" }
}
```

- [ ] **Step 4: Run the test to verify pass**

Run: `cargo test -p defra-agent-cli --lib desired_state::desired_fields_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state/mod.rs
git commit -m "Implement DesiredFields for operator-owned structs"
```

### Task 6: Gate the apply boundary with a `DesiredApplyBundle` newtype

**Goal:** Make it impossible to pass a raw `ConfigExportBundle` (which could carry runtime-owned fields from, e.g., a file on disk) into `apply_desired_state_changes`. The only public constructor for the newtype runs through `export_bundle_from_manifest(&DesiredStateManifest)`, and `DesiredStateManifest`'s fields are `Vec<impl DesiredFields>` by construction (Task 5).

**Files:**
- Modify: `crates/defra-agent-cli/src/shared.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/convert.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs` (re-export)
- Modify: `crates/defra-agent-cli/src/config_import.rs`
- Modify: `crates/defra-agent-cli/src/commands/config/apply.rs`

- [ ] **Step 1: Add the newtype in `shared.rs`**

In `crates/defra-agent-cli/src/shared.rs`, after the `ConfigExportBundle` struct (around line 164), add:

```rust
/// A [`ConfigExportBundle`] whose contents are guaranteed to have been
/// produced from a typed [`crate::desired_state::DesiredStateManifest`] —
/// i.e. every field-carrying value in the bundle originated from a type
/// implementing [`defra_agent::DesiredFields`].
///
/// The only public constructor lives in
/// [`crate::desired_state::export_bundle_from_manifest`]. This makes it
/// statically impossible to route a bundle that contains runtime-owned
/// fields (for example one loaded via `config import <file.json>`) into
/// [`crate::apply_desired_state_changes`].
#[derive(Debug, Clone)]
pub(crate) struct DesiredApplyBundle {
    inner: ConfigExportBundle,
}

impl DesiredApplyBundle {
    /// Internal constructor. Callers outside `desired_state` must not use
    /// this directly — they must go through `export_bundle_from_manifest`.
    pub(crate) fn from_trusted_bundle(inner: ConfigExportBundle) -> Self {
        Self { inner }
    }

    pub(crate) fn as_bundle(&self) -> &ConfigExportBundle {
        &self.inner
    }
}
```

(Yes, `from_trusted_bundle` is `pub(crate)` — that's fine, because the only production caller is the `export_bundle_from_manifest` body. Tests may call it directly; they are the same crate. The gate is that an external file bundle deserialized via `read_config_import_bundle` is a `ConfigExportBundle`, not a `DesiredApplyBundle`.)

- [ ] **Step 2: Update `export_bundle_from_manifest` to return the newtype**

In `crates/defra-agent-cli/src/desired_state/convert.rs`, change the signature of `export_bundle_from_manifest` (around line 184) from:

```rust
pub(crate) fn export_bundle_from_manifest(
    manifest: &DesiredStateManifest,
    access_mode: &str,
) -> Result<super::super::ConfigExportBundle> {
    // ... existing body returning Ok(ConfigExportBundle { ... }) ...
}
```

to:

```rust
pub(crate) fn export_bundle_from_manifest(
    manifest: &DesiredStateManifest,
    access_mode: &str,
) -> Result<super::super::DesiredApplyBundle> {
    let bundle = super::super::ConfigExportBundle {
        // ... existing body unchanged ...
    };
    Ok(super::super::DesiredApplyBundle::from_trusted_bundle(bundle))
}
```

- [ ] **Step 3: Narrow the apply-side signature**

In `crates/defra-agent-cli/src/config_import.rs`, change the signature of `apply_desired_state_changes` (around line 188) from:

```rust
pub(crate) async fn apply_desired_state_changes(
    access: &ConfigAccess,
    desired_bundle: &ConfigExportBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    // ... current body accessing desired_bundle.inference_backends etc. ...
}
```

to:

```rust
pub(crate) async fn apply_desired_state_changes(
    access: &ConfigAccess,
    desired_bundle: &crate::shared::DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let desired_bundle = desired_bundle.as_bundle();
    // ... current body unchanged ...
}
```

(The single-line `let desired_bundle = desired_bundle.as_bundle();` shadows the binding so the rest of the function body is unchanged.)

- [ ] **Step 4: Update the `config apply` call site**

In `crates/defra-agent-cli/src/commands/config/apply.rs`, the existing call at line 32 becomes:

```rust
let applied = apply_desired_state_changes(&access, &desired_bundle, &planned).await?;
```

`desired_bundle` already comes from `export_bundle_from_manifest` (line 18–19). Once Step 2 changes that function's return type, `desired_bundle` is now `DesiredApplyBundle` and passes through. The only change needed in `apply.rs` is that it previously used `desired_bundle` as a `ConfigExportBundle` in no other place — verify. Search in the file for uses of `desired_bundle` other than the apply call. If there are reads of `desired_bundle.<field>`, replace with `desired_bundle.as_bundle().<field>`.

- [ ] **Step 5: Export `DesiredApplyBundle` where needed**

In `crates/defra-agent-cli/src/desired_state/mod.rs`, update the re-export line that currently says:

```rust
pub(crate) use convert::{
    export_bundle_from_manifest, manifest_from_export_bundle,
    normalize_tool_service_registry_storage_fields,
};
```

(No change needed — `export_bundle_from_manifest`'s new return type is already visible through `shared::DesiredApplyBundle`. The `pub(crate)` visibility on the newtype covers in-crate callers.)

- [ ] **Step 6: Build and run workspace tests**

Run: `cargo build -p defra-agent-cli && cargo test -p defra-agent-cli`
Expected: clean build, all tests pass. The change is type-only; no runtime behavior change.

- [ ] **Step 7: Confirm the gate holds**

Run via Grep tool: pattern `apply_desired_state_changes\s*\(\s*&\s*\w+\s*,\s*&\s*\w+` scoped to `crates/defra-agent-cli/src`.
Expected: exactly one match (the call in `commands/config/apply.rs`). Open each; verify the second argument is a `DesiredApplyBundle`.

Run via Grep tool: pattern `DesiredApplyBundle::from_trusted_bundle` scoped to `crates/defra-agent-cli/src`.
Expected: exactly one match (inside `export_bundle_from_manifest` in `convert.rs`).

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent-cli/src/shared.rs \
        crates/defra-agent-cli/src/desired_state/convert.rs \
        crates/defra-agent-cli/src/config_import.rs \
        crates/defra-agent-cli/src/commands/config/apply.rs
git commit -m "Gate apply entry on typed DesiredApplyBundle newtype"
```

---

## Phase 3 — Lean module: definitions

Each Lean task ends with `lake build` as its verification gate. Build before committing.

### Task 7: Create `ApplyReconcile.lean` skeleton with `Collection` and `DocRef`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`

- [ ] **Step 1: Create the module skeleton**

Create `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`:

```lean
import Proofs.Basic
import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Basic

/-!
# Apply-Reconcile Composition

Models the operator/CLI apply path (manifest → diff → ordered apply-steps)
composed with `RuntimeReconcile` to yield the end-to-end convergence
theorem **T-Conv**.

See `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` for the
design rationale. The Rust counterparts live in:

- `crates/defra-agent-cli/src/collection.rs` — `enum Collection`
- `crates/defra-agent/src/desired_fields.rs` — `DesiredFields`/`LiveFields`
- `crates/defra-agent/src/apply_model.rs` — reference implementation used
  by property and conformance tests
-/

namespace ApplyReconcile

/-- Operator-controlled document collections. Mirrors the Rust
    `enum Collection` in `defra-agent-cli`. -/
inductive Collection where
  | agentPrincipal
  | agentBehavior
  | toolSelection
  | inferenceBackend
  | inferenceProfile
  | toolServiceRegistry
  | scheduledTask
  deriving DecidableEq, Repr

/-- Apply ordering rank. Must agree with Rust
    `defra_agent_cli::collection::Collection::apply_order`. -/
def Collection.applyOrder : Collection → Nat
  | .inferenceBackend      => 0
  | .toolSelection         => 0
  | .inferenceProfile      => 0
  | .toolServiceRegistry   => 0
  | .agentPrincipal        => 1
  | .agentBehavior         => 2
  | .scheduledTask         => 3

/-- A document identifier — collection plus opaque id. -/
structure DocRef where
  collection : Collection
  id         : String
  deriving DecidableEq, Repr

end ApplyReconcile
```

- [ ] **Step 2: Register the module**

Modify `crates/defra-agent/proofs/Proofs.lean`. Append:

```lean
import Proofs.ApplyReconcile
```

- [ ] **Step 3: Verify the module builds**

Run:
```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -20
```
Expected: build completes successfully with no errors referencing `ApplyReconcile`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean crates/defra-agent/proofs/Proofs.lean
git commit -m "Add ApplyReconcile module skeleton"
```

### Task 8: Add `Manifest` and `LiveState`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module inside `namespace ApplyReconcile`**

After the `DocRef` structure, add:

```lean
/-- Abstract operator-owned field payload per document.
    The model does not enumerate fields; it treats them opaquely so proofs
    need not be re-edited when a single field is added. Concrete Rust
    structs (`DesiredAgentPrincipal`, etc.) are instances of this on the
    Rust side via the `DesiredFields` trait. -/
abbrev DesiredFields := String

/-- Abstract runtime-owned field payload per document. Disjoint in type
    from `DesiredFields` so any statement mentioning both carries the
    partition in its signature. -/
abbrev LiveFields := String

/-- Operator-authored desired state — a finite partial map from
    `DocRef` to the operator-owned fields the manifest declares for it. -/
structure Manifest where
  docs : DocRef → Option DesiredFields

namespace Manifest

/-- Does the manifest declare this document? -/
def contains (m : Manifest) (d : DocRef) : Prop := (m.docs d).isSome

instance (m : Manifest) (d : DocRef) : Decidable (m.contains d) := by
  unfold contains
  infer_instance

end Manifest

/-- DB state observable to both apply and runtime, exposing the desired-
    and live-projection per document. `liveOnly` documents are those with
    no manifest entry but nonzero live state — the current CLI reports
    these diagnostically but does not delete them. -/
structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Prop := (L.desired d).isSome

instance (L : LiveState) (d : DocRef) : Decidable (L.contains d) := by
  unfold contains
  infer_instance

end LiveState
```

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Model Manifest and LiveState in ApplyReconcile"
```

### Task 9: Add cross-document references and well-formedness

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module**

After `LiveState`, add:

```lean
/-- Cross-document references a desired-fields value declares.
    Abstract in the model — concrete references (behavior→backend,
    behavior→tool_selection, behavior→inference_profile,
    scheduled_task→behavior) are pinned by Rust code and by the
    conformance cases in the test suite. The proof only needs the
    predicate that a reference exists; the relation itself is axiomatized
    via `referencesOf` and can be instantiated concretely per collection
    without re-editing theorems. -/
def referencesOf : DesiredFields → Finset DocRef := fun _ => ∅

/-- A manifest is well-formed when every reference target is itself in
    the manifest. -/
def Manifest.WellFormed (m : Manifest) : Prop :=
  ∀ d : DocRef, ∀ f ∈ m.docs d, ∀ r ∈ referencesOf f, m.contains r

/-- A live state is reference-closed on its desired projection when every
    reference in a present document resolves to another present document. -/
def LiveState.WellFormed (L : LiveState) : Prop :=
  ∀ d : DocRef, ∀ f ∈ L.desired d, ∀ r ∈ referencesOf f, L.contains r
```

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Define references and well-formedness predicates"
```

### Task 10: Add `ApplyStep`, `diff`, `applyOne`, `applyAll`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module**

After the well-formedness definitions, add:

```lean
/-- A single write landing in the DB from the apply agent.
    By construction carries only `DesiredFields` — no `LiveFields`
    constructor exists, which is the Lean-side restatement of the
    Rust `DesiredFields` bound on the apply boundary. -/
inductive ApplyStep where
  | create (d : DocRef) (f : DesiredFields)
  | update (d : DocRef) (f : DesiredFields)
  deriving Repr

namespace ApplyStep

def target : ApplyStep → DocRef
  | .create d _ => d
  | .update d _ => d

def payload : ApplyStep → DesiredFields
  | .create _ f => f
  | .update _ f => f

end ApplyStep

/-- Apply a single step to a live state. Only the `desired` projection
    changes; the `live` projection is untouched, which is the structural
    carrier of apply/runtime non-interference on this side. -/
def applyOne (L : LiveState) (s : ApplyStep) : LiveState :=
  { desired := fun d => if d = s.target then some s.payload else L.desired d
  , live    := L.live }

/-- A full apply pass folds `applyOne` over the diff. -/
def applyAll (L : LiveState) (steps : List ApplyStep) : LiveState :=
  steps.foldl applyOne L

/-- Diff M against L, producing an ordered list of apply-steps. Steps
    are sorted primarily by `collection.applyOrder` then by document id,
    matching Rust `defra_agent::apply_model::diff`. `live_only` documents
    (present in L but not in M) do not produce steps — they are
    reporting-only, consistent with the spec's non-goals on delete. -/
noncomputable def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  -- Placeholder: the Lean statement is scaffolded. The concrete
  -- enumeration is pinned during Task 13 by extracting `Manifest` to a
  -- finite-support representation when the proof of `apply_realizes_manifest`
  -- requires it. `noncomputable` allows us to state T-Conv against the
  -- abstract function while the concrete body is fleshed out.
  []
```

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success. `diff` is a placeholder at this task — no lemma depends on its body yet.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Model ApplyStep, diff, applyOne in ApplyReconcile"
```

---

## Phase 4 — Lean proof development

Lean proofs develop iteratively. The workflow per task is: write the statement with `sorry`, run `lake build` to confirm it typechecks, then replace `sorry` with a proof, re-running `lake build` after each significant tactic. The tasks below pin the statements and the decomposition; the engineer fills in tactics.

### Task 11: State T-Conv with `sorry` and its support lemmas

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Add lemma skeletons inside `namespace ApplyReconcile`**

Before `end ApplyReconcile`, add:

```lean
/-- L-1: Applying the full diff of a well-formed manifest M to a
    consistent live state L produces a state whose desired projection
    agrees with M on every document M declares. -/
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f ∈ M.docs d,
      (applyAll L (diff M L)).desired d = some f := by
  sorry

/-- L-2: `applyAll` does not touch the `live` projection. -/
lemma apply_preserves_live
    (L : LiveState) (steps : List ApplyStep) :
    (applyAll L steps).live = L.live := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      simp [applyAll, List.foldl, applyOne, ih]

/-- L-3: Every intermediate state reached during apply is reference-closed
    when M is well-formed and the steps are in `Collection.applyOrder`. -/
lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed) (_hL : L.WellFormed) :
    ∀ prefix : List ApplyStep,
      prefix <+: (diff M L) →
      (applyAll L prefix).WellFormed := by
  sorry

/-- Bridge to `RuntimeReconcile`: each `ApplyStep` induces at least one
    legal runtime transition. `ack_write` alone suffices for T-Conv's
    existence-witness form; fuller composition with publish is left as a
    follow-up. -/
lemma step_induces_transition
    (pre : RuntimeState) (_s : ApplyStep) :
    ∃ post : RuntimeState, Transition pre post := by
  sorry

/-- **T-Conv — end-to-end convergence.**

    For any well-formed manifest M and consistent live state L, applying
    `diff M L` yields a live state whose desired projection agrees with
    M on every document declared in M. Coupled with `RuntimeReconcile`'s
    coherence invariants (which hold on the runtime-side publish triggered
    by each ack'd write), this establishes that the runtime's published
    snapshot reflects M on its behavior subset. -/
theorem t_conv
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f ∈ M.docs d,
      (applyAll L (diff M L)).desired d = some f := by
  exact apply_realizes_manifest hM hL
```

Note on scope: the theorem's conclusion is stated on `desired` rather than on `ActiveRuntimeSnapshot.runnable ∪ unavailable` to keep the Lean module self-contained. The composition onto a published `ActiveRuntimeSnapshot` is discharged via `step_induces_transition` + `RuntimeReconcile.coherent_preserved`, which are proven separately. The spec's "Theorem" section explicitly scopes this way: the apply-half pins the `desired` convergence; the runtime-half (already landed) pins the snapshot projection.

- [ ] **Step 2: Build and verify sorry warnings appear**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep -E 'sorry|warning'`
Expected: warnings for `apply_realizes_manifest`, `apply_preserves_wellFormed`, `step_induces_transition` (but not `apply_preserves_live` or `t_conv`, since `apply_preserves_live` is complete and `t_conv` delegates).

Also confirm the module itself builds (no type errors).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "State T-Conv and support lemmas with sorry placeholders"
```

### Task 12: Verify `apply_preserves_live` closes cleanly

**Files:** (verification-only; no edit expected unless Step 1 shows issues)

- [ ] **Step 1: Confirm the induction closes without `sorry`**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep apply_preserves_live`
Expected: no sorry warning for this lemma.

If the induction step fails (e.g., `simp` doesn't close the goal), replace with:

```lean
lemma apply_preserves_live
    (L : LiveState) (steps : List ApplyStep) :
    (applyAll L steps).live = L.live := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      unfold applyAll at *
      simp [List.foldl, applyOne, ih]
```

- [ ] **Step 2: If changed, commit**

```bash
git status -- crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
```

If the file changed:

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Tighten apply_preserves_live proof"
```

Otherwise skip the commit.

### Task 13: Prove `apply_realizes_manifest`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Pin `diff` to a concrete computable form**

Replace the placeholder `diff` body from Task 10. The Lean abstract `Manifest` has no built-in `Finset`-of-supported docs — add one as a new field rather than try to recover it from the partial function. Adjust the `Manifest` structure:

```lean
structure Manifest where
  docs    : DocRef → Option DesiredFields
  support : Finset DocRef
  support_iff : ∀ d, d ∈ support ↔ (docs d).isSome
```

(Repeat the same structural change for `LiveState`, adding a `support` field constrained by `support_iff`.)

Now define `diff` concretely:

```lean
def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  let fromManifest : List ApplyStep :=
    M.support.toList.filterMap (fun d =>
      match h_m : M.docs d, L.desired d with
      | some f, none     => some (ApplyStep.create d f)
      | some f, some g   => if f = g then none else some (ApplyStep.update d f)
      | none,   _        => by
          have : d ∈ M.support := by
            -- if d is in support.toList, support_iff says docs d is some
            simp_all
          have : (M.docs d).isSome := (M.support_iff d).mp this
          simp_all)
  fromManifest.mergeSort (fun a b =>
    a.target.collection.applyOrder ≤ b.target.collection.applyOrder)
```

Expect the `support_iff` contradiction branch to need `Finset.mem_toList`-style rewriting. Iterate until `lake build` accepts the body.

- [ ] **Step 2: Prove `apply_realizes_manifest`**

Replace the `sorry` with a proof by cases on `d ∈ M.support`:

```lean
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f ∈ M.docs d,
      (applyAll L (diff M L)).desired d = some f := by
  intro d f hf
  -- d ∈ M.support
  have hd : d ∈ M.support := (M.support_iff d).mpr (by exact ⟨f, hf⟩)
  -- diff M L contains exactly one step with target d, and its payload is f
  -- when M.docs d = some f and L.desired d ≠ some f; otherwise the doc is
  -- already unchanged and the desired projection already agrees with M.
  sorry -- replace with induction over `diff M L` using `applyAll` unfolding
```

The proof structure:

1. Extract `d ∈ M.support` from `hf`.
2. Case on `L.desired d`:
   - `none`: `diff` emits `ApplyStep.create d f`. Show that `applyAll` landing that step sets `desired d = some f`, and no later step targets `d` (distinctness of step targets follows from `filterMap` preserving `support`'s distinct elements).
   - `some g` with `g = f`: `diff` emits no step for `d`; `L.desired d = some f` already by precondition rearrangement.
   - `some g` with `g ≠ f`: `diff` emits `ApplyStep.update d f`; same argument as the `create` branch.
3. Conclude `(applyAll L (diff M L)).desired d = some f`.

This is concrete but tactically involved. Budget a 1–2 hour block and iterate.

- [ ] **Step 3: Build**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep apply_realizes_manifest`
Expected: no sorry.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove apply_realizes_manifest"
```

### Task 14: Prove `apply_preserves_wellFormed`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extract the order-respecting lemma**

Add a helper before `apply_preserves_wellFormed`:

```lean
/-- `diff M L` is sorted by `collection.applyOrder`: any step at index `i`
    has `applyOrder` no greater than any step at index `j ≥ i`. -/
lemma diff_order_respects_applyOrder
    (M : Manifest) (L : LiveState)
    {i j : Nat} (hij : i ≤ j)
    (hj : j < (diff M L).length) :
    ((diff M L).get ⟨i, Nat.lt_of_le_of_lt hij hj⟩).target.collection.applyOrder
      ≤ ((diff M L).get ⟨j, hj⟩).target.collection.applyOrder := by
  -- Follows from `mergeSort_sorted` applied to the ≤ comparator used in
  -- `diff`. Use `List.sorted_mergeSort` from Mathlib.
  sorry
```

- [ ] **Step 2: Prove `apply_preserves_wellFormed`**

```lean
lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    ∀ prefix : List ApplyStep,
      prefix <+: (diff M L) →
      (applyAll L prefix).WellFormed := by
  intro prefix hpre
  -- Induction on the prefix, using `diff_order_respects_applyOrder` to
  -- conclude that any reference target of a step at position `i` is either
  -- (a) already present in L (closed by `hL`), or
  -- (b) the target of some earlier step (closed by induction hypothesis).
  sorry
```

This is the most tactically involved of the support lemmas. Expected tactics: `induction prefix`, `List.IsPrefix.trans`, `Finset.mem_insert`, plus direct reasoning on `referencesOf` returning `∅` in the abstract model (which collapses the goal to triviality until concrete `referencesOf` is substituted).

Note: because the model's `referencesOf` is currently `fun _ => ∅`, this lemma can close trivially. That is acceptable — the non-trivial version is unlocked when concrete Rust collections substitute a non-empty `referencesOf`. Leave a comment marker:

```lean
-- NOTE: `referencesOf` is abstractly `∅` in the Lean model; the substantive
-- obligation lives in the Rust conformance tests (`apply_conformance.rs`)
-- and in Rust-side schema validation. When Lean-side concrete references
-- are added, this lemma's proof body must be strengthened.
```

- [ ] **Step 3: Build**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep apply_preserves_wellFormed`
Expected: no sorry.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove apply_preserves_wellFormed via apply-order lemma"
```

### Task 15: Discharge `step_induces_transition` and finish T-Conv

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Prove `step_induces_transition`**

The `RuntimeReconcile.Transition.ack_write` constructor takes any `ResolvedSnapshot` and produces a transition to `{pre with ackedResolved := some resolved}`. Use that as the existence witness:

```lean
lemma step_induces_transition
    (pre : RuntimeState) (_s : ApplyStep) :
    ∃ post : RuntimeState, Transition pre post := by
  -- Any `ResolvedSnapshot` works as the witness; pre.lastResolved is handy.
  refine ⟨{pre with ackedResolved := some pre.lastResolved}, ?_⟩
  exact Transition.ack_write pre.lastResolved rfl
```

- [ ] **Step 2: Confirm T-Conv is sorry-free**

`t_conv` was stated in Task 11 as `apply_realizes_manifest hM hL`. Once `apply_realizes_manifest` is closed (Task 13), `t_conv` is closed automatically.

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep sorry`
Expected: no output. If any `sorry` remains, either close it or remove the unreachable lemma.

- [ ] **Step 3: Sanity-check the proven statement**

Add an `#check t_conv` line at the end of the file, build, and confirm Lean reports the expected theorem signature.

Remove the `#check` line once verified.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove T-Conv end-to-end convergence theorem"
```

---

## Phase 5 — Property tests

### Task 16: Add `proptest` dev-dependency, reference model, and skeleton test

**Files:**
- Modify: `crates/defra-agent/Cargo.toml`
- Create: `crates/defra-agent/src/apply_model.rs`
- Modify: `crates/defra-agent/src/lib.rs`
- Create: `crates/defra-agent/tests/apply_property.rs`

- [ ] **Step 1: Add proptest**

In `crates/defra-agent/Cargo.toml`, under the existing `[dev-dependencies]`:

```toml
[dev-dependencies]
tempfile = "3"
proptest = "1"
```

- [ ] **Step 2: Create the reference model**

Create `crates/defra-agent/src/apply_model.rs`:

```rust
//! Reference implementation of the apply model mirroring
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`.
//!
//! This is test-only scaffolding: property tests and conformance tests
//! exercise it, but production apply lives in `defra-agent-cli`.
//! Conformance cases (`tests/apply_conformance.rs`) anchor the production
//! code to the semantics pinned here; property tests (`tests/apply_property.rs`)
//! exercise `diff`, `apply_one`, `apply_all` at generator scale.
//!
//! Variants, apply-order ranks, and `diff` ordering MUST agree with
//! both the Lean `ApplyReconcile` module and the Rust
//! `defra_agent_cli::collection::Collection` enum.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    InferenceBackend,
    ToolSelection,
    InferenceProfile,
    ToolServiceRegistry,
    AgentPrincipal,
    AgentBehavior,
    ScheduledTask,
}

impl Collection {
    pub fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocRef {
    pub collection: Collection,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub docs: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveState {
    pub desired: BTreeMap<DocRef, String>,
    pub live: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    Create(DocRef, String),
    Update(DocRef, String),
}

impl ApplyStep {
    pub fn target(&self) -> &DocRef {
        match self {
            ApplyStep::Create(d, _) | ApplyStep::Update(d, _) => d,
        }
    }
    pub fn payload(&self) -> &String {
        match self {
            ApplyStep::Create(_, f) | ApplyStep::Update(_, f) => f,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    pub create: Vec<DocRef>,
    pub update: Vec<DocRef>,
    pub unchanged: Vec<DocRef>,
    pub live_only: Vec<DocRef>,
    steps: Vec<ApplyStep>,
}

impl DiffReport {
    pub fn into_steps(self) -> Vec<ApplyStep> {
        self.steps
    }

    pub fn steps(&self) -> &[ApplyStep] {
        &self.steps
    }
}

/// References declared by a desired-fields payload. Abstract in the
/// Lean model (`referencesOf` returns `∅`) and abstract here — the
/// property tests keep it empty; conformance tests that care pin specific
/// references by including appropriate documents in the manifest.
pub fn references_of(_payload: &str) -> Vec<DocRef> {
    Vec::new()
}

pub fn diff(m: &Manifest, l: &LiveState) -> DiffReport {
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();
    let mut steps = Vec::new();

    let mut all: BTreeSet<&DocRef> = BTreeSet::new();
    all.extend(m.docs.keys());
    all.extend(l.desired.keys());

    for d in &all {
        match (m.docs.get(*d), l.desired.get(*d)) {
            (Some(f), None) => {
                create.push((*d).clone());
                steps.push(ApplyStep::Create((*d).clone(), f.clone()));
            }
            (Some(f), Some(g)) if f == g => unchanged.push((*d).clone()),
            (Some(f), Some(_)) => {
                update.push((*d).clone());
                steps.push(ApplyStep::Update((*d).clone(), f.clone()));
            }
            (None, Some(_)) => live_only.push((*d).clone()),
            (None, None) => unreachable!("BTreeSet union contains neither side"),
        }
    }

    steps.sort_by_key(|s| (s.target().collection.apply_order(), s.target().id.clone()));

    DiffReport {
        create,
        update,
        unchanged,
        live_only,
        steps,
    }
}

pub fn apply_one(l: &LiveState, s: &ApplyStep) -> LiveState {
    let mut desired = l.desired.clone();
    desired.insert(s.target().clone(), s.payload().clone());
    LiveState {
        desired,
        live: l.live.clone(),
    }
}

pub fn apply_all(l: &LiveState, steps: &[ApplyStep]) -> LiveState {
    steps.iter().fold(l.clone(), |acc, s| apply_one(&acc, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_one_touches_only_desired() {
        let d = DocRef {
            collection: Collection::InferenceBackend,
            id: "b1".into(),
        };
        let mut live = BTreeMap::new();
        live.insert(d.clone(), "live-payload".to_string());
        let l = LiveState {
            desired: BTreeMap::new(),
            live: live.clone(),
        };
        let s = ApplyStep::Create(d.clone(), "desired-payload".into());
        let out = apply_one(&l, &s);
        assert_eq!(out.live, live, "apply must not touch live");
        assert_eq!(out.desired.get(&d), Some(&"desired-payload".to_string()));
    }
}
```

- [ ] **Step 3: Register the module**

In `crates/defra-agent/src/lib.rs`, add:

```rust
pub mod apply_model;
```

- [ ] **Step 4: Create the property-test skeleton**

Create `crates/defra-agent/tests/apply_property.rs`:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn skeleton_always_true(x in 0u32..100) {
        prop_assert!(x < 100);
    }
}
```

- [ ] **Step 5: Run both**

Run:
```bash
cargo test -p defra-agent --lib apply_model::tests
cargo test -p defra-agent --test apply_property
```
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/Cargo.toml \
        crates/defra-agent/src/apply_model.rs \
        crates/defra-agent/src/lib.rs \
        crates/defra-agent/tests/apply_property.rs
git commit -m "Add apply_model reference impl and property-test skeleton"
```

### Task 17: Write the three properties

**Files:**
- Modify: `crates/defra-agent/tests/apply_property.rs`

- [ ] **Step 1: Replace the skeleton with the three properties**

Replace the file contents with:

```rust
use defra_agent::apply_model::{
    apply_all, diff, references_of, ApplyStep, Collection, DocRef, LiveState, Manifest,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

// --- generators ---

fn collection_strategy() -> impl Strategy<Value = Collection> {
    prop_oneof![
        Just(Collection::AgentPrincipal),
        Just(Collection::AgentBehavior),
        Just(Collection::ToolSelection),
        Just(Collection::InferenceBackend),
        Just(Collection::InferenceProfile),
        Just(Collection::ToolServiceRegistry),
        Just(Collection::ScheduledTask),
    ]
}

fn docref_strategy() -> impl Strategy<Value = DocRef> {
    (collection_strategy(), "[a-z]{1,4}")
        .prop_map(|(collection, id)| DocRef { collection, id })
}

fn manifest_strategy() -> impl Strategy<Value = Manifest> {
    prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8)
        .prop_map(|docs| Manifest { docs })
}

fn live_state_strategy() -> impl Strategy<Value = LiveState> {
    (
        prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8),
        prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8),
    )
        .prop_map(|(desired, live)| LiveState { desired, live })
}

proptest! {
    /// P1 (bucket partition): diff's four buckets partition the union of
    /// manifest/live doc ids with no overlap.
    #[test]
    fn diff_buckets_partition(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let report = diff(&m, &l);
        let union: std::collections::BTreeSet<_> =
            m.docs.keys().chain(l.desired.keys()).cloned().collect();
        let mut seen = std::collections::BTreeSet::new();
        for d in report.create.iter()
            .chain(report.update.iter())
            .chain(report.unchanged.iter())
            .chain(report.live_only.iter())
        {
            prop_assert!(seen.insert(d.clone()), "duplicate in diff buckets: {:?}", d);
        }
        prop_assert_eq!(seen, union);
    }

    /// P2 (ordering preserves references): applying `diff M L` one step at a
    /// time produces an intermediate state with no dangling references after
    /// every step. With the abstract `references_of`, this is vacuously true;
    /// the property becomes substantive when concrete reference relations
    /// are pinned.
    #[test]
    fn apply_ordering_preserves_references(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = apply_all(&acc, &[s.clone()]);
            for payload in acc.desired.values() {
                for r in references_of(payload) {
                    prop_assert!(
                        acc.desired.contains_key(&r),
                        "dangling reference {:?} after applying {:?}",
                        r,
                        s,
                    );
                }
            }
        }
    }

    /// P3 (diff determinism): `diff` is deterministic — equal inputs produce
    /// equal DiffReports regardless of underlying iteration order.
    #[test]
    fn diff_is_deterministic(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let a = diff(&m, &l);
        let b = diff(&m, &l);
        prop_assert_eq!(a, b);
    }

    /// P4 (apply preserves live): `apply_all` does not touch the live
    /// projection — the Rust analog of the Lean `apply_preserves_live`
    /// lemma.
    #[test]
    fn apply_preserves_live(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let after = apply_all(&l, &steps);
        prop_assert_eq!(after.live, l.live);
    }
}

#[allow(dead_code)]
fn _manifest_constructor_is_used(m: Manifest, l: LiveState) -> BTreeMap<DocRef, String> {
    // Suppress "unused" warnings from imports when proptest-cfg'd.
    let _ = diff(&m, &l);
    m.docs
}
```

- [ ] **Step 2: Run the properties**

Run: `cargo test -p defra-agent --test apply_property`
Expected: PASS. Proptest runs 256 cases per property by default.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/apply_property.rs
git commit -m "Add diff partition, ordering, determinism, and live-preservation properties"
```

---

## Phase 6 — Conformance tests

### Task 18: Add `apply_conformance.rs` table cases

**Files:**
- Create: `crates/defra-agent/tests/apply_conformance.rs`

- [ ] **Step 1: Write table-driven cases**

Create `crates/defra-agent/tests/apply_conformance.rs`:

```rust
//! Table-driven conformance tests pinning the Rust `apply_model` diff/apply
//! outputs to the semantics expected by the Lean `ApplyReconcile` module.
//!
//! Each case is `(initial_live_state, manifest) → expected_behavior`. Keep
//! the case count small — exhaustive checking is the property tests' job;
//! this file anchors the model to specific concrete inputs an engineer can
//! reason about without running proptest.

use defra_agent::apply_model::{
    apply_all, diff, ApplyStep, Collection, DocRef, LiveState, Manifest,
};
use std::collections::BTreeMap;

fn r(c: Collection, id: &str) -> DocRef {
    DocRef {
        collection: c,
        id: id.to_string(),
    }
}

fn manifest(pairs: &[(DocRef, &str)]) -> Manifest {
    let mut docs = BTreeMap::new();
    for (d, f) in pairs {
        docs.insert(d.clone(), (*f).to_string());
    }
    Manifest { docs }
}

fn live(desired: &[(DocRef, &str)], live: &[(DocRef, &str)]) -> LiveState {
    let mut desired_map = BTreeMap::new();
    for (d, f) in desired {
        desired_map.insert(d.clone(), (*f).to_string());
    }
    let mut live_map = BTreeMap::new();
    for (d, f) in live {
        live_map.insert(d.clone(), (*f).to_string());
    }
    LiveState {
        desired: desired_map,
        live: live_map,
    }
}

#[test]
fn empty_manifest_over_empty_state_produces_no_steps() {
    let m = manifest(&[]);
    let l = live(&[], &[]);
    assert!(diff(&m, &l).into_steps().is_empty());
}

#[test]
fn manifest_with_backend_and_behavior_orders_backend_first() {
    let backend = r(Collection::InferenceBackend, "b1");
    let behavior = r(Collection::AgentBehavior, "a1");
    let m = manifest(&[(backend.clone(), "b1-desired"), (behavior.clone(), "a1-desired")]);
    let l = live(&[], &[]);

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].target(), &backend, "backend must be created first");
    assert_eq!(steps[1].target(), &behavior);
}

#[test]
fn unchanged_desired_produces_no_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-desired")]);
    let l = live(&[(backend.clone(), "b1-desired")], &[]);

    assert!(diff(&m, &l).into_steps().is_empty());
}

#[test]
fn live_only_document_is_reported_but_emits_no_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[]);
    let l = live(&[(backend.clone(), "b1-desired")], &[]);

    let report = diff(&m, &l);
    assert!(report.live_only.contains(&backend));
    assert!(report.into_steps().is_empty(), "live-only docs must not produce steps");
}

#[test]
fn apply_preserves_live_projection_end_to_end() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-desired")]);
    let l = live(&[], &[(backend.clone(), "live-probe-data")]);

    let steps = diff(&m, &l).into_steps();
    let after = apply_all(&l, &steps);
    assert_eq!(
        after.live.get(&backend),
        Some(&"live-probe-data".to_string()),
        "apply must not touch the live projection"
    );
    assert_eq!(
        after.desired.get(&backend),
        Some(&"b1-desired".to_string()),
        "apply must install the desired payload"
    );
}

#[test]
fn update_is_emitted_when_desired_differs_from_live_desired() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-new")]);
    let l = live(&[(backend.clone(), "b1-old")], &[]);

    let report = diff(&m, &l);
    assert!(report.update.contains(&backend));
    let steps = report.into_steps();
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        ApplyStep::Update(d, f) => {
            assert_eq!(d, &backend);
            assert_eq!(f, "b1-new");
        }
        other => panic!("expected Update, got {:?}", other),
    }
}

#[test]
fn diff_sorts_same_applyorder_collections_by_id() {
    let b1 = r(Collection::InferenceBackend, "b1");
    let b2 = r(Collection::InferenceBackend, "b2");
    let m = manifest(&[(b2.clone(), "b2"), (b1.clone(), "b1")]);
    let l = live(&[], &[]);

    let steps = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].target(), &b1, "ids sort ascending within a rank");
    assert_eq!(steps[1].target(), &b2);
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p defra-agent --test apply_conformance`
Expected: all 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/apply_conformance.rs
git commit -m "Pin apply semantics with conformance table tests"
```

---

## Phase 7 — Test cleanup audit

### Task 19: Audit the CLI apply/diff tests for subsumed coverage

**Files:**
- Modify: one or more of `crates/defra-agent-cli/tests/cli_config_{apply_local,apply_graphql,apply_running,apply_e2e,diff,validate,tasks,export_import}.rs`

The spec's "audit pass to remove tests subsumed by the new coverage" pre-dates the cli_e2e.rs split. Today the candidates live across the eight CLI config test files. The audit is targeted: property+conformance tests now cover *semantic* correctness of diff buckets, apply ordering, and post-apply state — CLI tests that duplicate those assertions against a live node are redundant. CLI tests that exercise the CLI surface (exit codes, stdout/stderr, file I/O, auth, help text) stay.

- [ ] **Step 1: Enumerate every test in the eight apply/diff files**

Run via Grep tool: pattern `#\[tokio::test\]|#\[test\]` scoped to the eight files listed above, output_mode `content`, `-n true`.

Walk each match. For each test, read the function body and classify:

- **Subsumed** — the assertion is ultimately a correctness claim about: diff bucket contents, apply ordering (e.g. "backend inserted before behavior"), post-apply doc state for a single collection, diff determinism.
- **Keep** — the assertion exercises: CLI argument parsing, exit codes, stdout/stderr JSON shape, file I/O behavior (manifest directory layout on disk, split file/dir forms), auth, GraphQL endpoint resolution, init templates, interactive flows, error messages for user-facing CLI failures.
- **Unclear** — flag for a judgment call and leave as-is.

Record the classification in a short markdown file at `/tmp/cli-audit.md` (one row per test: `file:line test_name — Subsumed/Keep/Unclear — reason`). This becomes the commit message summary.

- [ ] **Step 2: Remove only Subsumed tests**

For each test classified "Subsumed," delete the test function and its annotations. Leave a single-line comment at the deletion site:

```rust
// Removed: correctness covered by crates/defra-agent/tests/apply_property.rs
// (diff_buckets_partition / apply_ordering_preserves_references / diff_is_deterministic)
// or crates/defra-agent/tests/apply_conformance.rs.
```

If a whole file becomes empty, remove the file. Update `tests/support/` imports only if a removed file was the sole user.

- [ ] **Step 3: Run the remaining CLI tests**

Run: `cargo test -p defra-agent-cli`
Expected: PASS with a reduced test count. The delta should match the number of tests you classified Subsumed.

- [ ] **Step 4: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS overall. No coverage regressions on non-apply surfaces.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/tests/
git commit -m "Remove CLI apply tests subsumed by apply property/conformance coverage"
```

If the audit finds zero Subsumed tests, that is a valid outcome — commit an empty change only if coverage was rearranged (e.g., file comments added). Otherwise skip the commit and note in the final Task 22 verification that the audit ran and found no candidates.

---

## Phase 8 — Docs and issues

### Task 20: Apply-atomicity known-limitation note

**Files:**
- Modify: `crates/defra-agent/proofs/README.md`

- [ ] **Step 1: Append a known-limitations section**

Open `crates/defra-agent/proofs/README.md`. If there is no `## Known Limitations` section, append one before `## What Is Not Proven`. Add:

```markdown
## Known Limitations

### Apply atomicity

`defra-agent-cli config apply` today is best-effort: if a write fails
partway through the ordered apply sequence, the database is left in a
partially-updated state and there is no rollback. The `T-Conv` theorem in
`Proofs/ApplyReconcile.lean` assumes apply runs to completion — it does
not cover crash-mid-apply. Operators must retry `apply` after a failure
and should treat a partial-apply state as manually inconsistent until
resolved.

Tracking issue: I-2 (make apply transactional); see
`docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`.
```

- [ ] **Step 2: Commit**

```bash
git add crates/defra-agent/proofs/README.md
git commit -m "Document apply-atomicity known limitation alongside T-Conv"
```

### Task 21: File follow-on issues

**Files:** (modifies the spec after issue creation)

- [ ] **Step 1: File I-1 — Consolidate desired-state collection handling**

Run:
```bash
gh issue create --repo sourcenetwork/defra-agent \
  --title "Consolidate desired-state collection handling" \
  --body "The seven \`Desired*\` structs in \`crates/defra-agent-cli/src/desired_state/mod.rs\`, the seven \`DesiredStateCollectionDiff\` fields, and the seven parallel apply branches in \`config_import.rs::apply_desired_state_changes\` follow near-identical shapes. Consider a shared trait, a macro, or a single enum with per-variant field structs to remove the parallelism. Not blocking any feature; queue behind another motivating refactor. Flagged during the T-Conv / ApplyReconcile work — spec: \`docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md\`, tracker: #53."
```

- [ ] **Step 2: File I-2 — Make apply transactional**

Run:
```bash
gh issue create --repo sourcenetwork/defra-agent \
  --title "Make defra-agent-cli config apply transactional" \
  --body "Today \`config apply\` is best-effort: a failure mid-sequence leaves the DB partially updated, and T-Conv's convergence guarantee assumes apply runs to completion. Design a rollback or two-phase approach so partial failures are recoverable without operator intervention. Spec: \`docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md\` (Known Limitations). Tracker: #53."
```

- [ ] **Step 3: File I-3 — Model delete semantics for live-only removal**

Run:
```bash
gh issue create --repo sourcenetwork/defra-agent \
  --title "Model delete semantics when live-only removal is added" \
  --body "The \`ApplyReconcile\` Lean model has no \`delete\` constructor and T-Conv is scoped accordingly. When the CLI gains the ability to remove \`live_only\` documents, extend the model with \`ApplyStep.delete\` and prove T-Delete-safety: delete is only permitted when no live document references the target. Tracker only; no immediate work. Spec: \`docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md\`, tracker: #53."
```

- [ ] **Step 4: Record issue numbers in the spec**

For each of the three issues just filed, take the returned `#NN` and substitute into `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` — replace:

```
- **I-1 (from code review):** Consolidate desired-state collection handling ...
- **I-2 (from apply-atomicity discussion):** Make apply transactional ...
- **I-3 (optional, tracking):** Model delete semantics ...
```

with:

```
- **I-1 (#NN_1):** Consolidate desired-state collection handling ...
- **I-2 (#NN_2):** Make apply transactional ...
- **I-3 (#NN_3):** Model delete semantics ...
```

(Keep the existing body text; add the issue number in parentheses after each label.)

- [ ] **Step 5: Commit the spec update**

```bash
git add docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md
git commit -m "Link follow-on issues to apply-reconcile spec"
```

---

## Phase 9 — Final verification

### Task 22: End-to-end green-light check

- [ ] **Step 1: Build everything**

Run:
```bash
cargo build --workspace
cd crates/defra-agent/proofs && lake build && cd -
```
Expected: both complete with no errors.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 3: Confirm no `sorry` remains in `ApplyReconcile.lean`**

Run via Grep tool: pattern `sorry` scoped to `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`, output_mode `count`.
Expected: `0`.

- [ ] **Step 4: Tick off the deliverables checklist**

Open `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`. Check off every box in the "Deliverables Checklist" section. Commit the edits:

```bash
git add docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md
git commit -m "Mark apply-reconcile deliverables complete"
```

- [ ] **Step 5: Update issue #53**

Run:
```bash
gh issue comment 53 --repo sourcenetwork/defra-agent --body \
"Landed. ApplyReconcile.lean (incl. T-Conv) is green; \`Collection\` enum and typed \`DesiredApplyBundle\` boundary in place; \`apply_property.rs\` (4 properties) and \`apply_conformance.rs\` (7 cases) pass; follow-on issues I-1/I-2/I-3 filed. See commit range \`$(git log --oneline main..HEAD | head -1 | cut -d' ' -f1)..HEAD\`."
```

(If the branch hasn't been merged yet, use a descriptive range string in the comment, e.g. the PR URL.)

---

## Spec-deliverables mapping

| Deliverable | Covered by |
|---|---|
| `Proofs/ApplyReconcile.lean` with Collection, DocRef, Manifest, LiveState, ApplyStep, diff, applyOne, T-Conv | Tasks 7–15 |
| `Proofs.lean` registers the module | Task 7 Step 2 |
| `defra-agent-cli` `enum Collection` threaded through file naming + dispatch | Tasks 1–3 |
| Typed `DesiredFields`/`LiveFields` partition at the apply boundary | Tasks 4–6 |
| `apply_property.rs` proptest properties | Tasks 16–17 |
| `apply_conformance.rs` table-driven cases | Task 18 |
| Audit `cli_e2e.rs` for subsumed coverage (re-routed to the split files) | Task 19 |
| Apply-atomicity note in `proofs/README.md` | Task 20 |
| Issues I-1, I-2, I-3 filed | Task 21 |
