# #1331 One Validator Per Config Document Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every config document has exactly one validator, in `document_config`, and every write door (CLI desired state, the self-config tool, the imperative CLI commands, the codex shim) calls it. `AgentBehavior` writes go through the persona admission core.

**Architecture:** `document_config::{InferenceProfile, InferenceBackend, ToolSelectionDocument, AgentBehavior}` each expose `validate(&self, refs: &ConfigReferences) -> Result<()>` where `ConfigReferences` carries the id sets needed for referential checks (backend ids with their advertised models, tool selection ids, profile ids, skill ids). Desired state decodes into the document types and calls those validators instead of mirroring rules; `self_config` requests call them in their `validate` closures; `gents config agent-behavior set` and the codex shim model switch submit a persona request through `persona_ops` instead of writing raw.

**Tech Stack:** Rust (gents, gents-cli).

**Spec:** GitHub issue #1331.

## Global Constraints

- The union of today's rules survives: profile bounds (`stream_liveness_timeout_secs > 0`, `deadline_duration_secs > 0`, liveness < deadline, `seed >= 0`, `reasoning_effort` enum), backend (`backend_id`/`endpoint` non-empty, `api_key` non-empty-if-present, `api_key` xor `api_key_env_var`, `provider_kind` parses, `max_concurrent`/`max_queue_depth` positive, no-lockout on `models` dropping the current model), tool selection (`ToolSelectionDocument::validate` as is), behavior references (backend, model advertised by the backend, tool selection, profile, skill refs/excludes exist).
- Error messages from desired state stay as they are today where tests assert them; where a message differs between paths today, the `document_config` wording wins and desired-state tests are updated.
- `persona_ops` remains the only production writer of `AgentBehavior` except first-run `init`. `write_agent_behavior_document` becomes `pub(crate)` inside `gents` (no longer exported through `config_client` to the CLI).
- Net code deletion.

---

### Task 1: `document_config` validators (profile, backend, behavior refs)

**Files:**
- Modify: `crates/gents/src/document_config/inference_profile.rs` (add `InferenceProfile::validate(&self) -> Result<()>` with the profile rules; `upsert_inference_profile` calls it)
- Modify: `crates/gents/src/document_config/backend.rs` (or wherever `InferenceBackend` document lives; add `validate(&self, current_model: Option<&str>) -> Result<()>` with the backend rules incl. no-lockout)
- Modify: `crates/gents/src/document_config/behavior.rs` (add `AgentBehavior::validate_references(&self, refs: &ConfigReferences) -> Result<()>`)
- Create: `crates/gents/src/document_config/references.rs` (`ConfigReferences { backends: BTreeMap<String, Vec<String> /*models*/>, tool_selections: BTreeSet<String>, profiles: BTreeSet<String>, skills: BTreeSet<String> }` + a loader from a node)
- Test: unit tests per validator, table-driven from the rule list above (copy the concrete cases from `crates/gents-cli/src/desired_state/validate/agent.rs` tests so no rule is lost).

- [ ] Tests first (copy cases), implement, `cargo test -p gents --lib document_config` green, commit — `runtime: document_config owns config document validation (#1331)`.

### Task 2: Desired state and self-config call the owners

**Files:**
- Modify: `crates/gents-cli/src/desired_state/validate/agent.rs` (`validate_profiles`, `validate_backends`, `validate_behaviors` become: decode to the document type, build `ConfigReferences` from the manifest, call the owner; delete the rule bodies), `validate/tooling.rs:159-330` (`validate_tool_selections`: decode to `ToolSelectionDocument`, call `validate()`; keep only manifest-shape checks that have no document equivalent, and say which)
- Modify: `crates/gents/src/self_config/mod.rs:157-300` (`behavior_request`, `profile_request`, `backend_request` validate closures call the owners; `profile_request` writes via `upsert_inference_profile` or calls `validate` explicitly)
- Test: `cargo test -p gents-cli --lib desired_state`, `cargo test -p gents --lib self_config`, `cargo test -p gents --test conformance self_config`

- [ ] Implement; green; grep gate: `grep -rn 'reasoning_effort\|stream_liveness_timeout_secs' crates/gents-cli/src/desired_state/validate` returns nothing but decode/plumbing; commit — `cli+runtime: every config write door calls the document validators (#1331)`.

### Task 3: Behavior writes go through persona admission

**Files:**
- Modify: `crates/gents-cli/src/commands/config/behavior.rs:24-67` (`behavior_set` builds a `PersonaRequestDoc` edit op and calls `submit_local_persona` like `behavior_create/clone/disable` in the same file)
- Modify: `crates/gents-cli/src/commands/codex_shim/handlers/models.rs:186-200` (`apply_model_to_bound_behavior` submits a persona edit for `backend_id`/`model_name`)
- Modify: `crates/gents/src/config_client/mod.rs` (stop re-exporting `write_agent_behavior_document`; `init.rs` keeps access through a narrowly named bootstrap function if it must write before a catalog exists, documented as the one exception)
- Test: `cargo test -p gents-cli --test suites cli_config` (behavior set tests), `cargo test -p gents --test conformance persona_request`; a new CLI test that `behavior set --tool-selection-id missing` is rejected.

- [ ] Implement; green; commit — `cli: behavior set and codex model switch go through persona admission (#1331)`.

### Task 4: Apply-order note
- [ ] `crates/gents/src/collection.rs:145` vs `:35`: do not change the Lean rank here; add a doc comment on `DESIRED_STATE_APPLY_ORDER` stating it is a linear refinement of `Collection::apply_order()` and add a unit test asserting the linear order is consistent with the rank (never places a higher-rank collection before a lower one). Commit — `test: linear apply order refines the Lean rank (#1331)`.

### Task 5: Gate
- [ ] `cargo test -p gents`, `cargo test -p gents-cli`, `cargo check --workspace --all-targets`, `cargo fmt --all --check`; net deletion check; CHANGELOG `### Fixed`: "Config documents are validated by one owner regardless of write path; `gents config agent-behavior set` now rejects unknown backends, models, tool selections and profiles."
