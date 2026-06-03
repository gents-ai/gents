# Skills Declarative Core (Skill collection + apply path) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add a `Skill` DefraDB collection (+ `AgentBehavior.skill_refs`/`skill_excludes`) end-to-end through the config desired-state/apply path, keeping the Lean apply-reconcile fence green, with apply-time validation for skill references and tool refs.

**Architecture:** `Skill` is a new operator-controlled collection at apply-order **rank 0** (referenced by `AgentBehavior`; references `ToolServiceRegistry`). The apply path is strictly Lean-fenced (`config_import.rs:884` requires `CONFIG_APPLY_ORDER` to equal Lean's `productionWriteOrder` projection *and* Lean to cover every collection), so the Lean model and the Rust `Collection` enum change together. `Skill` rides the **generic** apply writer (like `AgentBehavior`/`ToolSelection`), not the custom writer.

**As-built rank note:** `Skill` is rank **0**, NOT rank 1. `AgentBehavior.skill_refs → Skill`, so the skill must be written before the behavior; within a single apply-order rank the Lean model sorts `DocRef`s by id, and `"behavior-a" < "skill-a"` would place the behavior write first and break referrer-closure. Putting `Skill` at rank 0 (the same rank as `ToolSelection`, which is likewise referenced by `AgentBehavior`) keeps it strictly before rank-1 `AgentBehavior`; `Skill → ToolServiceRegistry` "service-a" < "skill-a" stays closed within rank 0.

**Tech Stack:** Rust (workspace), Lean 4 + mathlib (apply-reconcile model), DefraDB GraphQL schemas.

**Sequence context:** Plan 2 of 5 for the skills spec (`docs/superpowers/specs/2026-06-02-skills-integration-design.md`); follows the committed Lean privilege algebra (`Proofs/Skills.lean`). Later plans: prompt+tool-surface composition, Codex shim wiring, migration CLI.

**Key decisions:**
- Apply-order **rank 0** (see as-built note above), list position immediately before `AgentBehavior`: `… ToolServiceRegistry, ToolSelection, Skill, AgentBehavior, Task, …`.
- **v1 schema** mirrors `AgentBehavior` (generic writer, `created_at: String`, no `@branchable`/`updated_at`) to avoid custom-writer/timestamp machinery. Versioning (`@branchable`/`DateTime`) deferred.
- `Skill` is NOT in `uses_custom_apply_writer` and NOT in the special-sanitize list; it has no runtime-owned fields (`runtime_owned_fields(Skill) = &[]`).

---

### Task 1: Lean — add `Skill` to the apply-reconcile model

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile/Collections.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases/Fixtures.lean`

- [ ] **Step 1:** In `Collections.lean`, add `| skill` to the `Collection` inductive (after `agentBehavior`), add `| .skill => 1` to `Collection.applyOrder`, and update any exhaustive `match`/`example`/parity proof that enumerates all variants (the proof bodies using `cases c <;> rfl` auto-extend; explicit enumerations need the new arm).
- [ ] **Step 2:** In `ContractCases/Types.lean`, add `| .skill => "Skill"` to `collectionName`, `| .skill => "skill_id"` to `collectionUniqueField`, and insert `.skill` into `productionWriteOrder` between `.agentBehavior` and `.task`. **Important:** `Skill` must come *before* `agentBehavior` in dependency terms, but the existing list has `agentBehavior` before `task`; since `AgentBehavior` references `Skill`, place `.skill` BEFORE `.agentBehavior` in `productionWriteOrder` (i.e. `…, .toolSelection, .skill, .agentBehavior, .task, …`).
- [ ] **Step 3:** In `ContractCases/Fixtures.lean`, add `def skillA : DocRef := doc .skill "skill-a"` near the other DocRefs; in the `production_write_boundary_all_collections` manifest add `desired .skill "skill-a" "skill-desired" [serviceA]` (skill → ToolServiceRegistry) immediately before the `agentBehavior` entry, and add `skillA` to the `agentBehavior` ref list. Adjust `prefixLen` if the build/fence indicates the desired-closure boundary shifted (start by leaving it; fix if Step 4/Task 9 fails).
- [ ] **Step 4:** Build the proofs.

Run: `cd crates/defra-agent/proofs && lake build`
Expected: full build succeeds, zero errors. If a `productionWriteOrder` / `applyOrder` parity proof fails, reconcile the rank (`skill => 1`) and list position until green.

- [ ] **Step 5:** Commit.

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile
git commit -m "Add Skill collection to Lean apply-reconcile model (#340)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: New `Skill` GraphQL schema + registration

**Files:**
- Create: `crates/defra-agent-protocol/schemas/agent/skill.graphql`
- Modify: `crates/defra-agent-protocol/src/schemas.rs`

- [ ] **Step 1:** Create `skill.graphql`:

```graphql
type Skill {
    skill_id: String @index(unique: true)
    agent_did: String @index
    scope: String @index
    name: String @index
    description: String
    instructions: String
    tool_refs: [String!]
    display_name: String
    interface_json: String
    enabled: Boolean @index
    created_at: String
}
```

- [ ] **Step 2:** In `schemas.rs`, add `SKILL_NAME`/`SKILL` constants (`include_str!("../schemas/agent/skill.graphql")`) and insert into the `ALL` + `ALL_COLLECTION_NAMES` arrays in apply-order position (after ToolSelection, before AgentBehavior — match the GraphQL registration ordering already used).
- [ ] **Step 3:** `cargo check -p defra-agent-protocol` → Expected: clean.

---

### Task 3: `Collection::Skill` variant + methods

**Files:**
- Modify: `crates/defra-agent/src/collection.rs`

- [ ] **Step 1:** Add `Skill` to the enum, to `ALL` (bump `[Collection; 9]`→`[10]`), and arms to `graphql_type` (`"Skill"`), `unique_field` (`"skill_id"`), `apply_order` (group with `AgentBehavior` ⇒ `1`), `dir_name` (`Some("skills")`), and `Display` (`"skills"`). Update any in-file test enumerating canonical variants/ranks.
- [ ] **Step 2:** `cargo check -p defra-agent` → Expected: clean (exhaustiveness errors will name every match that needs the new arm — fix each).

---

### Task 4: `DesiredSkill` + manifest/diff/counts plumbing

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/convert.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/diff.rs`

- [ ] **Step 1:** Add `DesiredSkill { skill_id, agent_did, scope, name, description, instructions, tool_refs: Vec<String>, display_name, interface_json, enabled }` (mirror `DesiredToolSelection`'s derive/serde attrs). Add `skills: Vec<DesiredSkill>` to `DesiredStateManifest`. Add `skills` to `DesiredStateDiffCollections`, `DesiredStateDiffCollectionsCounts`, `DesiredStateCounts` (+ their `get`/`counts`/`empty` methods). Add `DesiredFields`/`HasUniqueId` impls for `DesiredSkill`.
- [ ] **Step 2:** Add `AgentBehavior` skill fields: `#[serde(default)] skill_refs: Vec<String>` and `#[serde(default)] skill_excludes: Vec<String>` on `DesiredAgentBehavior`.
- [ ] **Step 3:** In `convert.rs`, add the `skills` arms to both `manifest_from_export_bundle` (field list: all Skill fields) and `export_bundle_from_manifest`. Add `skill_refs`/`skill_excludes` to the AgentBehavior field list in `manifest_from_export_bundle`.
- [ ] **Step 4:** In `diff.rs`, add `skills: diff_manifest_collection(&desired.skills, &live.skills)`.
- [ ] **Step 5:** `cargo check -p defra-agent-cli` → Expected: clean (fix exhaustiveness as flagged).

---

### Task 5: `ConfigExportBundle` + `ConfigApplyCounts` + bundle building

**Files:**
- Modify: `crates/defra-agent-cli/src/shared.rs`
- Modify: `crates/defra-agent-cli/src/config_bundle.rs`
- Modify: `crates/defra-agent-cli/src/main.rs`

- [ ] **Step 1:** In `shared.rs`: add `#[serde(default)] skills: Vec<Value>` to `ConfigExportBundle`; add `Collection::Skill => Some(&self.skills)` to `docs_for_collection`; add `skills: usize` to `ConfigApplyCounts` and handle in `set`/`changed`.
- [ ] **Step 2:** In `main.rs`: add `EXPORT_SKILL_FIELDS` (`"skill_id agent_did scope name description instructions tool_refs display_name interface_json enabled created_at"`); extend `EXPORT_AGENT_BEHAVIOR_FIELDS` with `skill_refs skill_excludes`.
- [ ] **Step 3:** In `config_bundle.rs`: fetch `Skill` rows in `build_config_export_bundle` + `build_desired_state_live_bundle` (sort by `skill_id`); add `skills` to both bundle returns and to the empty `live_manifest_from_bundle` manifest. `Skill` needs NO special `sanitize_import_document` arm (no runtime-owned fields) — confirm it falls through the default path like `AgentBehavior`/`ToolSelection`.
- [ ] **Step 4:** `cargo check -p defra-agent-cli` → Expected: clean.

---

### Task 6: Manifest load/write

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/write.rs`

- [ ] **Step 1:** In `load.rs`: `load_per_doc_collection(root, Collection::Skill, …)`; add to counts + manifest construction.
- [ ] **Step 2:** In `write.rs`: `validate_vec(&manifest.skills, "skill_id")`; `write_per_doc_collection(root, Collection::Skill, &manifest.skills, …)`.
- [ ] **Step 3:** `cargo check -p defra-agent-cli` → Expected: clean.

---

### Task 7: Apply-time validation

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/validate.rs`

- [ ] **Step 1:** In `validate_manifest` (static): collect `skill_ids` (error on empty/duplicate `skill_id`); validate `scope ∈ {"principal","behavior"}`; validate each skill's `agent_did` matches the principal (mirror the behavior ownership check). In the AgentBehavior loop, validate each `skill_refs`/`skill_excludes` entry resolves to a known `skill_id` (mirror the `backend_id`/`tool_selection_id` resolution pattern). A `skill_ref` to a skill whose `agent_did` differs from the behavior's principal is an error (D6).
- [ ] **Step 2:** `tool_refs` validation: a `tool_ref` not matching a known host-tool kind / registered mcp service id / cli name is **not** a hard error (D3 degrade) — collect into a separate non-fatal list if a warning channel exists, otherwise skip (only hard structural errors block apply). Document the choice inline.
- [ ] **Step 3:** `cargo check -p defra-agent-cli` → Expected: clean.

---

### Task 8: Build the whole workspace + add a Skill apply unit test

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs` (`CONFIG_APPLY_ORDER` + `runtime_owned_fields` test helper)

- [ ] **Step 1:** Insert `Collection::Skill` into `CONFIG_APPLY_ORDER` between `Collection::ToolSelection` and `Collection::AgentBehavior`. Add `Collection::Skill => &[]` to the `runtime_owned_fields` test helper if it has an exhaustive match.
- [ ] **Step 2:** `cargo check -p defra-agent-cli --tests` → Expected: clean.

---

### Task 9: Verify the Lean fence + apply-order tests pass

- [ ] **Step 1:** Run the apply-order + fence tests:

Run: `cargo test -p defra-agent-cli config_apply_order_contains_each_collection_once config_apply_order_has_retry_safe_prefixes 2>&1 | tail -20`
Expected: pass (now 10 collections).

- [ ] **Step 2:** Run the Lean write-boundary fence:

Run: `cargo test -p defra-agent-cli generated_apply_reconcile_cases_fence_production_apply_write_boundary 2>&1 | tail -40`
Expected: pass. If it fails on write-order mismatch, reconcile `CONFIG_APPLY_ORDER` ↔ Lean `productionWriteOrder`. If it fails on referrer-closure/prefix, adjust the Fixtures `prefixLen` / ref lists (Task 1 Step 3) and rebuild Lean.

- [ ] **Step 3:** Commit.

```bash
git add -A
git commit -m "Add Skill collection to config apply path + validation (#340)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:** Implements the D1 `Skill` schema (v1-simplified), D5 `skill_refs`/`skill_excludes` on `AgentBehavior`, and the D4 commitment to extend the apply ordering in Lean. Activation/composition (D2) and the runtime `document_config` loader + effective-set resolver are the NEXT plan (they consume this collection but don't gate the apply fence). D3 tool-ref degrade is reflected in Task 7 Step 2 (non-fatal tool_refs).

**Placeholder scan:** File-by-file edit specs are concrete; the v1 schema and the apply-order position are fully specified. Where exact current struct/field text must match, the executing agent reads the file immediately before editing (the map's line ranges are in the spec/exploration). The two highest-risk spots (Lean fixture `prefixLen`, `CONFIG_APPLY_ORDER` ↔ `productionWriteOrder` parity) have explicit reconcile instructions in Task 9.

**Type consistency:** `DesiredSkill` field names match the `skill.graphql` fields and `EXPORT_SKILL_FIELDS`. `Collection::Skill` `graphql_type`="Skill"/`unique_field`="skill_id" match Lean `collectionName`/`collectionUniqueField`. Apply-order rank 1 matches Lean `applyOrder .skill => 1`. List position (after ToolSelection, before AgentBehavior) is identical in Rust `CONFIG_APPLY_ORDER` and Lean `productionWriteOrder`.
