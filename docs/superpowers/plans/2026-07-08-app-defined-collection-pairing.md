# App-Defined Collection Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one agent replicate an app-defined `@branchable` collection (e.g. `ChangeProposed`) to a paired peer as desired-state config, so a merged document fires the peer's `EventTrigger` through the reconcile path — no hand-wired `add_replicator`.

**Architecture:** Add one `app-collections` policy template (`Unscoped` + `Replicate`, bring-your-own collections). Honor the already-existing-but-dropped `DataPlanePairingDesired.collections` field, gated on `template == "app-collections"`. Populate the subscription set at both the resolver and `merge_layered_desired`. Soft-skip malformed rows so a bad data-plane row never stalls a co-existing control pairing; reject `app-collections` on the control-plane path.

**Tech Stack:** Rust (`defra-agent`, `defra-agent-cli`, `defra-agent-schemas`), Lean 4 + Mathlib (`crates/defra-agent/proofs`), DefraDB P2P (embedded), tokio integration tests.

**Spec:** `docs/superpowers/specs/2026-07-08-app-defined-collection-pairing-design.md`. Issue #657. Follow-up epic #660.

## Global Constraints

- **Lean-first foundation flow:** Lean model → conformance tests → Rust. Zero `sorry`s. (CLAUDE.md)
- **`graphql::escape_graphql_string()`** for every value interpolated into a GraphQL string.
- **Never emit `[]`** in a DefraDB mutation — an empty list literal corrupts nillable array columns; emit `null`.
- **Gate with the full package suite** `cargo test -p defra-agent` (not `--lib`; integration tests are separate compile units).
- **Compile the whole workspace before pushing:** `cargo check --workspace --all-targets`.
- **`tracing`, never `println`.**
- **Clippy clean:** `cargo clippy --workspace --all-targets -- -D warnings`.
- **New template id string is exactly `app-collections`;** the Rust const is `APP_COLLECTIONS_TEMPLATE`; the Lean def is `appCollectionsTemplate`. Use these verbatim everywhere.
- **Replicated app collections must be `@branchable`** (operator/schema responsibility; the e2e schema carries it).
- **PR to `sourcenetwork/defra-agent` off `origin/main`, referencing #657. Do not merge.**

---

## File Structure

- `crates/defra-agent/proofs/Proofs/ScopeTemplates/State.lean` — add `appCollectionsTemplate` + extend `builtinCatalog` (Task 1a).
- `crates/defra-agent/proofs/Proofs/ScopeTemplates/Derivation.lean` — add catalog-membership theorem (Task 1a).
- `crates/defra-agent/proofs/Proofs/PairingReconcile/Layering.lean` — **new**: model `mergeLayered` + the delivery-aware subscription rule; prove app-collections subscription survives, push data-plane subscription is cleared (no unfiltered gossip), control-plane base preserved (Task 1b).
- `crates/defra-agent/proofs/Proofs/PairingReconcile.lean` — add `import Proofs.PairingReconcile.Layering` to the barrel (Task 1b).
- `crates/defra-agent/tests/conformance/scope_templates.rs` — conformance: app-collections resolves as Unscoped/Replicate/empty template set (Task 2).
- `crates/defra-agent/src/agent/p2p_reconcile/templates.rs` — new template + const + fix two unit tests (Task 3).
- `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` — `load_desired` query; `data_plane_desired_from_pairing_row` (honor row collections, soft-skip, `Result<Option<..>>`); `desired_from_pairing_row` (reject app-collections, `Result<Option<..>>`); `merge_layered_desired` conditional subscription preservation; **plus its in-crate `#[cfg(test)] mod tests`** — new app-collections resolver/soft-skip/reject tests AND updates to the existing resolver call sites that the signature change breaks (Tasks 4–7).
- `crates/defra-agent/tests/conformance/pairing_reconcile.rs` — merge subscription-preservation conformance only (Task 5, via the public `merge_layered_desired`). Resolution / soft-skip / reject tests are in-crate in engine.rs (Tasks 6–7), not here.
- `crates/defra-agent-cli/src/commands/p2p/pairings.rs` — reject `app-collections` on generic pairing path (Task 8).
- `crates/defra-agent/tests/e2e_triggers.rs` — the harness root (a file, not `mod.rs`); add `mod app_collection_pairing_p2p_e2e;` next to `mod event_trigger_p2p_e2e;` (line ~11) (Task 9).
- `crates/defra-agent/tests/e2e_triggers/app_collection_pairing_p2p_e2e.rs` — membership-materialization harness + acceptance e2e (Tasks 9–10).

---

## Task 1: Lean — catalog entry + layered-merge subscription model

Two Lean model changes proven together under one `lake build`: **1a** adds the
`app-collections` template to the catalog; **1b** models `mergeLayered` and proves
the delivery-aware subscription rule that the Rust `merge_layered_desired` change
(Task 5) implements. 1b exists because the "data-plane push layers do not extend
the subscription set, so conversation data never gossips unfiltered" property is
safety-relevant, and this change makes an app-collections exception to it — that
is exactly a "what invariants hold" change, which starts in Lean (CLAUDE.md).

**Scope boundary (stated deliberately):** the *source* of a data-plane layer's
collection set (row vs template) and the *soft-skip* of a malformed row are
input-validation plumbing below the machine boundary — the reconcile machine
consumes an abstract desired state and proves nothing about where its Finsets
originate, and "malformed input → no layer" is not a safety property. Those are
fenced by Rust unit/conformance (Tasks 6–7), not Lean. Only the subscription
rule — which governs what may gossip — is modeled here.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ScopeTemplates/State.lean:157-165` (builtinCatalog) + add a def near line 155
- Modify: `crates/defra-agent/proofs/Proofs/ScopeTemplates/Derivation.lean` (add theorem near line 188)
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile/Layering.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile.lean` (barrel import)

**Interfaces:**
- Produces: `ScopeTemplates.appCollectionsTemplate : Template`; `builtinCatalog` now has 8 entries; theorem `appCollections_in_catalog`. `PairingReconcile.Layering.{Layer, clampDataPlane, mergeLayered}` + subscription-rule theorems mirrored by Rust conformance in Task 5.

- [ ] **Step 1: Add the template def and catalog entry**

In `State.lean`, add after `subagentHostTemplate` (line 155):

```lean
def appCollectionsTemplate : Template :=
  { id := "app-collections"
  , collections := (∅ : Finset String)
  , scope := .unscoped
  , delivery := .replicate }
```

Then extend `builtinCatalog` (line 158) to include it as the final entry:

```lean
def builtinCatalog : Catalog :=
  [ conversationTemplate
  , agentConfigTemplate
  , backupTemplate
  , discoveryTemplate
  , networkControlTemplate
  , subagentCoordinatorTemplate
  , subagentHostTemplate
  , appCollectionsTemplate ]
```

- [ ] **Step 2: Add the catalog-membership theorem**

In `Derivation.lean`, after `subagentHost_in_catalog` (line 188):

```lean
/-- Concrete catalog membership: the app-collections (bring-your-own) template
resolves from the built-in catalog. Its collection set is empty by contract —
the DataPlanePairingDesired row supplies the collections. -/
theorem appCollections_in_catalog :
    resolveTemplate builtinCatalog "app-collections" = some appCollectionsTemplate := by
  decide

/-- The app-collections template carries no fixed collections: its set is
row-supplied, not catalog-fixed. -/
theorem appCollections_collections_empty :
    appCollectionsTemplate.collections = (∅ : Finset String) := rfl

/-- The app-collections template is whole-collection replicate (no per-peer
filter): Unscoped scope yields no filters for any collection list. -/
theorem appCollections_unscoped_no_filter (collections : List String) (peerDid localDid : Did) :
    scopeFilter appCollectionsTemplate.scope collections peerDid localDid = [] := rfl
```

- [ ] **Step 3 (1b): Create the layered-merge model**

Create `crates/defra-agent/proofs/Proofs/PairingReconcile/Layering.lean`:

```lean
import Proofs.Basic
import Mathlib.Data.Finset.Basic

/-!
# Pairing Reconcile — Layered merge (subscription rule)

A pure derivation beside the reconcile machine (the ScopeTemplates pattern: a
derivation below the machine, not a new machine). Models Rust
`merge_layered_desired`: a control-plane base desired layer is merged with an
optional data-plane layer. A data-plane layer's *subscriptions* are dropped
UNLESS it is the app-collections (whole-collection Replicate) policy — push
data-plane layers must not extend the subscription set, so conversation data
never gossips unfiltered; app-collections is Unscoped Replicate and must
subscribe on both sides for the merged doc to be observable. Replicator
collection sets always union (the machine keys replicator identity on them).
-/

namespace PairingReconcile.Layering

/-- The fields of Rust `PairingDesired` that the merge rule reads. -/
structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  /-- `template_ids.contains "app-collections"` in Rust. -/
  isAppCollections : Bool
  deriving DecidableEq, Repr

/-- Drop a data-plane layer's subscriptions unless it is app-collections. -/
def clampDataPlane (l : Layer) : Layer :=
  if l.isAppCollections then l else { l with subscriptions := (∅ : Finset String) }

/-- Merge a control-plane base with an optional data-plane layer. Mirrors Rust
`merge_layered_desired`: clamp the data-plane layer, then union. -/
def mergeLayered (base : Option Layer) (dataPlane : Option Layer) : Option Layer :=
  match base, dataPlane.map clampDataPlane with
  | none, none => none
  | some b, none => some b
  | none, some d => some d
  | some b, some d =>
      some { subscriptions := b.subscriptions ∪ d.subscriptions
           , replicatorCollections := b.replicatorCollections ∪ d.replicatorCollections
           , isAppCollections := b.isAppCollections }

/-- An app-collections data-plane layer's subscriptions survive the merge, so an
`InstallCollection` op can reach the diff. -/
theorem appCollections_subscription_survives
    (base : Option Layer) (d : Layer) (h : d.isAppCollections = true)
    (m : Layer) (hm : mergeLayered base (some d) = some m) :
    d.subscriptions ⊆ m.subscriptions := by
  cases base with
  | none => simp [mergeLayered, clampDataPlane, h] at hm; subst hm; exact subset_rfl
  | some b =>
      simp [mergeLayered, clampDataPlane, h] at hm; subst hm; exact Finset.subset_union_right

/-- A non-app-collections data-plane layer contributes NO subscriptions: with an
empty base the merged subscription set is empty (push data never gossips
unfiltered). -/
theorem nonApp_none_base_no_subscription
    (d : Layer) (h : d.isAppCollections = false) :
    mergeLayered none (some d) = some { d with subscriptions := (∅ : Finset String) } := by
  simp [mergeLayered, clampDataPlane, h]

/-- A non-app-collections data-plane layer does not add to a base's subscriptions:
the merged subscription set equals the base's. -/
theorem nonApp_subscription_eq_base
    (b d : Layer) (h : d.isAppCollections = false)
    (m : Layer) (hm : mergeLayered (some b) (some d) = some m) :
    m.subscriptions = b.subscriptions := by
  simp [mergeLayered, clampDataPlane, h] at hm; subst hm; simp

/-- The control-plane base is always preserved (subscriptions and replicator
collections are supersets of the base's), regardless of the data-plane layer. -/
theorem base_preserved
    (b : Layer) (dp : Option Layer) (m : Layer)
    (hm : mergeLayered (some b) dp = some m) :
    b.subscriptions ⊆ m.subscriptions ∧ b.replicatorCollections ⊆ m.replicatorCollections := by
  cases dp with
  | none => simp [mergeLayered] at hm; subst hm; exact ⟨subset_rfl, subset_rfl⟩
  | some d =>
      simp [mergeLayered, clampDataPlane] at hm
      subst hm
      exact ⟨Finset.subset_union_left, Finset.subset_union_left⟩
```

Add to the barrel `crates/defra-agent/proofs/Proofs/PairingReconcile.lean`:

```lean
import Proofs.PairingReconcile.Layering
```

- [ ] **Step 4: Build the proofs and verify zero sorries**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: builds clean, no errors, no `sorry` warnings. (If a fresh worktree, first symlink the parent's mathlib `.lake/build` per the reference note, then `lake build`.) If a `simp`/`subst` tactic above does not close a goal, adjust it (e.g. add the relevant `Finset.subset_union_left`/`Finset.mem_union` lemma or `cases`/`rfl`) until the goal is discharged — the statements are exact; only the proof term may need tuning. Zero `sorry` is the gate.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ScopeTemplates/State.lean crates/defra-agent/proofs/Proofs/ScopeTemplates/Derivation.lean crates/defra-agent/proofs/Proofs/PairingReconcile/Layering.lean crates/defra-agent/proofs/Proofs/PairingReconcile.lean
git commit -m "proof: app-collections catalog + layered-merge subscription rule (#657)"
```

---

## Task 2: Conformance — `app-collections` resolution mirror

**Files:**
- Modify: `crates/defra-agent/tests/conformance/scope_templates.rs`

**Interfaces:**
- Consumes: real `defra_agent::agent::p2p_reconcile::templates::{resolve_template, Delivery, Scope}` (Task 3 must make `resolve_template("app-collections")` return `Some`; this test is written first and fails until then).

- [ ] **Step 1: Write the failing conformance test**

Append to `scope_templates.rs`:

```rust
/// Mirrors Lean `appCollections_in_catalog` / `appCollections_collections_empty`
/// / `appCollections_unscoped_no_filter`: the app-collections "bring-your-own"
/// template resolves, is Unscoped + Replicate, and carries no fixed collections
/// (the DataPlanePairingDesired row supplies them).
#[test]
fn app_collections_template_is_unscoped_replicate_byo() {
    let t = resolve_template("app-collections").expect("app-collections in catalog");
    assert_eq!(t.id, "app-collections");
    assert!(matches!(t.delivery, Delivery::Replicate));
    assert!(matches!(t.scope, Scope::Unscoped));
    assert!(
        t.collections.is_empty(),
        "app-collections carries no fixed collections; the row supplies them"
    );
    // Unscoped yields no filters even over a supplied collection list.
    let f = scope_filter(&t.scope, &["ChangeProposed"], "did:key:bob", "did:key:alice");
    assert!(f.is_empty(), "unscoped app-collections must not filter");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p defra-agent --test conformance app_collections_template_is_unscoped_replicate_byo`
Expected: FAIL — `resolve_template("app-collections")` returns `None` (panics on `.expect`).

- [ ] **Step 3: Commit the failing test**

```bash
git add crates/defra-agent/tests/conformance/scope_templates.rs
git commit -m "test(conformance): app-collections template resolution (failing) (#657)"
```

---

## Task 3: Rust — add the `app-collections` template

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`

**Interfaces:**
- Produces: `pub const APP_COLLECTIONS_TEMPLATE: &str = "app-collections";`; a new `ScopeTemplate` in `BUILTIN_TEMPLATES` with `collections: &[]`, `scope: Scope::Unscoped`, `delivery: Delivery::Replicate`. `resolve_template("app-collections")` returns `Some`.

- [ ] **Step 1: Add the constant**

In `templates.rs`, next to the other template-id consts (after line 222):

```rust
pub const APP_COLLECTIONS_TEMPLATE: &str = "app-collections";
```

- [ ] **Step 2: Add the catalog entry**

Append to `BUILTIN_TEMPLATES` (before the closing `]` at line 267):

```rust
    ScopeTemplate {
        id: APP_COLLECTIONS_TEMPLATE,
        // Bring-your-own: the DataPlanePairingDesired row supplies the set.
        collections: &[],
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
```

- [ ] **Step 3: Fix the two unit tests the new entry breaks**

Update `all_builtin_templates_have_nonempty_collections` (line 386) to exempt the bring-your-own template:

```rust
    #[test]
    fn all_builtin_templates_have_nonempty_collections() {
        for t in builtin_templates() {
            // app-collections is the one bring-your-own template: its collection
            // set is supplied by the DataPlanePairingDesired row, not the catalog.
            if t.id == APP_COLLECTIONS_TEMPLATE {
                assert!(
                    t.collections.is_empty(),
                    "app-collections must carry no fixed collections"
                );
                continue;
            }
            assert!(
                !t.collections.is_empty(),
                "template {} has no collections",
                t.id
            );
        }
    }
```

Update `builtin_template_count_is_seven` (line 397) to eight and rename:

```rust
    #[test]
    fn builtin_template_count_is_eight() {
        assert_eq!(builtin_templates().len(), 8);
    }
```

- [ ] **Step 4: Add a focused unit test for the new template**

```rust
    #[test]
    fn app_collections_is_byo_unscoped_replicate() {
        let t = resolve_template(APP_COLLECTIONS_TEMPLATE).unwrap();
        assert_eq!(t.delivery, Delivery::Replicate);
        assert!(matches!(t.scope, Scope::Unscoped));
        assert!(t.collections.is_empty());
    }
```

- [ ] **Step 5: Run templates unit tests + the Task 2 conformance test**

Run: `cargo test -p defra-agent --lib p2p_reconcile::templates`
Expected: PASS (including the count/nonempty/byo tests).
Run: `cargo test -p defra-agent --test conformance app_collections_template_is_unscoped_replicate_byo`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/templates.rs
git commit -m "feat(p2p): add app-collections bring-your-own scope template (#657)"
```

---

## Task 4: Rust — select `collections` in `load_desired`

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs:497-502` (the `DataPlanePairingDesired` sub-query)

**Interfaces:**
- Produces: `PairingStateRow.collections` is populated for data-plane rows (already deserialized; only the query field was missing).

- [ ] **Step 1: Add `collections` to the sub-query**

In `load_desired` (the query around line 497), change the `DataPlanePairingDesired` selection to include `collections`:

```rust
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    agent_did
                    collections
                    replicator_addresses
                    template
                }}
```

- [ ] **Step 2: Compile check (no behavior change yet)**

Run: `cargo check -p defra-agent`
Expected: compiles clean (the field flows into `PairingStateRow.collections`, still unused by the resolver until Task 6).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs
git commit -m "feat(p2p): read DataPlanePairingDesired.collections in load_desired (#657)"
```

---

## Task 5: Rust — `merge_layered_desired` preserves the app-collections subscription

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs:894-919` (`merge_layered_desired`)
- Test: `crates/defra-agent/tests/conformance/pairing_reconcile.rs`

**Interfaces:**
- Consumes: `PairingDesired.template_ids: BTreeSet<String>` (already exists), `templates::APP_COLLECTIONS_TEMPLATE` (Task 3).
- Produces: `merge_layered_desired` keeps the data-plane layer's `collections` when `template_ids` contains `app-collections`; clears it otherwise (unchanged for every existing template).

- [ ] **Step 1: Write the failing conformance test**

Append to `pairing_reconcile.rs` (uses the existing `set(&[...])` helper in that file):

```rust
/// Mirrors Lean `PairingReconcile.Layering.appCollections_subscription_survives`
/// / `nonApp_none_base_no_subscription`: an `app-collections` data-plane layer's
/// subscription set survives `merge_layered_desired`, so an `InstallCollection`
/// op can reach the diff; a network-control-only data-plane layer's subscription
/// is still cleared (conversation data must never gossip unfiltered).
#[test]
fn merge_preserves_app_collections_subscription_only() {
    use defra_agent::agent::p2p_reconcile::diff::PairingDesired;
    use defra_agent::agent::p2p_reconcile::engine::merge_layered_desired;

    // app-collections data-plane layer: subscription set must survive.
    let app_layer = PairingDesired {
        collections: set(&["ChangeProposed"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ChangeProposed"]),
        replicator_filter: Default::default(),
        template_ids: set(&["app-collections"]),
    };
    let merged = merge_layered_desired(None, Some(app_layer)).expect("merged");
    assert!(
        merged.collections.contains("ChangeProposed"),
        "app-collections subscription must survive the merge: {merged:?}"
    );

    // network-control data-plane layer: subscription still cleared.
    let nc_layer = PairingDesired {
        collections: set(&["AgentRequest"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: Default::default(),
        template_ids: set(&["network-control"]),
    };
    let merged_nc = merge_layered_desired(None, Some(nc_layer)).expect("merged nc");
    assert!(
        merged_nc.collections.is_empty(),
        "non-app-collections data-plane subscription must be cleared: {merged_nc:?}"
    );
}

/// Spec conformance case (iii); mirrors Lean `PairingReconcile.Layering.base_preserved`:
/// an app-collections data-plane layer merges with a co-existing control
/// (network-control) base pairing without cross-contaminating their subscriptions
/// or replicator filters — the control pairing is undisturbed.
#[test]
fn app_collections_coexists_with_control_pairing() {
    use defra_agent::agent::p2p_reconcile::diff::PairingDesired;
    use defra_agent::agent::p2p_reconcile::engine::merge_layered_desired;

    let base = PairingDesired {
        collections: set(&["AgentNetwork", "NetworkMembership"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["AgentNetwork", "NetworkMembership"]),
        replicator_filter: Default::default(),
        template_ids: set(&["network-control"]),
    };
    let app_layer = PairingDesired {
        collections: set(&["ChangeProposed"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ChangeProposed"]),
        replicator_filter: Default::default(),
        template_ids: set(&["app-collections"]),
    };
    let merged = merge_layered_desired(Some(base), Some(app_layer)).expect("merged");
    // Control-plane subscriptions preserved AND the app-collections subscription added.
    assert!(merged.collections.contains("AgentNetwork"));
    assert!(merged.collections.contains("NetworkMembership"));
    assert!(merged.collections.contains("ChangeProposed"));
    // Both replicator collection sets present; no filter cross-contamination.
    assert!(merged.replicator_collections.contains("AgentNetwork"));
    assert!(merged.replicator_collections.contains("ChangeProposed"));
    assert!(merged.replicator_filter.is_empty(), "both layers unscoped => no filter");
    assert!(merged.template_ids.contains("network-control"));
    assert!(merged.template_ids.contains("app-collections"));
}
```

If `merge_layered_desired` or `PairingDesired` are not already `pub` at those paths, note it — Step 3 makes them reachable. (`PairingDesired` is `pub` in `diff.rs`; confirm `merge_layered_desired` is `pub` in `engine.rs` — it is, per `pub fn merge_layered_desired`.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p defra-agent --test conformance merge_preserves_app_collections_subscription_only`
Expected: FAIL — the current blanket `desired.collections.clear()` empties the app-collections subscription, so `merged.collections.contains("ChangeProposed")` is false.

- [ ] **Step 3: Make the clear conditional**

Replace the unconditional clear (lines 902-905) with:

```rust
    // Layer-2 desired rows add data-plane collections to the per-peer
    // replicator, not to the subscription set — EXCEPT the app-collections
    // (bring-your-own) policy, which is a whole-collection Replicate that must
    // subscribe on both sides for the merged doc to be observable. All other
    // data-plane layers keep the clear so conversation data never gossips
    // unfiltered.
    let data_plane = data_plane.map(|mut desired| {
        if !desired
            .template_ids
            .contains(super::templates::APP_COLLECTIONS_TEMPLATE)
        {
            desired.collections.clear();
        }
        desired
    });
```

(`super::templates::APP_COLLECTIONS_TEMPLATE` — confirm the `use`/path resolves in `engine.rs`; it already references `super::templates::*` elsewhere.)

- [ ] **Step 4: Run it to verify it passes + run existing merge tests**

Run: `cargo test -p defra-agent --test conformance merge_preserves_app_collections_subscription_only`
Expected: PASS.
Run: `cargo test -p defra-agent --lib p2p_reconcile::engine`
Expected: PASS (existing `merge_layered_*` unit tests at engine.rs:1017+ unaffected — they use network-control/AgentRequest layers that still clear).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs crates/defra-agent/tests/conformance/pairing_reconcile.rs
git commit -m "feat(p2p): preserve app-collections subscription through merge_layered_desired (#657)"
```

---

## Task 6: Rust — honor row collections + soft-skip in `data_plane_desired_from_pairing_row`

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs:738-828` (`data_plane_desired_from_pairing_row`) and its call site in `load_desired` (lines 513-523)
- Test: in-crate `#[cfg(test)] mod tests` in `engine.rs` (new tests + updates to existing call sites the signature change breaks)

**Interfaces:**
- Consumes: `templates::APP_COLLECTIONS_TEMPLATE`, `PairingStateRow.collections` (Task 4).
- Produces: signature changes to `fn data_plane_desired_from_pairing_row(row, signed_endpoint, self_did) -> Result<Option<PairingDesired>>`. For `template == "app-collections"`: `replicator_collections` and `collections` (subscription) both = trimmed row collections, filter Unscoped (`{}`); empty trimmed set → `Ok(None)` (soft-skip). For other templates → `Ok(Some(..))` unchanged. Foreign `agent_did` still `Err`.

- [ ] **Step 1: Write the failing unit tests (in-crate, no feature seam)**

`data_plane_desired_from_pairing_row` is private; the repo already tests it directly from the in-crate `#[cfg(test)] mod tests` in `engine.rs` (see `data_plane_desired_uses_signed_endpoint_address_and_self_did` at ~engine.rs:1073, which constructs `PairingStateRow` + `NetworkEndpointEntry` inline). Add the two new tests to **that same module** — do NOT invent a `conformance-seams` feature: integration tests compile `defra-agent` as a normal dependency, so a `#[cfg(any(test, feature=...))]` `pub` seam is invisible to `tests/conformance/*` unless the feature is declared in `Cargo.toml` AND the test run enables it. In-crate `#[cfg(test)]` is the idiomatic home here. Append inside the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn app_collections_row_resolves_row_collections_as_subscription_and_replicator() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let layer = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:self".to_string()), // == self_did (allowed)
                collections: Some(vec!["ChangeProposed".to_string()]),
                replicator_addresses: None,
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect("resolve ok")
        .expect("some layer");
        assert!(layer.replicator_collections.contains("ChangeProposed"));
        assert!(
            layer.collections.contains("ChangeProposed"),
            "app-collections must subscribe (Replicate)"
        );
        assert!(layer.replicator_filter.is_empty(), "unscoped => no filter");
        assert!(layer.template_ids.contains("app-collections"));
    }

    #[test]
    fn app_collections_empty_collections_soft_skips() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let out = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:self".to_string()),
                collections: Some(vec!["   ".to_string()]), // blank-only after trim
                replicator_addresses: None,
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect("resolve ok (soft-skip is Ok(None), not Err)");
        assert!(out.is_none(), "empty/blank app-collections set must soft-skip to None");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p defra-agent --lib p2p_reconcile::engine::tests::app_collections`
Expected: FAIL to COMPILE — the resolver currently returns `Result<PairingDesired>`, so `.expect("resolve ok").expect("some layer")` (a double unwrap through `Option`) does not type-check. This compile failure is the red state; Step 3 changes the signature to `Result<Option<..>>`.

- [ ] **Step 3: Change the signature to `Result<Option<PairingDesired>>` and honor row collections**

Rewrite `data_plane_desired_from_pairing_row` (lines 738-828). Keep the address-mismatch warn and the foreign-`agent_did` hard `bail!` (lines 754-780) exactly. Replace the collection/subscription construction (lines 782-827) with:

```rust
    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(DEFAULT_PAIRING_TEMPLATE);
    let template = resolve_template(template_id).unwrap_or_else(|| {
        tracing::warn!(
            template = template_id,
            "unknown data-plane pairing scope template; falling back to default \"{DEFAULT_PAIRING_TEMPLATE}\""
        );
        resolve_template(DEFAULT_PAIRING_TEMPLATE)
            .expect("default pairing template is in the catalog")
    });
    let peer_did = signed_endpoint.agent_did.trim();
    if data_plane_scope_requires_signed_peer_did(&template.scope) && peer_did.is_empty() {
        anyhow::bail!(
            "DataPlanePairingDesired for peer {} uses template {template_id:?} but the signed \
             PeerEndpoint has a blank agent_did",
            signed_endpoint.peer_id
        );
    }

    row.replicator_addresses = Some(vec![signed_endpoint.address.clone()]);
    let replicator_addresses = row
        .replicator_addresses
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();

    // app-collections (bring-your-own): the row supplies the collection set; the
    // template supplies only scope (Unscoped) + delivery (Replicate). A blank set
    // is malformed input — SOFT-SKIP this layer (Ok(None) + warn) rather than
    // bail, so a bad app row never fails the whole peer's desired load and stalls
    // a co-existing control pairing (reconcile_peer_tick desired_read_failed).
    let (replicator_collections, subscription_collections): (BTreeSet<String>, BTreeSet<String>) =
        if template.id == super::templates::APP_COLLECTIONS_TEMPLATE {
            let row_cols = row
                .collections
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect::<BTreeSet<_>>();
            if row_cols.is_empty() {
                tracing::warn!(
                    peer_id = %signed_endpoint.peer_id,
                    "app-collections DataPlanePairingDesired has no non-blank collections; \
                     skipping this data-plane layer (control pairing unaffected)"
                );
                return Ok(None);
            }
            // Replicate: subscribe to the same set so the merged doc is observable.
            (row_cols.clone(), row_cols)
        } else {
            // Legacy / template-driven data-plane rows: unchanged. Template
            // collections drive the replicator; no subscription (push channel).
            let cols = template
                .collections
                .iter()
                .map(|&c| c.to_string())
                .collect::<BTreeSet<_>>();
            (cols, BTreeSet::new())
        };

    let filter_collections = replicator_collections
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let replicator_filter =
        data_plane_scope_filter(&template.scope, &filter_collections, peer_did, self_did);

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
```

Note: `data_plane_scope_filter` takes `&[&str]`; the previous call passed `template.collections` (a `&[&str]`). We now pass the row-derived set, so build `filter_collections` as shown. For non-app-collections templates this is the template's own collection names (identical set), preserving prior filter behavior.

- [ ] **Step 4: Update the `load_desired` call site**

At lines 513-523, the match arm already unwraps with `?`. Because the function now returns `Result<Option<PairingDesired>>`, the `?` yields `Option<PairingDesired>` directly — which is exactly the type of `data_plane`. Change the arm from `Some(data_plane_desired_from_pairing_row(row, &entry, ...)?)` to just the flattened value:

```rust
        let data_plane = match (
            materialized_entry,
            first_row::<PairingStateRow>(&response, "DataPlanePairingDesired")?,
        ) {
            (Some(entry), Some(row)) => {
                data_plane_desired_from_pairing_row(row, &entry, self.identity.did())?
            }
            _ => None,
        };
```

(The soft-skip `Ok(None)` now flows through as `data_plane = None`, leaving `base` intact in `merge_layered_desired`.)

- [ ] **Step 4b: Update the existing in-crate test call sites the signature change breaks**

The signature is now `Result<Option<PairingDesired>>`, so every existing test that did `data_plane_desired_from_pairing_row(..).expect("..")` and then read a field now has an `Option` and won't compile. Update these call sites in the `#[cfg(test)] mod tests` — they are `.expect(..)`-then-use-field sites at approximately engine.rs **1079, 1112, 1148** (add a second unwrap):

```rust
        // before: .expect("data-plane desired")
        // after:
        .expect("data-plane desired")
        .expect("some data-plane layer")
```

The **error-expecting** site at ~engine.rs **1175** (`let error = data_plane_desired_from_pairing_row(..)` used to assert a `bail!`) is UNAFFECTED — the foreign-`agent_did` path still returns `Err`, so its `.unwrap_err()`/error assertion still holds. Do not add a second unwrap there. Let the compiler enumerate the exact sites: fix each until `cargo test -p defra-agent --lib p2p_reconcile` compiles.

- [ ] **Step 5: Run the in-crate tests + full engine tests**

Run: `cargo test -p defra-agent --lib p2p_reconcile::engine::tests::app_collections`
Expected: PASS (both resolution + soft-skip).
Run: `cargo test -p defra-agent --lib p2p_reconcile`
Expected: PASS (existing resolver/merge unit tests updated in Step 4b, all green).

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs
git commit -m "feat(p2p): honor app-collections row collections + soft-skip malformed rows (#657)"
```

---

## Task 7: Rust — reject `app-collections` on the control-plane path

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs:671-736` (`desired_from_pairing_row`) + its `load_desired` call site (lines 506-508)
- Test: in-crate `#[cfg(test)] mod tests` in `engine.rs`

**Interfaces:**
- Produces: `fn desired_from_pairing_row(row, local_did) -> Result<Option<PairingDesired>>`. A `PeerPairingDesired`/base row whose resolved template is `app-collections` → `Ok(None)` + warn (no wiring). All other templates unchanged.

- [ ] **Step 1: Write the failing in-crate unit test**

Append to the same in-crate `#[cfg(test)] mod tests` in `engine.rs`, calling the private `desired_from_pairing_row` directly (as the Task 6 tests do):

```rust
    #[test]
    fn app_collections_on_control_plane_path_soft_skips() {
        // A base/PeerPairingDesired row naming app-collections has no way to
        // supply row collections; it must resolve to no wiring (soft-skip),
        // never an empty-collection replicator.
        let out = desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:peer".to_string()),
                collections: None,
                replicator_addresses: Some(vec!["addr-b".to_string()]),
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            "did:key:self",
        )
        .expect("resolve ok");
        assert!(out.is_none(), "app-collections is invalid for a control-plane row");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent --lib p2p_reconcile::engine::tests::app_collections_on_control_plane_path_soft_skips`
Expected: FAIL to COMPILE — `desired_from_pairing_row` returns `Result<PairingDesired>`, so `.expect("resolve ok")` then `.is_none()` does not type-check; today it also resolves app-collections to empty-collection wiring.

- [ ] **Step 3: Change signature + add the reject**

First, make `APP_COLLECTIONS_TEMPLATE` usable unqualified in `engine.rs`: add it to the existing `use super::templates::{ resolve_template, scope_filter, Delivery, DidSource, PairingFilters, Scope };` import (engine.rs:18) — i.e. append `APP_COLLECTIONS_TEMPLATE`. (Task 5's `merge_layered_desired` edit may reference it as `super::templates::APP_COLLECTIONS_TEMPLATE`; either form is fine, but with the import both Tasks 5-7 can use the bare name consistently.)

In `desired_from_pairing_row` (line 671), change the return type to `Result<Option<PairingDesired>>`. After the template is resolved (around line 697), add:

```rust
    // app-collections is a data-plane-only (bring-your-own) policy: a
    // control-plane / PeerPairingDesired row cannot supply row collections, so
    // it would resolve to empty wiring yet has_wiring() would be true (addresses
    // present). Refuse to wire it. Soft-skip (Ok(None) + warn) so a raw-GraphQL
    // row cannot install an empty-collection replicator.
    if template.id == APP_COLLECTIONS_TEMPLATE {
        tracing::warn!(
            "PeerPairingDesired names the app-collections template, which is \
             data-plane-only and supplies no collections here; skipping (no wiring)"
        );
        return Ok(None);
    }
```

Wrap the final `Ok(PairingDesired { .. })` (line 729) as `Ok(Some(PairingDesired { .. }))`, and the existing `bail!` (line 708) stays a hard `Err`.

- [ ] **Step 4: Update the `load_desired` base call site**

At lines 506-508, the base currently does `.map(|row| desired_from_pairing_row(row, self.identity.did())).transpose()?` yielding `Option<PairingDesired>`. Now the closure returns `Result<Option<..>>`, so after `.transpose()?` you get `Option<Option<PairingDesired>>`; flatten it:

```rust
        let base = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
            .map(|row| desired_from_pairing_row(row, self.identity.did()))
            .transpose()?
            .flatten();
```

- [ ] **Step 4b: Update the existing in-crate test call sites the signature change breaks**

`desired_from_pairing_row` now returns `Result<Option<PairingDesired>>`. The existing in-crate tests call it at approximately engine.rs **1843, 1866, 1885, 1890, 1904, 1933, 1953, 1995, 2037**. Each `.expect("..")`-then-use-field site (including the **1890 "unknown template"** case — an unknown id falls back to `conversation`, which is now `Ok(Some(..))`, so it needs the second unwrap too) gets a second unwrap:

```rust
        // before: desired_from_pairing_row(row, "did:key:self").expect("desired")
        // after:
        desired_from_pairing_row(row, "did:key:self")
            .expect("desired")
            .expect("some desired layer")
```

The only sites that must NOT get a second unwrap are any that assert an `Err` (the blank-DID `bail!` at engine.rs:708 is unchanged). Let the compiler enumerate: fix each until `cargo test -p defra-agent --lib p2p_reconcile` compiles.

- [ ] **Step 5: Run the in-crate test + full lib tests**

Run: `cargo test -p defra-agent --lib p2p_reconcile::engine::tests::app_collections_on_control_plane_path_soft_skips`
Expected: PASS.
Run: `cargo test -p defra-agent --lib p2p_reconcile`
Expected: PASS (existing resolver tests updated in Step 4b).

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs
git commit -m "feat(p2p): reject app-collections template on the control-plane path (#657)"
```

---

## Task 8: CLI — reject `app-collections` on the generic pairing path

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/p2p/pairings.rs:173-188` (`resolve_pairing_template`)

**Interfaces:**
- Consumes: `defra_agent::agent::p2p_reconcile::templates::APP_COLLECTIONS_TEMPLATE`.
- Produces: `resolve_pairing_template("app-collections")` returns `Err` with a clear message; all other ids unchanged.

- [ ] **Step 1: Write the failing unit test**

In `pairings.rs` tests (or the crate's CLI tests), add:

```rust
    #[test]
    fn app_collections_rejected_on_generic_pairing_path() {
        let err = resolve_pairing_template("app-collections").unwrap_err().to_string();
        assert!(
            err.contains("app-collections"),
            "error must name the offending template: {err}"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-cli app_collections_rejected_on_generic_pairing_path`
Expected: FAIL — `app-collections` currently resolves successfully (it is a valid builtin).

- [ ] **Step 3: Add the guard**

In `resolve_pairing_template`, after the empty check and before the `resolve_template(template).is_some()` acceptance:

```rust
    if template == defra_agent::agent::p2p_reconcile::templates::APP_COLLECTIONS_TEMPLATE {
        anyhow::bail!(
            "the app-collections template is for DataPlanePairingDesired rows only; \
             it supplies no collections on the control-plane pairing path — write a \
             data-plane pairing with an explicit collection set instead"
        );
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p defra-agent-cli app_collections_rejected_on_generic_pairing_path`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/p2p/pairings.rs
git commit -m "feat(cli): reject app-collections on the generic pairing path (#657)"
```

---

## Task 9: E2E harness — in-process membership materialization helper

**Files:**
- Create: `crates/defra-agent/tests/e2e_triggers/app_collection_pairing_p2p_e2e.rs`
- Modify: `crates/defra-agent/tests/e2e_triggers.rs` (the harness root is this FILE, not `e2e_triggers/mod.rs`) — add `mod app_collection_pairing_p2p_e2e;` next to the existing `mod event_trigger_p2p_e2e;` (line ~11).

The integration-test binary is named `e2e_triggers` (from `e2e_triggers.rs`), so all `cargo test` runs below use `--test e2e_triggers`.

**Interfaces:**
- Consumes: `defra_agent_protocol::network_token::{NetworkRecord, MembershipRecord, EndpointRecord}` (each has `signing_payload()`); `AgentIdentity::sign`; `bs58` (sig encoding — see `network.rs:690 decode_sig` uses `bs58::decode`, so write with `bs58::encode(sig).into_string()`); `graphql::escape_graphql_string`.
- Produces: `async fn seed_materializable_peer(node, network_id, admin_identity, member_identity, member_node_id, member_address)` that writes admin-signed `AgentNetwork` + admin-signed active `NetworkMembership` + member-signed fresh `PeerEndpoint` so `GraphqlNetworkStore::load_materializable_entries` returns the member. Verified against that function.

**NOTE — spike first.** This task front-loads investigation: the exact mutation field names + sig encoding must match the decoders `network_record` / `membership_record` / `endpoint_record` (`network.rs:638-690`) and the writers `write_agent_network` / `write_membership` (`crates/defra-agent-cli/src/commands/p2p/network_admin.rs:275+`) and `endpoint.rs:112 upsert_PeerEndpoint`. Read those four before writing the helper; mirror their field/encoding shapes exactly. The verification gate (Step 3) is objective, so a wrong encoding fails fast.

- [ ] **Step 1: Read the four reference writers/decoders and the record structs**

Run (read, do not edit):
```bash
sed -n '638,700p' crates/defra-agent/src/agent/p2p_reconcile/network.rs        # decoders + decode_sig (bs58)
sed -n '275,430p' crates/defra-agent-cli/src/commands/p2p/network_admin.rs      # write_agent_network / write_membership mutation shapes
sed -n '80,130p'  crates/defra-agent/src/agent/p2p_reconcile/endpoint.rs        # upsert_PeerEndpoint mutation shape
grep -n "signing_payload\|struct NetworkRecord\|struct MembershipRecord\|struct EndpointRecord" $(rg -l network_token crates/*/src | head)
```
Record the exact field names (`network_id`, `admin_did`, `admin_sig`, `member_did`, `status`, `granted_at`, `did`, `node_id`, `address`, `updated_at`, `binding_sig`, …) and that signatures are **bs58-encoded**.

- [ ] **Step 2: Write the helper (mirror the signing pattern from `peer_registry_discovery.rs:362-410`)**

Skeleton — fill field names/encoding from Step 1; the signing shape is known-good from the conformance test:

```rust
use defra_agent_protocol::network_token::{EndpointRecord, MembershipRecord, NetworkRecord};

fn bs58_sig(sig: &[u8]) -> String { bs58::encode(sig).into_string() }

/// Write the signed AgentNetwork + active NetworkMembership + fresh PeerEndpoint
/// documents on `node` so `GraphqlNetworkStore::load_materializable_entries`
/// materializes `member_identity`'s endpoint. `admin_identity` signs the network
/// + membership; `member_identity` signs its own endpoint binding.
async fn seed_materializable_peer(
    node: &EmbeddedNode,
    network_id: &str,
    admin_identity: &dyn AgentIdentity,
    member_identity: &dyn AgentIdentity,
    member_node_id: &str,
    member_address: &str,
) {
    // 1. AgentNetwork (admin-signed) — construct NetworkRecord, sign
    //    signing_payload(), write via add_/create_AgentNetwork with admin_sig=bs58.
    // 2. NetworkMembership (admin-signed, status="active", granted_at=now).
    // 3. PeerEndpoint (member-signed, updated_at=now so it is fresh) for the member.
    //    Mirror endpoint.rs:112 upsert_PeerEndpoint field names.
    // Escape every interpolated string with escape_graphql_string; never emit [].
}
```

- [ ] **Step 3: Verify the gate materializes the peer (the objective fence)**

Add a `#[tokio::test]` that boots one node, calls `seed_materializable_peer`, then constructs `GraphqlNetworkStore::new(node, admin_identity)` and asserts `load_materializable_entries()` contains an entry with the member's `node_id`/`address`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seed_makes_peer_materializable() {
    let db = test_p2p_db("app-collection-seed").await;
    // ... register schemas / identities ...
    seed_materializable_peer(db.node.as_ref(), "net-test", &admin, &member, "peer-node", "addr").await;
    let store = defra_agent::agent::p2p_reconcile::network::GraphqlNetworkStore::new(
        db.node.clone(), Arc::new(admin_clone),
    );
    let entries = store.load_materializable_entries().await.unwrap();
    // NetworkEndpointEntry exposes { peer_id, agent_did, address } (network.rs:28);
    // the endpoint doc's node_id becomes the entry's peer_id (network.rs:141).
    assert!(entries.iter().any(|e| e.peer_id == "peer-node"),
        "seeded peer must be materializable: {entries:?}");
}
```

Run: `cargo test -p defra-agent --test e2e_triggers seed_makes_peer_materializable`
Expected: PASS. If FAIL, the sig encoding or a field name is wrong — compare against Step 1 decoders (this is the fast feedback loop; do not proceed until green).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/e2e_triggers/app_collection_pairing_p2p_e2e.rs crates/defra-agent/tests/e2e_triggers.rs
git commit -m "test(e2e): in-process membership-materialization harness for app-collection pairing (#657)"
```

---

## Task 10: E2E — reconcile-driven app-collection replication fires the trigger

**Files:**
- Modify: `crates/defra-agent/tests/e2e_triggers/app_collection_pairing_p2p_e2e.rs`

**Interfaces:**
- Consumes: `seed_materializable_peer` (Task 9); the harness helpers copied from `event_trigger_p2p_e2e.rs` (`wait_for_runtime_snapshot`, `create_task`, `create_event_trigger_with_filter`, `query_agent_requests_for_trigger`, `fetch_event_trigger`, `test_p2p_db`, `MockModelEndpoint`, `bind_default_behavior_backend`).

- [ ] **Step 1: Write the failing acceptance test**

Model on `p2p_replicated_doc_fires_event_trigger` (event_trigger_p2p_e2e.rs:401-576). Key deltas — the source collection is app-defined `@branchable`, and replication is established by **config rows + reconcile**, not `install_one_way_replicator`:

```rust
async fn register_change_proposed_schema(node: &EmbeddedNode) {
    // @branchable is REQUIRED — DefraDB only P2P-syncs branchable collections.
    let sdl = r#"
        type ChangeProposed @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(sdl).await.expect("add_schema ChangeProposed");
}

/// Write a DataPlanePairingDesired row via desired-state config (NOT add_replicator).
/// HAPPY-PATH ONLY: this helper requires a non-empty collection set. The malformed
/// (blank-only) soft-skip row in Task 10 Step 3 is written by its own inline
/// mutation, NOT through this helper — because `collections` is `[String!]!`
/// (non-null), the only valid "empty" vector is a blank-only list `["   "]`, and
/// this helper must never be asked to emit `[]` (Global Constraints).
async fn write_app_collection_pairing(
    node: &EmbeddedNode, peer_id: &str, self_did: &str, address: &str, collections: &[&str],
) {
    assert!(
        collections.iter().any(|c| !c.trim().is_empty()),
        "write_app_collection_pairing is happy-path only; pass a non-empty collection set"
    );
    let peer = escape_graphql_string(peer_id);
    let did = escape_graphql_string(self_did);
    let addr = escape_graphql_string(address);
    let cols = collections.iter()
        .map(|c| format!(r#""{}""#, escape_graphql_string(c)))
        .collect::<Vec<_>>().join(",");
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(r#"mutation {{
        create_DataPlanePairingDesired(input: {{
            peer_id: "{peer}", agent_did: "{did}",
            collections: [{cols}], replicator_addresses: ["{addr}"],
            template: "app-collections", created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }}
    }}"#);
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "create DataPlanePairingDesired: {:?}", resp.errors);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_collection_pairing_fires_event_trigger_via_reconcile() {
    // Node A (sender/writer) + Node B (agent). Both run DefraAgent::run.
    // 1. Both register ChangeProposed (@branchable).
    // 2. seed_materializable_peer on BOTH nodes: A knows B, B knows A
    //    (signed network/membership/endpoint), so data_plane_materialized_entry
    //    returns Some on both.
    // 3. On B: Task + EventTrigger watching ChangeProposed/created; WAIT for the
    //    trigger to reconcile into the active snapshot (ordering invariant).
    // 4. Co-existing control pairing: establish a subagent (control) pairing on
    //    A<->B (write the PeerPairingDesired rows) and capture its applied state.
    // 5. Write DataPlanePairingDesired (template app-collections, collections
    //    ["ChangeProposed"]) on BOTH nodes (A->B and B->A) via config. Wait for
    //    each node's reconcile generation to advance and last_reconcile_result
    //    == "applied".
    // 6. Write a ChangeProposed doc on A; poll B for exactly one AgentRequest
    //    caused_by_trigger_id, rendered content, execution_origin "scheduled";
    //    then EventTrigger last_status "fired", fire_count 1,
    //    last_fired_source_doc_id == the doc id.
    // 7. Idempotence: capture generation, force/await another sweep, assert no new
    //    ops and the control pairing's applied state is unchanged.
}
```

Fill the body by copying the assertion blocks from `event_trigger_p2p_e2e.rs:436-573` verbatim (they already assert exactly one request, lineage, `execution_origin`, rendered content, `last_status`/`fire_count`/`last_fired_source_doc_id`/`last_error`, and the apply-owned fields), swapping `ReplicatedEvent`→`ChangeProposed` and the trigger/task ids.

- [ ] **Step 2: Run to verify it fails for the RIGHT reason**

Run: `cargo test -p defra-agent --test e2e_triggers app_collection_pairing_fires_event_trigger_via_reconcile -- --nocapture`
Expected at this point in the branch: it should **PASS** if Tasks 3-7 are already merged (the reconcile path is implemented). To honor TDD, run this test on a checkout WITHOUT Tasks 5-6 (e.g. `git stash` the engine.rs resolver/merge changes) and confirm it FAILS with the diagnostic ("doc replicated but trigger did not fire" or "no replicator established"). Document the observed failure, then restore.

- [ ] **Step 3: Add the malformed-path assertion (guards the soft-skip)**

Append a second `#[tokio::test]` (or extend the first after the happy-path asserts):

```rust
// Malformed app-collections row (empty collections) must NOT stall the
// co-existing control pairing: the data-plane layer soft-skips, base survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_app_collection_row_does_not_stall_control_pairing() {
    // Boot one agent node with a control pairing already applied; write a
    // DataPlanePairingDesired with collections: ["   "] (blank-only) + template
    // app-collections; assert the next reconcile tick is NOT desired_read_failed
    // (fetch_runtime_snapshot / last_reconcile_result stays "applied") and the
    // control pairing's applied collections are unchanged.
}
```

`DataPlanePairingDesired.collections` is declared `[String!]!` (non-null) in the
SDL, so the malformed vector must be a valid **blank-only list** `["   "]` — which
the resolver trims to empty and soft-skips. Do NOT use `collections: null` (schema
rejects it) nor `[]` (Global Constraints: corrupts nillable array columns).

- [ ] **Step 4: Run the full e2e module**

Run: `cargo test -p defra-agent --test e2e_triggers app_collection_pairing`
Expected: PASS (both the happy path and the malformed-path guard).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/e2e_triggers/app_collection_pairing_p2p_e2e.rs
git commit -m "test(e2e): reconcile-driven app-collection replication fires EventTrigger (#657)"
```

---

## Task 11: Whole-workspace gate + PR

**Files:** none (verification + PR).

- [ ] **Step 1: Full package suite**

Run: `cargo test -p defra-agent`
Expected: PASS (lib + conformance + e2e_triggers all compile and pass).

- [ ] **Step 2: Workspace compile (examples/desktop/all targets)**

Run: `cargo check --workspace --all-targets`
Expected: clean. (Two resolver signatures changed to `Result<Option<..>>`; confirm no other caller broke.)

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Lean proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero sorries.

- [ ] **Step 5: Open the PR (do not merge)**

```bash
git push -u origin feat/app-collection-pairing-657
gh pr create --repo sourcenetwork/defra-agent --base main \
  --title "P2P: replicate an app-defined collection over a pairing (#657)" \
  --body "$(cat <<'EOF'
Closes #657. Follow-up epic: #660 (document-defined scope templates).

Adds an `app-collections` (Unscoped/Replicate, bring-your-own) scope template and
honors the previously-dropped `DataPlanePairingDesired.collections` field, gated on
`template == "app-collections"`. Subscription is preserved for that policy at both
the resolver and `merge_layered_desired`. Malformed rows soft-skip (never stall a
co-existing control pairing); the template is rejected on the control-plane path
(reconciler + CLI). Lean-first: catalog totality extended; behavior fenced by
conformance calling the real resolver/merge. Acceptance e2e drives replication
through reconcile (not `add_replicator`) and fires B's EventTrigger on the merged
app-defined doc.

Spec: docs/superpowers/specs/2026-07-08-app-defined-collection-pairing-design.md

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Notes for the executor

- **Resolver tests live in-crate (Tasks 6-7).** `data_plane_desired_from_pairing_row` / `desired_from_pairing_row` are private; test them from the existing `#[cfg(test)] mod tests` in `engine.rs` (which already calls the former directly), NOT from `tests/conformance/*` — integration tests compile `defra-agent` as a normal dependency and cannot see private items or `#[cfg(test)]`/unfeatured-`pub` seams. Do not add a `conformance-seams` feature. Merge-rule conformance (Task 5) is different: it uses the already-`pub` `merge_layered_desired`, so it can (and does) live in `tests/conformance/pairing_reconcile.rs`.
- **e2e target name:** integration tests under `tests/e2e_triggers/` compile as one binary; find its harness entry (`grep -rn "mod event_trigger_p2p_e2e" crates/defra-agent/tests/`) and use that `--test e2e_triggers` for the run commands above.
- **`data_plane_scope_filter` for app-collections** returns `{}` (Unscoped), so the replicator is unfiltered — correct for whole-collection sync. Confirm `apply_op`'s `InstallReplicator` passes an empty `PairingFilters` (it reads `desired.replicator_filter`).
- **Ordering invariant** is enforced by test sequencing (trigger reconciled before the data-plane rows are written), matching `event_trigger_p2p_e2e.rs`.
- **Optional (spec §4, reviewer-optional):** a diagnostic test that an unknown/non-`@branchable` collection name surfaces an error string naming the offending collection. This exercises the `apply_op` → `add_p2p_collections` error path, which needs a live node; only add it if it can be written without excessive harness cost. No runtime branchable gate either way. Not a blocking deliverable.
- **CLI writer (`upsert_data_plane`, `demo/fleet.rs`):** unchanged by this plan. It is only unsafe if invoked with `template == "app-collections"` (its `data_plane_collections_literal` expands to `[]`), which the demo never does. Full `config apply` ownership of `DataPlanePairingDesired.collections` — including a proper `--collections` writer for `app-collections` — is #607, out of scope here.
