# Cache-safe role-aware prompt templating — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a behavior declare role-tagged prompt templates so the cacheable system prefix stays byte-stable across requests while per-request dynamic context (acting seat/DID, live collection summary, time) renders into a `<context>`-tagged user message — proven cache-safe in Lean, fenced by conformance, captured for training fidelity.

**Architecture:** Foundation-flow: a new Lean sub-model `PromptAssembly/Template` proves the cache-stability property (a system template that reads only run-constant variables renders identically across requests) and a `contextPreamble` slot ordering; a conformance mirror fences it; then Rust builds a runtime-owned variable catalog, a parser-backed complete-or-reject reads-collector that backs the cache-safety guard, renders `system_prompt` once at startup into the frozen preamble, renders a new `request_context_template` per request into a durably-persisted context message, and merges the catalog into the existing MiniJinja task templating for interop.

**Tech Stack:** Lean 4 + Mathlib (proofs), Rust (`defra-agent`, `defra-agent-cli`, `defra-agent-schemas`), MiniJinja 2.x (`unstable_machinery` for AST parsing), DefraDB GraphQL.

**Spec:** `docs/superpowers/specs/2026-06-15-prompt-templating-design.md`

**Gate (run after each phase; full gate at the end):**
```
cd crates/defra-agent/proofs && lake build      # zero sorry
cargo test -p defra-agent
cargo test -p defra-agent-cli
```

**Review calibration (per project convention):** this is a >10-task plan; skip per-task code-quality reviewers, keep spec-compliance checks, and do one final whole-branch review (Task 15).

---

## File Structure

**Create:**
- `crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean` — the cache-stability sub-model.
- `crates/defra-agent/tests/conformance/prompt_template.rs` — conformance mirror.
- `crates/defra-agent/src/template/catalog.rs` — runtime-owned variable catalog (volatility + availability).
- `crates/defra-agent/src/template/reads.rs` — parser-backed complete-or-reject reads-collector + system-template guard.

**Modify:**
- `crates/defra-agent/proofs/Proofs/PromptAssembly.lean` — import the new sub-model.
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` — ledger entry for the new guarantee.
- `crates/defra-agent/tests/conformance.rs` — wire the new conformance module.
- `Cargo.toml` (workspace) — add `unstable_machinery` to the minijinja feature list.
- `crates/defra-agent/src/template/mod.rs` — `TemplateScope` gains `node`/`ctx`; expose new submodules; render helper.
- `crates/defra-agent-schemas/schemas/agent/agent_behavior.graphql` — `request_context_template` field.
- `crates/defra-agent/src/document_config/behavior.rs` — struct + queries + writers + default.
- `crates/defra-agent/src/config.rs` — `AgentBehavior` runtime config field.
- `crates/defra-agent/src/agent/document_view/snapshot.rs` (and `runtime_snapshot.rs` if it carries behavior) — carry the field into the resolved behavior.
- `crates/defra-agent-cli/src/desired_state/mod.rs` — accept the new behavior field.
- `crates/defra-agent-cli/src/main.rs` — export allowlist.
- `crates/defra-agent/src/prompt.rs` — render `system_prompt` as run-constant template into the frozen preamble.
- `crates/defra-agent/src/agent/loop_stream.rs` — render + inject the per-request context message.
- `crates/defra-agent/src/hook/persistence/message_spawn.rs` + `prompt_hook.rs` — persist the context message with sequence before the prompt.
- `crates/defra-agent/src/trigger_engine/mod.rs` — merge `node`/`ctx` into task `TemplateScope`.

---

## Phase A — Lean cache-stability model

### Task 1: The `Template` sub-model and the cache-stability theorem

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PromptAssembly.lean`

- [ ] **Step 1: Write the sub-model with the core theorems**

Create `crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean`:

```lean
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Proofs.PromptAssembly.Executable

/-!
# PromptAssembly.Template — cache-safe role-aware templating (issue #497)

The dynamic-context counterpart to the provider-input sanitizer. A behavior's
*system* template must stay byte-stable across requests so the provider prefix
cache is not invalidated; only its *per-request* context may vary.

The model abstracts over the rendering engine (MiniJinja): a template is
characterised by the set of variable references it reads, and `render` depends
only on the binding restricted to those reads (engine purity / strict-undefined
evaluation). The cache-safety guarantee is therefore a property of *which
variables a template reads*, not of engine expressiveness.

Rust conformance: `crates/defra-agent/tests/conformance/prompt_template.rs`.
-/

namespace PromptAssembly.Template

/-- Volatility class of a catalog variable. -/
inductive Volatility where
  | static
  | runConstant
  | perRequest
  deriving DecidableEq, Repr

/-- A catalog variable reference (full dotted key, e.g. `"node.node_did"`). -/
abbrev VarRef := String

/-- The runtime-owned catalog: maps a full ref to its volatility. An unknown
ref (not in the catalog) maps to `none` and is therefore never run-constant. -/
abbrev Catalog := VarRef → Option Volatility

/-- A binding assigns each variable a rendered value. -/
abbrev Binding := VarRef → String

/-- A template, abstracted by the complete set of variable refs it reads. -/
structure Template where
  reads : Finset VarRef
  deriving DecidableEq

/-- Render normal form: exactly the (ref, value) pairs the template reads.
Models a pure engine — output is a function of the read variables alone. -/
def render (t : Template) (b : Binding) : Finset (VarRef × String) :=
  t.reads.image (fun v => (v, b v))

/-- Engine purity: agreement on the read set ⇒ identical render. -/
theorem render_determined (t : Template) (b1 b2 : Binding)
    (h : ∀ v ∈ t.reads, b1 v = b2 v) :
    render t b1 = render t b2 := by
  unfold render
  apply Finset.image_congr
  intro v hv
  simp [h v hv]

/-- A system template is well-formed when every variable it reads is
run-constant per the catalog. (Static literal text contributes no reads.) -/
def WellFormedSystem (cat : Catalog) (t : Template) : Prop :=
  ∀ v ∈ t.reads, cat v = some .runConstant

/-- Two bindings agree on all run-constant variables — the condition that
holds across two requests in the same run (run-constants are frozen at start). -/
def AgreeRunConstant (cat : Catalog) (b1 b2 : Binding) : Prop :=
  ∀ v, cat v = some .runConstant → b1 v = b2 v

/-- **Cache stability.** A well-formed system template renders identically
across any two requests whose bindings agree on run-constant values — i.e. the
cacheable system prefix is byte-stable regardless of per-request context. -/
theorem system_render_stable (cat : Catalog) (t : Template) (b1 b2 : Binding)
    (wf : WellFormedSystem cat t) (agree : AgreeRunConstant cat b1 b2) :
    render t b1 = render t b2 := by
  apply render_determined
  intro v hv
  exact agree v (wf v hv)

/-- Decidable mirror of the cache-safety guard. -/
def validateSystem (cat : Catalog) (t : Template) : Bool :=
  t.reads.all (fun v => decide (cat v = some .runConstant))

/-- The guard is sound and complete w.r.t. `WellFormedSystem`. -/
theorem validateSystem_correct (cat : Catalog) (t : Template) :
    validateSystem cat t = true ↔ WellFormedSystem cat t := by
  unfold validateSystem WellFormedSystem
  rw [Finset.all_iff_forall]
  constructor
  · intro h v hv; simpa using (h v hv)
  · intro h v hv; simpa using (h v hv)

end PromptAssembly.Template
```

- [ ] **Step 2: Wire the import into the barrel**

In `crates/defra-agent/proofs/Proofs/PromptAssembly.lean`, add after the existing imports:

```lean
import Proofs.PromptAssembly.Template
```

- [ ] **Step 3: Build the proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: builds clean, zero `sorry`. (If `Finset.all_iff_forall` resolves to a slightly different name in the pinned Mathlib, use `simp [Finset.all_iff_forall]` or replace the `validateSystem_correct` proof body with `by simp [validateSystem, WellFormedSystem, Finset.all_iff_forall, decide_eq_true_iff]`.)

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean crates/defra-agent/proofs/Proofs/PromptAssembly.lean
git commit -m "feat(proofs): PromptAssembly.Template cache-stability model (#497)"
```

---

### Task 2: The `contextPreamble` slot and its assembly ordering

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/PromptAssembly/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean`

- [ ] **Step 1: Add the slot variant**

In `crates/defra-agent/proofs/Proofs/PromptAssembly/Executable.lean`, extend the `Slot` inductive (currently `preamble | summaryReminder | skillReminder | conversation | prompt`) with a `contextPreamble` case:

```lean
inductive Slot where
  | preamble
  | summaryReminder
  | skillReminder (index : Nat)
  | conversation (index : Nat)
  | contextPreamble
  | prompt
  deriving DecidableEq, Repr
```

The existing `assemble`/`perTurnRequest`/`buildLayers`/`injectSkills` definitions and the `assemble_spec`/`assemble_head`/`assemble_last` theorems are unchanged — they describe the no-context request and still hold by `rfl`/their existing proofs.

- [ ] **Step 2: Add the context-bearing assembly + its order spec**

Append to `crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean`, before `end PromptAssembly.Template`:

```lean
open PromptAssembly (Slot)

/-- Per-request assembly when a `request_context_template` is present: the
rendered context rides immediately before the new prompt, after the
conversation. Mirrors `loop_stream::build_request` injecting the context
message ahead of the prompt. -/
def assembleWithContext (skillCount summaryCount conversationLen : Nat) : List Slot :=
  Slot.preamble ::
    ((List.range skillCount).map Slot.skillReminder ++
      ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
        (List.range conversationLen).map Slot.conversation)) ++
    [Slot.contextPreamble, Slot.prompt]

/-- The context slot precedes the prompt and follows the conversation. Any
reordering in `loop_stream.rs` breaks this `rfl`. -/
theorem assembleWithContext_spec (skillCount summaryCount conversationLen : Nat) :
    assembleWithContext skillCount summaryCount conversationLen =
      Slot.preamble ::
        ((List.range skillCount).map Slot.skillReminder ++
          ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
            (List.range conversationLen).map Slot.conversation)) ++
        [Slot.contextPreamble, Slot.prompt] := rfl

/-- Context immediately precedes the prompt (the last two slots). -/
theorem assembleWithContext_context_before_prompt
    (skillCount summaryCount conversationLen : Nat) :
    (assembleWithContext skillCount summaryCount conversationLen).getLast? = some Slot.prompt := by
  rw [assembleWithContext]
  simp [List.getLast?_concat]
```

- [ ] **Step 3: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero sorry. (If `getLast?_concat` simp form differs, prove via `rw [← List.cons_append, ← List.cons_append, List.getLast?_concat]` as in the existing `assemble_last`.)

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/PromptAssembly/Executable.lean crates/defra-agent/proofs/Proofs/PromptAssembly/Template.lean
git commit -m "feat(proofs): contextPreamble slot before prompt (#497)"
```

---

## Phase B — Conformance fence

### Task 3: CoverageLedger entry

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`

- [ ] **Step 1: Add the ledger entry**

In `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`, the `followUpHookCoverage` list ends with the existing `PromptAssembly.providerInput.sanitizeLoadedHistory` boundary entry (around line 744). Add a sibling entry in the same list (before the closing `]`) using the same `tagged (followUpCoverage …) domain surfaces` shape as the neighbouring `follow_up_hook` entries:

```lean
  , tagged (followUpCoverage
      "follow_up_hook"
      "PromptAssembly.Template.system_render_stable"
      "system_render_stable proves a well-formed system template renders identically across requests that agree on run-constant values — the cacheable prefix is byte-stable. validateSystem_correct ties the apply-time guard to well-formedness. Fenced by tests/conformance/prompt_template.rs.")
      "compaction" []
```

- [ ] **Step 2: Build the ledger**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean. (If `followUpCoverage`/`tagged` arity differs from this sketch, open the file and match the exact constructor used by the entry immediately above; the three string args are domain-tag, Lean symbol, rationale.)

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
git commit -m "test(proofs): ledger entry for Template.system_render_stable (#497)"
```

---

### Task 4: Conformance mirror module

**Files:**
- Create: `crates/defra-agent/tests/conformance/prompt_template.rs`
- Modify: `crates/defra-agent/tests/conformance.rs`

> Note: `Template` is a submodule of the `PromptAssembly` barrel, so the
> structure fence (`tests/conformance/structure.rs`) is already satisfied by the
> existing `PromptAssembly → conformance/prompt_assembly.rs` home — no
> `model_homes()` change is needed. We add a sibling module for clarity.

This task depends on the Rust catalog + guard API (Tasks 6–8). **Order:** implement Tasks 6–8 first, then return here. The test imports the public API created there: `defra_agent::template::catalog::{Volatility, Catalog}` and `defra_agent::template::reads::{collect_system_reads, validate_system_template}`.

- [ ] **Step 1: Write the conformance module**

Create `crates/defra-agent/tests/conformance/prompt_template.rs`:

```rust
//! PromptAssembly.Template conformance (issue #497).
//!
//! Mirrors the Lean cache-stability sub-model against the Rust guard +
//! renderer. Each test names the theorem it fences.
//!
//! - system_render_stable: a well-formed system template (reads ⊆ run-constant)
//!   renders identically under two different per-request bindings.
//! - validateSystem_correct: the apply-time guard accepts iff well-formed.
//! - The guard is complete-or-reject: unanalyzable constructs and unknown
//!   refs are rejected; `{% raw %}` bodies are not reads.

use defra_agent::template::catalog::{default_catalog, Volatility};
use defra_agent::template::reads::{collect_system_reads, validate_system_template};
use defra_agent::template::{render_template, TemplateScope};

/// node.* binding (run-constant) for two requests; the ctx values differ.
fn scope(now: &str, seat: &str) -> TemplateScope {
    TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
        node: serde_json::json!({ "node_did": "did:key:zNODE", "behavior_id": "policy_agent" }),
        ctx: serde_json::json!({ "now": now, "acting_seat": seat, "acting_did": "did:key:zSEAT" }),
    }
}

#[test]
fn system_render_stable_under_per_request_change() {
    // A system template that reads only run-constant node.* vars.
    let tmpl = "You are {{ node.behavior_id }} on {{ node.node_did }}.";
    let cat = default_catalog();
    validate_system_template(tmpl, &cat).expect("well-formed system template");

    let r1 = render_template(tmpl, &scope("2026-06-15T00:00:00Z", "seat-A")).unwrap();
    let r2 = render_template(tmpl, &scope("2030-01-01T12:00:00Z", "seat-B")).unwrap();
    assert_eq!(r1, r2, "system render must be byte-stable across requests");
}

#[test]
fn validate_rejects_per_request_ref_in_system_template() {
    let cat = default_catalog();
    let err = validate_system_template("Now: {{ ctx.now }}", &cat).unwrap_err();
    assert!(
        format!("{err}").contains("ctx.now"),
        "error must name the offending per-request var, got: {err}"
    );
}

#[test]
fn validate_rejects_unanalyzable_construct_in_system_template() {
    // Control flow rebinds names; the guard cannot prove the read set, so it
    // must reject (fail-closed) rather than silently accept.
    let cat = default_catalog();
    assert!(
        validate_system_template("{% for x in node.list %}{{ x }}{% endfor %}", &cat).is_err(),
        "system template with control flow must be rejected"
    );
}

#[test]
fn validate_accepts_per_request_ref_inside_raw_block() {
    // The documented escape hatch: braces inside {% raw %} are literal text,
    // not reads, so a per-request-looking token there is safe.
    let cat = default_catalog();
    validate_system_template("Literal: {% raw %}{{ ctx.now }}{% endraw %}", &cat)
        .expect("raw block contents are not reads");
}

#[test]
fn validate_rejects_unknown_namespace_path() {
    let cat = default_catalog();
    assert!(
        validate_system_template("{{ ctx.bogus_unknown }}", &cat).is_err(),
        "unknown ctx.* path must reject, never default to run-constant"
    );
    assert!(
        validate_system_template("{{ node.bogus_unknown }}", &cat).is_err(),
        "unknown node.* path must reject"
    );
}

#[test]
fn collect_system_reads_returns_full_refs() {
    let reads = collect_system_reads("{{ node.node_did }} {{ node.behavior_id }}").unwrap();
    assert!(reads.contains("node.node_did"));
    assert!(reads.contains("node.behavior_id"));
}

#[test]
fn catalog_volatility_matches_model() {
    let cat = default_catalog();
    assert_eq!(cat.volatility("node.node_did"), Some(Volatility::RunConstant));
    assert_eq!(cat.volatility("ctx.now"), Some(Volatility::PerRequest));
    assert_eq!(cat.volatility("ctx.unknown"), None);
}
```

- [ ] **Step 2: Wire the module**

In `crates/defra-agent/tests/conformance.rs`, find the alphabetical block of `#[path = "conformance/…"]` module declarations (e.g. the `prompt_assembly` entry) and add:

```rust
#[path = "conformance/prompt_template.rs"]
mod prompt_template;
```

(Match the surrounding `#[path]` + `mod name;` two-line pattern exactly.)

- [ ] **Step 3: Verify it fails to compile (API not yet built)**

Run: `cargo test -p defra-agent --test conformance prompt_template 2>&1 | head -30`
Expected: compile errors referencing `template::catalog` / `template::reads` — confirms the fence is driving the API. (Proceed to Phase C; return after Task 8 to make it pass.)

- [ ] **Step 4: Commit (compiling-deferred)**

```bash
git add crates/defra-agent/tests/conformance/prompt_template.rs crates/defra-agent/tests/conformance.rs
git commit -m "test(conformance): prompt_template fence for cache-safety guard (#497)"
```

---

## Phase C — Rust template engine: catalog, reads-collector, guard

### Task 5: Enable the MiniJinja AST parser

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add the feature**

In the workspace root `Cargo.toml`, change the minijinja line (line ~65):

```toml
minijinja = { version = "2", default-features = false, features = ["builtins", "serde", "unstable_machinery"] }
```

- [ ] **Step 2: Verify the AST module is reachable**

Run: `cargo build -p defra-agent 2>&1 | tail -5`
Expected: builds (the feature unlocks `minijinja::machinery`). If the build warns the feature is unknown, confirm the exact feature name with `cargo metadata --format-version=1 | grep -o 'unstable_machinery'` against the locked 2.19 version.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: enable minijinja unstable_machinery for AST reads-collection (#497)"
```

---

### Task 6: The variable catalog

**Files:**
- Create: `crates/defra-agent/src/template/catalog.rs`
- Modify: `crates/defra-agent/src/template/mod.rs`

- [ ] **Step 1: Write the catalog**

Create `crates/defra-agent/src/template/catalog.rs`:

```rust
//! Runtime-owned variable catalog for prompt templating (#497).
//!
//! The catalog is the single source of truth for *what varies per request*.
//! Behaviors reference variable names; they cannot declare volatility, so the
//! cache-safety guard checks every system-template ref against this table.
//!
//! Two orthogonal axes:
//! - `Volatility` drives the cache-safety guard (a system template may read
//!   only `RunConstant` / `Static` vars).
//! - `Availability` drives a render-site scope check (a var is only legal
//!   where the runtime actually supplies it).

use std::collections::BTreeMap;

/// How often a variable's value may change. Mirrors Lean `Volatility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volatility {
    /// Filled once at runtime start, then frozen. Cache-safe in system prompts.
    RunConstant,
    /// Varies per request. Forbidden in system prompts.
    PerRequest,
}

/// Where a variable is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// The frozen system preamble (run-constant render at startup).
    System,
    /// The per-request context message in the owned loop.
    RequestContext,
    /// Task `prompt_template` rendered at trigger fire time.
    Task,
}

#[derive(Debug, Clone)]
struct Entry {
    volatility: Volatility,
    availability: &'static [Site],
}

/// The runtime catalog: full dotted ref → entry.
#[derive(Debug, Clone)]
pub struct Catalog {
    entries: BTreeMap<&'static str, Entry>,
}

impl Catalog {
    /// Volatility of a full ref, or `None` if the ref is unknown.
    pub fn volatility(&self, var: &str) -> Option<Volatility> {
        self.entries.get(var).map(|e| e.volatility)
    }

    /// Whether `var` is a known catalog ref available at `site`.
    pub fn is_available_at(&self, var: &str, site: Site) -> bool {
        self.entries
            .get(var)
            .is_some_and(|e| e.availability.contains(&site))
    }

    /// Whether `var` is a known catalog ref at all.
    pub fn is_known(&self, var: &str) -> bool {
        self.entries.contains_key(var)
    }
}

/// The v1 catalog (see spec §"The variable catalog (v1)").
pub fn default_catalog() -> Catalog {
    use Site::*;
    use Volatility::*;
    let mut entries = BTreeMap::new();
    let mut add = |key: &'static str, vol: Volatility, avail: &'static [Site]| {
        entries.insert(
            key,
            Entry {
                volatility: vol,
                availability: avail,
            },
        );
    };
    // node.* — run-constant, available everywhere.
    add("node.node_did", RunConstant, &[System, RequestContext, Task]);
    add("node.behavior_id", RunConstant, &[System, RequestContext, Task]);
    // ctx.* — per-request.
    add("ctx.now", PerRequest, &[RequestContext, Task]);
    add("ctx.acting_seat", PerRequest, &[RequestContext]);
    add("ctx.acting_did", PerRequest, &[RequestContext]);
    add("ctx.collection_summary", PerRequest, &[RequestContext]);
    Catalog { entries }
}
```

- [ ] **Step 2: Expose the submodule**

In `crates/defra-agent/src/template/mod.rs`, after the existing `#[cfg(test)] mod tests;` line add:

```rust
pub mod catalog;
pub mod reads;
```

(`reads` is created in Task 7; declaring it now is fine only after Task 7 exists — if building between tasks, add `pub mod reads;` in Task 7 instead. To keep each task compiling, add only `pub mod catalog;` here and add `pub mod reads;` in Task 7 Step 2.)

- [ ] **Step 3: Build + unit-check the catalog**

Run: `cargo build -p defra-agent 2>&1 | tail -5`
Expected: builds.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/template/catalog.rs crates/defra-agent/src/template/mod.rs
git commit -m "feat(template): runtime-owned variable catalog (#497)"
```

---

### Task 7: Parser-backed complete-or-reject reads-collector

**Files:**
- Create: `crates/defra-agent/src/template/reads.rs`
- Modify: `crates/defra-agent/src/template/mod.rs`

- [ ] **Step 1: Write the collector + guard**

Create `crates/defra-agent/src/template/reads.rs`:

```rust
//! Parser-backed reads-collector and cache-safety guard (#497).
//!
//! Backs the cache-safety guard for *system* templates. Unlike the best-effort
//! textual `parse_template_for_validation` (kept for trigger-scope checks),
//! this collector is **complete-or-reject**: a system template that uses any
//! construct whose read-set cannot be proven (control flow, name rebinding,
//! macros) is rejected, never silently accepted. `{% raw %}` blocks are
//! literal text and contribute no reads (the documented escape hatch).

use std::collections::BTreeSet;

use minijinja::machinery::{parse, Stmt, Expr};
use minijinja::syntax::SyntaxConfig;

use super::catalog::{Catalog, Volatility};
use super::TemplateError;

/// Collect the complete set of full variable refs a *system* template reads,
/// rejecting any construct that introduces bindings or control flow (which a
/// system template may not use). Returns dotted refs like `"node.node_did"`.
pub fn collect_system_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(
        template,
        "system_prompt",
        SyntaxConfig::default(),
        Default::default(),
    )
    .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_system(&ast, &mut reads)?;
    Ok(reads)
}

/// Validate a system template against the catalog: every read must be a known
/// run-constant ref. Rejects per-request refs, unknown refs, and unanalyzable
/// constructs. The error names the offending ref and the `{% raw %}` escape.
pub fn validate_system_template(template: &str, cat: &Catalog) -> Result<(), TemplateError> {
    let reads = collect_system_reads(template)?;
    for r in &reads {
        match cat.volatility(r) {
            Some(Volatility::RunConstant) => {}
            Some(Volatility::PerRequest) => {
                return Err(TemplateError::Render(format!(
                    "system template may not read per-request variable `{r}`; \
                     move it to request_context_template, or wrap literal braces \
                     in {{% raw %}}…{{% endraw %}}"
                )));
            }
            None => {
                return Err(TemplateError::Render(format!(
                    "system template references unknown variable `{r}`; \
                     wrap literal braces in {{% raw %}}…{{% endraw %}} if intended as text"
                )));
            }
        }
    }
    Ok(())
}

/// Walk a statement, allowing only the emit-only subset legal in a system
/// template: the template root, literal text (incl. `{% raw %}`), and `{{ }}`
/// expression emits. Anything else fails closed.
fn walk_stmt_system(stmt: &Stmt, reads: &mut BTreeSet<String>) -> Result<(), TemplateError> {
    match stmt {
        Stmt::Template(t) => {
            for child in &t.children {
                walk_stmt_system(child, reads)?;
            }
            Ok(())
        }
        // Literal text — includes `{% raw %}` bodies (emitted raw). No reads.
        Stmt::EmitRaw(_) => Ok(()),
        // `{{ expr }}` — collect refs from the expression.
        Stmt::EmitExpr(e) => collect_expr(&e.expr, reads),
        // Everything else (ForLoop, IfCond, WithBlock, Set, SetBlock, Macro,
        // CallBlock, FilterBlock, Block, AutoEscape, Do, …) rebinds names or
        // branches: the read-set is not provable here → fail closed.
        _ => Err(TemplateError::Render(
            "system template may only use literal text and {{ variable }} \
             substitutions (no control flow, loops, set, macros, or filter \
             blocks); wrap literal braces in {% raw %}…{% endraw %}"
                .to_string(),
        )),
    }
}

/// Collect dotted variable refs from an expression tree. Variable accesses are
/// `Var` roots extended by `GetAttr`; filter/test/function *names* are not
/// `Var` nodes, so they are correctly excluded. Args/operands are recursed.
fn collect_expr(expr: &Expr, reads: &mut BTreeSet<String>) -> Result<(), TemplateError> {
    if let Some(path) = dotted_path(expr) {
        reads.insert(path);
        return Ok(());
    }
    match expr {
        Expr::GetAttr(g) => collect_expr(&g.expr, reads),
        Expr::GetItem(g) => {
            collect_expr(&g.expr, reads)?;
            collect_expr(&g.subscript_expr, reads)
        }
        Expr::Filter(f) => {
            if let Some(e) = &f.expr {
                collect_expr(e, reads)?;
            }
            for a in &f.args {
                collect_expr(a, reads)?;
            }
            Ok(())
        }
        Expr::Test(t) => {
            collect_expr(&t.expr, reads)?;
            for a in &t.args {
                collect_expr(a, reads)?;
            }
            Ok(())
        }
        Expr::Call(c) => {
            collect_expr(&c.expr, reads)?;
            for a in &c.args {
                collect_expr(a, reads)?;
            }
            Ok(())
        }
        Expr::BinOp(b) => {
            collect_expr(&b.left, reads)?;
            collect_expr(&b.right, reads)
        }
        Expr::UnaryOp(u) => collect_expr(&u.expr, reads),
        Expr::Var(_) | Expr::Const(_) => Ok(()),
        // Any other expression form (List/Map/IfExpr/Slice/…): recurse defensively
        // by treating it as opaque is unsafe for a guard, so reject.
        _ => Err(TemplateError::Render(
            "system template uses an unsupported expression; \
             keep system templates to plain {{ variable }} substitutions"
                .to_string(),
        )),
    }
}

/// If `expr` is a variable access chain (`Var` or nested `GetAttr` rooted at a
/// `Var`), return its dotted path (e.g. `node.node_did`). Otherwise `None`.
fn dotted_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Var(v) => Some(v.id.to_string()),
        Expr::GetAttr(g) => {
            let base = dotted_path(&g.expr)?;
            Some(format!("{base}.{}", g.name))
        }
        _ => None,
    }
}
```

> **Implementation note for the engineer:** the exact field names on the
> `minijinja::machinery::ast` nodes (`Spanned<…>` wrappers, `.expr`, `.args`,
> `.name`, `.id`, `.children`, `Filter.expr: Option<…>`) are version-pinned to
> minijinja 2.19. If a field name differs, open
> `~/.cargo/registry/src/*/minijinja-2.19.*/src/machinery/ast.rs` and match the
> real struct fields. The control structure (emit-only subset for system,
> Var/GetAttr chain collection, filter-name exclusion) is the contract; the
> field access is mechanical.

- [ ] **Step 2: Expose the submodule**

In `crates/defra-agent/src/template/mod.rs` add (if not already added in Task 6):

```rust
pub mod reads;
```

- [ ] **Step 3: Write the failing conformance check (Task 4) now passes the guard tests**

Run: `cargo test -p defra-agent --test conformance prompt_template 2>&1 | tail -30`
Expected: the guard tests (`validate_*`, `collect_system_reads_returns_full_refs`, `catalog_volatility_matches_model`) compile. The `system_render_stable_under_per_request_change` test needs the extended `TemplateScope` (Task 8) — it may still fail to compile until then.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/template/reads.rs crates/defra-agent/src/template/mod.rs
git commit -m "feat(template): parser-backed complete-or-reject reads guard (#497)"
```

---

### Task 8: Extend `TemplateScope` with `node`/`ctx`

**Files:**
- Modify: `crates/defra-agent/src/template/mod.rs`

- [ ] **Step 1: Add the namespaces to the scope and context**

In `crates/defra-agent/src/template/mod.rs`, extend the `TemplateScope` struct:

```rust
pub struct TemplateScope {
    pub event: serde_json::Value,
    pub doc: Option<serde_json::Value>,
    pub args: Option<serde_json::Value>,
    /// Run-constant runtime context (`node.*`). Always present (may be `{}`).
    pub node: serde_json::Value,
    /// Per-request runtime context (`ctx.*`). Always present (may be `{}`).
    pub ctx: serde_json::Value,
}
```

In `render_template`, after the `args` insertion in the `context` builder, add:

```rust
        ctx.insert("node".to_string(), scope.node.clone());
        ctx.insert("ctx".to_string(), scope.ctx.clone());
```

- [ ] **Step 2: Fix existing constructors**

Every existing `TemplateScope { event, doc, args }` literal now needs `node`/`ctx`. Find them:

Run: `grep -rn "TemplateScope {" crates/defra-agent/src crates/defra-agent/tests`
For each non-`node` literal (the trigger-engine dispatch and template tests), add `node: serde_json::json!({}), ctx: serde_json::json!({}),` (Task 13 supplies real values at the trigger site). In `crates/defra-agent/src/template/tests.rs` add the two fields as empty objects to each constructor.

- [ ] **Step 3: Build + run template tests + the stability conformance test**

Run: `cargo test -p defra-agent template:: 2>&1 | tail -20`
Run: `cargo test -p defra-agent --test conformance prompt_template 2>&1 | tail -20`
Expected: all `prompt_template` conformance tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/template/mod.rs crates/defra-agent/src/template/tests.rs
git commit -m "feat(template): node/ctx runtime scopes in TemplateScope (#497)"
```

---

## Phase D — Behavior field plumbing

### Task 9: `request_context_template` end-to-end config surface

**Files:**
- Modify: `crates/defra-agent-schemas/schemas/agent/agent_behavior.graphql`
- Modify: `crates/defra-agent/src/document_config/behavior.rs`
- Modify: `crates/defra-agent/src/config.rs`
- Modify: `crates/defra-agent/src/agent/document_view/snapshot.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`
- Modify: `crates/defra-agent-cli/src/main.rs`

- [ ] **Step 1: Schema**

In `crates/defra-agent-schemas/schemas/agent/agent_behavior.graphql`, add after `system_prompt`:

```graphql
    request_context_template: String
```

- [ ] **Step 2: Document-config struct + queries + writers + default**

In `crates/defra-agent/src/document_config/behavior.rs`:

- Add to the `AgentBehavior` struct after `system_prompt`:
  ```rust
      pub request_context_template: Option<String>,
  ```
- Add `request_context_template` to the field list of **every** GraphQL query in the file (the read sites at the `system_prompt` lines ~67, ~107, ~158, and any in `list_agent_behaviors` / other loaders — grep `system_prompt` in this file and add the field one line below each).
- Add to **both** write renderers (lines ~196 and ~234), mirroring the `system_prompt` line exactly:
  ```rust
      graphql_fields::graphql_string_field("request_context_template", behavior.request_context_template.as_deref()),
  ```
- Add to the struct default/literal (line ~298):
  ```rust
      request_context_template: None,
  ```

Run after editing: `grep -c "request_context_template" crates/defra-agent/src/document_config/behavior.rs` — confirm the count equals (struct 1) + (each query field-list) + (2 writers) + (1 default).

- [ ] **Step 3: Update document_config tests fixture**

In `crates/defra-agent/src/document_config/tests.rs`, the fixture at line ~518 constructs an `AgentBehavior`. Add `request_context_template: None,` to it (and any other `AgentBehavior { … }` literal in that file — grep `system_prompt:` there).

- [ ] **Step 4: Runtime config**

In `crates/defra-agent/src/config.rs`, the `AgentBehavior` runtime struct (the one with `system_prompt: String`, around line 28-54) gains:

```rust
    pub request_context_template: Option<String>,
```

Find where this struct is constructed from the document-config behavior (grep `system_prompt:` in `config.rs` and `snapshot.rs`) and thread `request_context_template` through, mirroring `system_prompt`.

- [ ] **Step 5: Reconcile / resolved behavior**

In `crates/defra-agent/src/agent/document_view/snapshot.rs`, wherever the resolved/runtime `AgentBehavior` is built from the loaded document (grep `system_prompt`), carry `request_context_template` across.

- [ ] **Step 6: Desired-state acceptance (CLI)**

In `crates/defra-agent-cli/src/desired_state/mod.rs` (line ~39 rejects unknown behavior fields), add `request_context_template` to the accepted-behavior-fields set, mirroring `system_prompt`. Grep `system_prompt` in that file and add the field at each parallel site.

- [ ] **Step 7: Export allowlist (CLI)**

In `crates/defra-agent-cli/src/main.rs` (export allowlist ~line 375), add `request_context_template` to the behavior export field list, mirroring `system_prompt`.

- [ ] **Step 8: Build both crates**

Run: `cargo build -p defra-agent -p defra-agent-cli 2>&1 | tail -10`
Expected: builds. Fix any remaining `AgentBehavior { … }` literals the compiler flags (missing field) by adding `request_context_template: None`.

- [ ] **Step 9: Commit**

```bash
git add crates/defra-agent-schemas crates/defra-agent/src/document_config crates/defra-agent/src/config.rs crates/defra-agent/src/agent/document_view/snapshot.rs crates/defra-agent-cli/src/desired_state/mod.rs crates/defra-agent-cli/src/main.rs crates/defra-agent/src/document_config/tests.rs
git commit -m "feat(behavior): request_context_template config surface (#497)"
```

---

## Phase E — System render into the frozen preamble

### Task 10: Render `system_prompt` as a run-constant template at startup

**Files:**
- Modify: `crates/defra-agent/src/prompt.rs`
- Modify: `crates/defra-agent/src/template/mod.rs` (add a small render helper)

- [ ] **Step 1: Add a node-scope render helper**

In `crates/defra-agent/src/template/mod.rs`, add a helper that renders a template against only the run-constant `node` scope (used for the system prompt). It detects the no-markers fast path and validates before rendering:

```rust
/// Render a *system* template against the run-constant `node` scope.
///
/// Fast path: a template with no MiniJinja markers (`{{`, `{%`, `{#`) is
/// returned verbatim (existing literal system prompts are unaffected).
/// Otherwise the template is validated against the catalog (rejecting any
/// per-request/unknown ref or unanalyzable construct) and rendered.
pub fn render_system_prompt(
    template: &str,
    node: serde_json::Value,
    cat: &catalog::Catalog,
) -> Result<String, TemplateError> {
    if !template.contains("{{") && !template.contains("{%") && !template.contains("{#") {
        return Ok(template.to_string());
    }
    reads::validate_system_template(template, cat)?;
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
        node,
        ctx: serde_json::json!({}),
    };
    render_template(template, &scope)
}
```

- [ ] **Step 2: Render at preamble construction**

In `crates/defra-agent/src/prompt.rs`, locate `build_preamble_with_targets` (line ~261) — the first thing it does with `system_prompt` is `strip_title_generation_suffix(system_prompt)` then push it. The caller `for_behavior` (line ~170) has access to the behavior's principal (for `node.node_did`) and `behavior_name` (for `node.behavior_id`).

Thread a rendered system prompt in: at the start of `build_preamble_with_targets`, before the existing `let system_prompt = strip_title_generation_suffix(system_prompt);`, render it:

```rust
    let node = serde_json::json!({
        "node_did": agent_did,        // pass the principal DID into for_behavior/build_preamble
        "behavior_id": behavior_name, // already a param
    });
    let rendered = crate::template::render_system_prompt(
        system_prompt,
        node,
        &crate::template::catalog::default_catalog(),
    )
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "system_prompt template invalid; using literal");
        system_prompt.to_string()
    });
    let system_prompt = strip_title_generation_suffix(&rendered);
```

> The signature of `build_preamble_with_targets` currently takes `system_prompt: &str, behavior_name: &str, …`. Add an `agent_did: &str` parameter and pass `behavior.principal.agent_did` (or the existing did available in `for_behavior`) through from `for_behavior`. Mirror how `behavior_name` flows.

> **Cache-safety note:** because validation guarantees no per-request refs, rendering once here freezes the preamble; the `unwrap_or_else` literal fallback only triggers on an *invalid* template (logged), never on a valid one.

- [ ] **Step 3: Build + run prompt tests**

Run: `cargo test -p defra-agent prompt:: 2>&1 | tail -20`
Expected: builds and passes. Existing literal system prompts (no markers) are unchanged by the fast path.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/prompt.rs crates/defra-agent/src/template/mod.rs
git commit -m "feat(prompt): render system_prompt as run-constant template into frozen preamble (#497)"
```

---

## Phase F — Per-request context render + persistence

### Task 11: Render and inject the context message in the owned loop

**Files:**
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`

- [ ] **Step 1: Build the per-request ctx scope + render the context message**

In `crates/defra-agent/src/agent/loop_stream.rs`, in `run_loop_stream` just before `let mut new_messages: Vec<Message> = vec![prompt];` (line ~96), render the behavior's `request_context_template` (if present) and prepend it.

The render needs `node` (run-constant) + `ctx` (per-request). Build `ctx` lazily — only compute `collection_summary` if the template reads it:

```rust
    // Per-request context message (#497). Rendered ahead of the prompt; the
    // <context> tag matches the system-reminder shape so it reads as ambient.
    let context_message: Option<Message> = match &config.request_context_template {
        Some(tmpl) if !tmpl.trim().is_empty() => {
            let cat = crate::template::catalog::default_catalog();
            // Best-effort reads for lazy provider evaluation; on parse failure
            // compute the cheap vars only and let strict-undefined surface gaps.
            let reads = crate::template::reads::collect_request_reads(tmpl).unwrap_or_default();
            let ctx = build_ctx_scope(&self_ctx_inputs, &reads).await; // see Step 2
            let node = serde_json::json!({
                "node_did": config.principal.agent_did,
                "behavior_id": config.behavior_id,
            });
            let scope = crate::template::TemplateScope {
                event: serde_json::json!({}),
                doc: None,
                args: None,
                node,
                ctx,
            };
            match crate::template::render_template(tmpl, &scope) {
                Ok(rendered) => Some(Message::User {
                    content: vec![message::UserContent::Text(message::Text {
                        text: format!("<context>\n{rendered}\n</context>"),
                    })],
                }),
                Err(e) => {
                    tracing::error!(error = %e, "request_context_template render failed; skipping context");
                    None
                }
            }
        }
        _ => None,
    };

    let mut new_messages: Vec<Message> = match context_message.clone() {
        Some(ctx_msg) => vec![ctx_msg, prompt],
        None => vec![prompt],
    };
```

> The exact accessors (`config.principal.agent_did`, `config.behavior_id`, `config.request_context_template`) follow the `AgentBehavior` runtime config from Task 9 Step 4. `Message`/`UserContent`/`Text` are already imported in this file (used elsewhere); confirm the import path matches existing usage.

- [ ] **Step 2: Add `collect_request_reads` + a `ctx`-scope builder**

In `crates/defra-agent/src/template/reads.rs`, add a non-rejecting reads collector for request-context/task templates (control flow is allowed there; we only need the ref set for lazy eval + availability):

```rust
/// Collect refs from a request-context/task template without the system-only
/// statement restriction (loops/conditionals are allowed off the cache path).
/// Used for lazy provider evaluation and the availability check.
pub fn collect_request_reads(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    let ast = parse(template, "request_context", SyntaxConfig::default(), Default::default())
        .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let mut reads = BTreeSet::new();
    walk_stmt_any(&ast, &mut reads);
    Ok(reads)
}

/// Recurse all statements collecting variable refs from every embedded
/// expression (best-effort; never rejects).
fn walk_stmt_any(stmt: &Stmt, reads: &mut BTreeSet<String>) {
    match stmt {
        Stmt::Template(t) => for c in &t.children { walk_stmt_any(c, reads); },
        Stmt::EmitExpr(e) => { let _ = collect_expr(&e.expr, reads); }
        Stmt::ForLoop(f) => {
            let _ = collect_expr(&f.iter, reads);
            for c in &f.body { walk_stmt_any(c, reads); }
            for c in &f.else_body { walk_stmt_any(c, reads); }
        }
        Stmt::IfCond(i) => {
            let _ = collect_expr(&i.expr, reads);
            for c in &i.true_body { walk_stmt_any(c, reads); }
            for c in &i.false_body { walk_stmt_any(c, reads); }
        }
        _ => {} // other forms: best-effort, ignored for lazy-eval purposes
    }
}
```

> Field names (`f.iter`, `f.body`, `i.true_body`, …) are minijinja-2.19 pinned; verify against `machinery/ast.rs` as in Task 7.

For the `ctx`-scope builder, add a free function in `loop_stream.rs` (or a small `agent/context_scope.rs` module) that, given the runtime handles already available in `run_loop_stream` (the `node`/session/db handle used elsewhere in the loop) and the `reads` set, produces the `ctx` JSON object:

```rust
async fn build_ctx_scope(/* runtime handles */, reads: &std::collections::BTreeSet<String>) -> serde_json::Value {
    use chrono::SecondsFormat;
    let mut ctx = serde_json::Map::new();
    // Always-cheap per-request vars.
    ctx.insert("now".to_string(), serde_json::json!(
        chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    ));
    // Actor identity, if the runtime has it on the request (acting seat/DID).
    // Populate from the request's acting principal; empty string if absent.
    // ctx.insert("acting_seat", …); ctx.insert("acting_did", …);
    // Lazy: only query the live summary when the template reads it.
    if reads.contains("ctx.collection_summary") {
        ctx.insert("collection_summary".to_string(), serde_json::json!(
            collection_summary(/* db handle */).await.unwrap_or_default()
        ));
    }
    serde_json::Value::Object(ctx)
}
```

> `acting_seat`/`acting_did` and `collection_summary` source: wire from the
> request's acting principal and a small DefraDB summary query. If the acting
> principal is not yet plumbed into `run_loop_stream`, populate `now` (+
> `collection_summary` when read) for v1 and leave actor identity emitting empty
> strings; capture the gap in a follow-up rather than blocking. `collection_summary`
> should escape nothing into a mutation directly — it is a *read* query; the
> rendered value reaches persistence via Task 12 which escapes at the mutation.

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent 2>&1 | tail -10`
Expected: builds.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/agent/loop_stream.rs crates/defra-agent/src/template/reads.rs
git commit -m "feat(loop): render + inject per-request <context> message (#497)"
```

---

### Task 12: Durably persist the context message before the prompt

**Files:**
- Modify: `crates/defra-agent/src/hook/persistence/prompt_hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence/message_spawn.rs`
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`

- [ ] **Step 1: Persist the context message with an earlier sequence than the prompt**

The context message reaches the provider but is **not** captured by `on_completion_call` (which persists only `prompt`). Persist it explicitly so the durable `AgentMessage` order matches provider order (context sequence < prompt sequence).

In `crates/defra-agent/src/hook/persistence/prompt_hook.rs`, extend `on_completion_call` to accept and persist an optional context message *before* the prompt. Change its signature and body:

```rust
    pub async fn on_completion_call(
        &self,
        prompt: &Message,
        context: Option<&Message>,
        _history: &[Message],
    ) -> HookAction {
        let result: anyhow::Result<()> = async {
            let mut state = self.state.lock().await;
            if !state.initialized {
                let session_id =
                    session::create_session(&self.node, &state.agent_name, &self.agent_did).await?;
                state.session_id = Some(session_id);
                state.initialized = true;
            }
            state.reset_after_user_message();
            drop(state);

            if let Some(ctx) = context {
                self.persist_message(ctx).await?; // earlier sequence
            }
            self.persist_message(prompt).await?;   // later sequence
            Ok(())
        }
        .await;

        match result {
            Ok(()) => { self.record_success(); HookAction::Continue }
            Err(e) => self.on_persistence_error("persist user prompt", &e),
        }
    }
```

> `persist_message` (in `message_spawn.rs`) already calls `session::append_message`, which assigns the next sequence; persisting context first then prompt yields the correct ordering with no new sequence logic.

- [ ] **Step 2: Pass the context message at the call site**

In `crates/defra-agent/src/agent/loop_stream.rs`, find the `on_completion_call(&prompt, …)` invocation (the first-turn persistence call) and pass the context message built in Task 11:

```rust
    hook.on_completion_call(&new_messages.last().unwrap(), context_message.as_ref(), &history).await;
```

> Match the real existing call shape — the prompt argument it currently passes (likely `&new_messages[0]` or the original `prompt`). Pass `context_message.as_ref()` as the new arg. Update any other `on_completion_call` call sites (grep) to pass `None`.

- [ ] **Step 3: Integration test — context persisted with correct order**

Add a focused test asserting both existence and ordering. Prefer extending an existing loop/persistence integration test; if writing fresh, add to the existing persistence test module. The assertion:

```rust
// After driving one request whose behavior sets request_context_template:
// load AgentMessage rows for the session ordered by sequence; assert the
// <context> user message has a strictly smaller sequence than the prompt.
let rows = load_messages_for_session(&node, &session_id).await;
let ctx_seq = rows.iter().find(|m| m.content.contains("<context>")).map(|m| m.sequence);
let prompt_seq = rows.iter().find(|m| m.content.contains(PROMPT_MARKER)).map(|m| m.sequence);
assert!(ctx_seq.is_some(), "context message must be durably persisted");
assert!(ctx_seq < prompt_seq, "context sequence must precede prompt sequence");
```

> Use the existing test harness's session/message loader (grep `load_messages` / `AgentMessage` in `tests/`). If a full loop test is too heavy, assert at the `persist_message` level that two calls in order produce ascending sequences and the context content round-trips.

- [ ] **Step 4: Build + test**

Run: `cargo test -p defra-agent 2>&1 | tail -20`
Expected: builds and passes, including the new persistence-order assertion.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/hook/persistence/prompt_hook.rs crates/defra-agent/src/hook/persistence/message_spawn.rs crates/defra-agent/src/agent/loop_stream.rs
git commit -m "feat(persistence): durably capture context message before prompt (#497)"
```

---

## Phase G — Task interop

### Task 13: Merge `node`/`ctx` into task `TemplateScope`

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`
- Modify: `crates/defra-agent/src/template/mod.rs` (availability check helper, optional)

- [ ] **Step 1: Populate node/ctx at trigger fire**

In `crates/defra-agent/src/trigger_engine/mod.rs`, the dispatch path (lines ~294–316) builds:

```rust
   let scope = crate::template::TemplateScope {
       event: intent.event_vars.clone(),
       doc: intent.doc_vars.clone(),
       args: intent.args_vars.clone(),
   };
```

Extend it with the task-available catalog vars (`node.*` run-constant + `ctx.now`):

```rust
   let node = serde_json::json!({
       "node_did": intent.task.agent_did_or_principal,   // the deployment DID
       "behavior_id": intent.task.behavior_id,
   });
   let ctx = serde_json::json!({
       "now": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
   });
   let scope = crate::template::TemplateScope {
       event: intent.event_vars.clone(),
       doc: intent.doc_vars.clone(),
       args: intent.args_vars.clone(),
       node,
       ctx,
   };
```

> Source the DID from whatever the dispatch already has (the `ResolvedTask`/`FireIntent` carries `behavior_id`; the deployment DID is available on the runtime snapshot used here — mirror how the request is written with `agent_did` in `lifecycle/manual.rs`). For v1, `ctx.now` is the only task `ctx.*` var (matches the "system time into task start" interop ask); `event.fired_at` remains for existing templates.

- [ ] **Step 2: Extend trigger-scope validation to know node/ctx**

The apply-time `parse_template_for_validation` scope check rejects scopes a trigger kind doesn't supply. Add `node` and `ctx` (with `ctx` limited to task-available vars) to the allowed roots for task templates so `{{ node.node_did }}` / `{{ ctx.now }}` validate. Find the validation match (grep `parse_template_for_validation` usage in trigger/apply paths) and add `"node"` and `"ctx"` to the permitted roots for the task render site, rejecting non-task `ctx.*` vars (`acting_seat`, `acting_did`, `collection_summary`) via the catalog `is_available_at(.., Site::Task)` check.

- [ ] **Step 3: Test task interop**

Extend an existing trigger e2e/conformance test (e.g. `tests/conformance/triggers.rs` or `tests/e2e_triggers/`) with a task whose `prompt_template` reads `{{ ctx.now }}` and `{{ node.node_did }}`; assert the rendered request content contains a plausible timestamp and the DID.

```rust
// PROMPT_TEMPLATE: "tick at {{ ctx.now }} on {{ node.node_did }}"
// after fire: assert request content contains "did:" and "T..:..:..Z"
```

- [ ] **Step 4: Build + test**

Run: `cargo test -p defra-agent trigger 2>&1 | tail -20`
Expected: builds and passes.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/trigger_engine/mod.rs crates/defra-agent/src/template/mod.rs
git commit -m "feat(triggers): node/ctx catalog scopes in task templates (#497)"
```

---

## Phase H — Gate and review

### Task 14: Full gate

- [ ] **Step 1: Proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero `sorry`. Confirm: `grep -rn "sorry" Proofs/PromptAssembly/Template.lean` returns nothing.

- [ ] **Step 2: Rust suites (full packages, not --lib)**

Run: `cargo test -p defra-agent`
Run: `cargo test -p defra-agent-cli`
Expected: green. (Per project notes, some Lean-conformance/trigger tests can flake under parallel build — rerun any isolated failure once before treating it as real; see memory `reference_lean_conformance_flake_parallel_build` / `reference_trigger_conformance_snapshot_flake`.)

- [ ] **Step 3: fmt + clippy**

Run: `cargo fmt --all && cargo clippy -p defra-agent -p defra-agent-cli --all-targets 2>&1 | tail -20`
Expected: no new warnings; commit any fmt drift.

- [ ] **Step 4: Commit any fixups**

```bash
git add -A && git commit -m "chore: fmt/clippy fixups for prompt templating (#497)"
```

---

### Task 15: Whole-branch review

- [ ] **Step 1: Spec-compliance + final review**

Use `superpowers:requesting-code-review` (or `/code-review high`) over the full branch diff against `main`. Verify against the spec:
- system template guard is fail-closed (per-request, unknown, control-flow all rejected; `{% raw %}` accepted);
- context message is durably persisted with sequence < prompt;
- `request_context_template` round-trips through export/import;
- `escape_graphql_string` is used for any rendered value interpolated into a mutation; no `[]` literals emitted (use `null`);
- `tracing` (no `println`);
- the Lean model has zero `sorry` and the conformance fence passes.

- [ ] **Step 2: Address findings**

Use `superpowers:receiving-code-review` — verify each finding against the code before implementing; push back with reasoning where warranted.

- [ ] **Step 3: Finish the branch**

Use `superpowers:finishing-a-development-branch` to choose merge/PR. (Per worktree org model, the coordinator merges via squash PR — open the PR referencing #497.)

---

## Self-review notes (plan author)

- **Spec coverage:** D1 (context user msg)→Task 11; D2 (reuse engine)→Tasks 6–8; D3 (catalog)→Task 6; D4 (system render-once frozen)→Task 10; D5 (request_context_template)→Tasks 9, 11; D6 (lazy providers)→Task 11 Step 2; D7 (complete-or-reject)→Task 7. Catalog v1→Task 6. Lean theorems→Tasks 1–2; conformance→Tasks 3–4; config surface→Task 9; persistence order→Task 12; task interop→Task 13.
- **Known soft spots flagged inline (not placeholders):** minijinja AST field names are version-pinned and must be verified against the 2.19 source (Tasks 7, 11); `acting_seat`/`acting_did` plumbing may be deferred to a follow-up if the acting principal isn't in `run_loop_stream` yet (Task 11) — emit empty strings, don't block.
- **Type consistency:** `Volatility` (Lean) ↔ `catalog::Volatility` (Rust); `collect_system_reads`/`validate_system_template`/`collect_request_reads`/`render_system_prompt`/`default_catalog` names are used identically across Tasks 4, 6, 7, 8, 10, 11.
