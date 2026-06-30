# Declarative Subagent Delegation Pairing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a declarative P2P pairing topology for cross-deployment subagent delegation, so fleet config (not hand-wired test replicators) can route coordinator→host parent/bridge docs and host→coordinator child/result docs with consistent authorization and no whole-network leak.

**Architecture:** Two directional scope templates — `subagent-coordinator` (forward leg: parent `AgentRequest` by `agent_did==local`, bridge `AgentToolCall` by `spawn_target_did==peer`) and `subagent-host` (return leg: the host's owned conversation set by `agent_did==local`) — both `Delivery::Push` (no gossip subscription). A denormalized `AgentToolCall.spawn_target_did` makes the coordinator-owned bridge routable to its host. Lean-first: extend the `ScopeTemplates` model + add a cancel-propagation contract case, mirror in conformance, then implement Rust.

**Tech Stack:** Rust (`defra-agent`, `defra-agent-schemas`, `defra-agent-cli`, `defra-agent-protocol`), Lean 4 (`crates/defra-agent/proofs`), DefraDB (`defradb.rs`, GraphQL), tokio.

**Spec:** `docs/superpowers/specs/2026-06-29-declarative-subagent-pairing-design.md`

## Global Constraints

- **Lean → conformance → Rust order.** Any change to *what crosses* / *what is legal* starts in `crates/defra-agent/proofs/`, mirrors in `tests/conformance/`, then Rust. Zero `sorry`s.
- **Gate with the full package suite:** `cargo test -p defra-agent` (not `--lib`; integration tests are separate compile units).
- **Always `graphql::escape_graphql_string()`** for anything interpolated into a GraphQL string.
- **Never emit `[]` in a DefraDB mutation** — emit `null` for empty lists.
- `tracing`, never `println`.
- **Lean build:** from `crates/defra-agent/proofs`, `lake build` (proofs) and `lake env lean --run Proofs/Conformance/Contracts.lean` (contract JSON extraction).
- **New filter fields must be `@immutable`** (#1033 DAG-completeness: a doc cannot drift in/out of a filter).
- **Two new templates:** `subagent-coordinator`, `subagent-host`. **One new schema field:** `AgentToolCall.spawn_target_did`. **No new `PeerPairingDesired` field.** Role is the template id; host DID is derived (peer on coordinator, local on host).

---

## File Structure

**Lean (proofs):**
- Modify `crates/defra-agent/proofs/Proofs/ScopeTemplates/State.lean` — add `DidSource`, `CollectionRule`, `Scope.perCollection`; add the two templates to the catalog.
- Modify `crates/defra-agent/proofs/Proofs/ScopeTemplates/Derivation.lean` — extend `scopeFilter` + crossing-soundness theorems.
- Create `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/CancelPropagation.lean` — cancel contract cases.
- Modify `Proofs/Conformance/ContractCases/Types.lean`, `Proofs/Conformance/Contracts.lean`, `Proofs/Conformance/CoverageLedger.lean` — register the cancel domain.

**Conformance:**
- Modify `crates/defra-agent/tests/conformance/scope_templates.rs` — assert the two templates' filter maps.
- Modify `crates/defra-agent/tests/conformance/structure.rs` — declare the cancel-propagation home.
- Create `crates/defra-agent/tests/conformance/cancel_propagation.rs` — drive cancel across the declarative legs.
- Modify `crates/defra-agent/src/lean_vocab_test/background_transcript.rs` + `conformance_consumers.rs` — Rust mirror of the cancel case + ledger entry.

**Rust (runtime):**
- Modify `crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql` — add `spawn_target_did`.
- Modify `crates/defra-agent/src/migration.rs` — add-field patch.
- Modify `crates/defra-agent/src/tool_call_lifecycle.rs` — `new_subagent` takes + stores `spawn_target_did`.
- Modify `crates/defra-agent/src/tool_call_lifecycle/transition/native.rs` — emit `spawn_target_did` in the bridge create mutation.
- Modify `crates/defra-agent/src/hook/persistence/message_spawn.rs` + `.../orchestration.rs` — pass the target DID.
- Modify `crates/defra-agent/src/agent/p2p_reconcile/templates.rs` — `DidSource`, `CollectionRule`, `Scope::PerCollection`, two templates, `scope_filter` signature.
- Modify `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` — thread `local_did`, blank-DID guard.
- Modify `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs` — `chosen_template` skips `subagent-*`.
- Modify `crates/defra-agent-cli/src/commands/p2p/join.rs` — write the complementary role.
- Modify `crates/defra-agent/src/trigger_engine/subagent_source.rs` — claim-time target gate.
- Modify the pairing reconciler apply path — restart-stopgap `tracing::warn!`.

**E2E:**
- Modify `crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs` — drive declarative templates.

---

## Phase A — Lean model

### Task 1: Extend `ScopeTemplates` Lean model with directional per-collection scope

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ScopeTemplates/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ScopeTemplates/Derivation.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ScopeTemplates/Executable.lean` (contract, if it enumerates Scope)

**Interfaces:**
- Produces (Lean): `DidSource`, `CollectionRule`, `Scope.perCollection`, `scopeFilter (scope) (collections) (peerDid localDid : String)`, theorems `subagentCoordinator_filter_eq`, `subagentHost_filter_eq`, `subagent_filter_values_local_or_peer`.

- [ ] **Step 1: Add the per-collection scope to `State.lean`.** After the existing `Scope` inductive (currently `peerDid (field) | unscoped`), replace it with:

```lean
inductive DidSource where
  | localDid
  | peerDid
  deriving DecidableEq, Repr

structure CollectionRule where
  collection : String
  field : String
  source : DidSource
  deriving DecidableEq, Repr

inductive Scope where
  | peerDid (field : String)
  | unscoped
  | perCollection (rules : List CollectionRule)
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Add the two templates to the Lean catalog.** In the catalog definition (the `List Template` mirrored from Rust), add:

```lean
-- conversation collection set, reused for subagent-host
def subagentHostCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry"]

def subagentCoordinatorRules : List CollectionRule :=
  [ { collection := "AgentRequest",  field := "agent_did",        source := .localDid }
  , { collection := "AgentToolCall", field := "spawn_target_did", source := .peerDid  } ]

def subagentHostRules : List CollectionRule :=
  subagentHostCollections.map (fun c => { collection := c, field := "agent_did", source := .localDid })

-- appended to the catalog list:
  , { id := "subagent-coordinator", collections := ["AgentRequest", "AgentToolCall"],
      scope := .perCollection subagentCoordinatorRules, delivery := .push }
  , { id := "subagent-host", collections := subagentHostCollections,
      scope := .perCollection subagentHostRules, delivery := .push }
```

- [ ] **Step 3: Extend `scopeFilter` in `Derivation.lean` to take `localDid`.** Change the signature and add the `perCollection` arm (the produced set is `CollectionFilterKey`s, matching `PairingReconcile/State.lean`):

```lean
def scopeFilter (scope : Scope) (collections : List String)
    (peerDid localDid : String) : List CollectionFilterKey :=
  match scope with
  | .peerDid field =>
      collections.map (fun c => { collection := c, field := field, value := peerDid })
  | .unscoped => []
  | .perCollection rules =>
      rules.map (fun r =>
        { collection := r.collection, field := r.field,
          value := match r.source with | .localDid => localDid | .peerDid => peerDid })
```

- [ ] **Step 4: State and prove crossing-soundness theorems.** Add to `Derivation.lean`. The exact-filter theorems are by `rfl`/`simp [scopeFilter, ...]` after `decide` on the concrete rule lists; the value-membership theorem is the no-third-party guarantee:

```lean
theorem subagent_filter_values_local_or_peer
    (rules : List CollectionRule) (peerDid localDid : String)
    (k : CollectionFilterKey)
    (hk : k ∈ scopeFilter (.perCollection rules) [] peerDid localDid) :
    k.value = localDid ∨ k.value = peerDid := by
  simp [scopeFilter] at hk
  obtain ⟨r, _, hr⟩ := hk
  cases hsrc : r.source <;> simp [hsrc] at hr <;> subst hr <;> simp

theorem subagentCoordinator_filter_eq (peerDid localDid : String) :
    scopeFilter (.perCollection subagentCoordinatorRules) [] peerDid localDid
      = [ { collection := "AgentRequest",  field := "agent_did",        value := localDid }
        , { collection := "AgentToolCall", field := "spawn_target_did", value := peerDid  } ] := by
  simp [scopeFilter, subagentCoordinatorRules]
```

(Prove `subagentHost_filter_eq` the same way over `subagentHostRules`. Zero `sorry`; follow the proof style already used for `scopeFilter_peerDid` in this file.)

- [ ] **Step 5: Build the proofs.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: builds clean, no `sorry` warnings.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/ScopeTemplates/
git commit -m "proof(#575): directional per-collection subagent scope templates"
```

---

## Phase B — Conformance mirror (scope templates)

### Task 2: Fence the Rust catalog to the new Lean templates

**Files:**
- Modify: `crates/defra-agent/tests/conformance/scope_templates.rs`

**Interfaces:**
- Consumes (from Task 5, written next but the test is authored first to drive it): `resolve_template("subagent-coordinator"|"subagent-host")`, `scope_filter(scope, collections, peer_did, local_did)` (new 4-arg signature), `Scope::PerCollection`.

- [ ] **Step 1: Write the failing conformance test.** Append to `scope_templates.rs`, mirroring the Lean `subagentCoordinator_filter_eq` / `subagentHost_filter_eq`:

```rust
/// Mirrors Lean `subagentCoordinator_filter_eq` / `subagentHost_filter_eq` and
/// `subagent_filter_values_local_or_peer`.
#[test]
fn subagent_templates_resolve_to_directional_filters() {
    let coord = resolve_template("subagent-coordinator").expect("coordinator in catalog");
    assert_eq!(coord.delivery, Delivery::Push);
    let f = scope_filter(&coord.scope, coord.collections, "did:key:host", "did:key:coord");
    assert_eq!(f.get("AgentRequest").unwrap().field, "agent_did");
    assert_eq!(f.get("AgentRequest").unwrap().value, "did:key:coord"); // local
    assert_eq!(f.get("AgentToolCall").unwrap().field, "spawn_target_did");
    assert_eq!(f.get("AgentToolCall").unwrap().value, "did:key:host"); // peer

    let host = resolve_template("subagent-host").expect("host in catalog");
    assert_eq!(host.delivery, Delivery::Push);
    let f = scope_filter(&host.scope, host.collections, "did:key:coord", "did:key:host");
    for col in host.collections {
        let p = f.get(*col).expect("filter for every host collection");
        assert_eq!(p.field, "agent_did");
        assert_eq!(p.value, "did:key:host"); // local
    }
    // no-third-party: every value is one of the two pairing DIDs
    for p in f.values() {
        assert!(p.value == "did:key:host" || p.value == "did:key:coord");
    }
}
```

- [ ] **Step 2: Run it to confirm it fails to compile / fails.**

Run: `cargo test -p defra-agent --test conformance subagent_templates_resolve_to_directional_filters`
Expected: FAIL — `resolve_template` returns `None` / `scope_filter` arity mismatch (templates + signature land in Task 5/6).

- [ ] **Step 3: Commit the (red) test.**

```bash
git add crates/defra-agent/tests/conformance/scope_templates.rs
git commit -m "test(#575): conformance for directional subagent scope filters (red)"
```

> This test goes green in Task 6 once the Rust catalog + `scope_filter` signature land. That is the intended Lean→conformance→Rust fence.

---

## Phase C — Schema + lifecycle (`spawn_target_did`)

### Task 3: Add the `spawn_target_did` schema field + migration

**Files:**
- Modify: `crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql`
- Modify: `crates/defra-agent/src/migration.rs`

**Interfaces:**
- Produces: an `AgentToolCall.spawn_target_did` column (`String @index @immutable`) on fresh and upgraded DBs.

- [ ] **Step 1: Add the field to the SDL.** In `agent_tool_call.graphql`, directly after the `child_request_id: String @index` line, add:

```graphql
    spawn_target_did: String @index @immutable
```

- [ ] **Step 2: Add the migration patch.** In `migration.rs`, mirror the existing `ADD_AGENT_TOOL_CALL_WORKFLOW_PATCH` precedent (Kind 11 = NillableString):

```rust
const ADD_AGENT_TOOL_CALL_SPAWN_TARGET_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"spawn_target_did","Kind":11}}
]"#;
```

Wire it into the same startup migration routine that applies the workflow patch, guarded by the existing `collection_has_field()` idempotency check (apply only if `AgentToolCall` lacks `spawn_target_did`).

- [ ] **Step 3: Verify immutable enforcement.** Confirm DefraDB honors `@immutable` for a field added via patch on upgraded DBs (read `defradb.rs` schema-patch handling, or confirm via the fresh-DB SDL path which carries the directive). If patch-added fields cannot be immutable, note it: the bridge only ever *creates* `spawn_target_did` (never updates it), so functional correctness holds; record the finding in the task's commit message.

- [ ] **Step 4: Build to confirm SDL + patch compile/parse.**

Run: `cargo build -p defra-agent`
Expected: success.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql crates/defra-agent/src/migration.rs
git commit -m "feat(#575): add AgentToolCall.spawn_target_did field + migration"
```

### Task 4: Stamp `spawn_target_did` at the lifecycle layer (both producers)

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (`new_subagent`, struct field)
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition/native.rs` (`start_running` mutation)
- Modify: `crates/defra-agent/src/hook/persistence/message_spawn.rs` (call site)
- Modify: `crates/defra-agent/src/hook/persistence/orchestration.rs` (call site)
- Test: `crates/defra-agent/tests/` integration (or an existing spawn test module)

**Interfaces:**
- Consumes: the resolved target DID — `target.agent_did` (message_spawn) / `spec.agent_did` (orchestration).
- Produces: `ToolCallLifecycle.spawn_target_did: Option<String>`; bridge rows carry `spawn_target_did`.

- [ ] **Step 1: Write the failing test.** In an integration test that spawns a subagent bridge, assert the persisted bridge carries `spawn_target_did`:

```rust
#[tokio::test]
async fn spawned_bridge_carries_spawn_target_did() {
    let h = spawn_test_harness().await; // existing helper that produces a spawn bridge
    let bridge = h.fetch_latest_spawn_bridge().await;
    assert_eq!(bridge.spawn_target_did.as_deref(), Some(h.target_did.as_str()));
}
```

(If no such harness exists, drive it through `message_spawn` with a resolved target; reuse the pattern in `r5_cross_deployment.rs::setup_parent_hook_on_db` + `spawn_from_parent_hook`.)

- [ ] **Step 2: Run it; confirm failure.**

Run: `cargo test -p defra-agent spawned_bridge_carries_spawn_target_did`
Expected: FAIL — field absent / not set.

- [ ] **Step 3: Add the field + constructor param.** In `tool_call_lifecycle.rs`, add `spawn_target_did: Option<String>` to the `ToolCallLifecycle` struct, and a `spawn_target_did: String` parameter to `new_subagent(...)` (place it adjacent to `child_request_id`), storing `spawn_target_did: (!spawn_target_did.trim().is_empty()).then_some(spawn_target_did)`.

- [ ] **Step 4: Emit it in the create mutation.** In `transition/native.rs` `start_running()`, build a `spawn_target_did` fragment alongside the existing `{bridge_fields}`:

```rust
let spawn_target_field = match self.spawn_target_did.as_deref() {
    Some(did) if !did.trim().is_empty() => {
        format!(r#"spawn_target_did: "{}","#, escape_graphql_string(did))
    }
    _ => String::new(),
};
```

Insert `{spawn_target_field}` into the `create_AgentToolCall(input: {{ ... }})` block (next to `child_request_id`).

- [ ] **Step 5: Pass the target DID at both call sites.**
  - `message_spawn.rs`: the `new_subagent(...)` call — pass `target.agent_did.clone()` for the new param (the resolved target DID already used in `bridge_args`).
  - `orchestration.rs`: pass `spec.agent_did.clone()`.

- [ ] **Step 6: Run the test; confirm pass.**

Run: `cargo test -p defra-agent spawned_bridge_carries_spawn_target_did`
Expected: PASS.

- [ ] **Step 7: Add a workflow-fan-out variant of the test** asserting the orchestration producer also stamps it (mirror Step 1 against a workflow spawn). Run and confirm PASS.

- [ ] **Step 8: Commit.**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs crates/defra-agent/src/tool_call_lifecycle/transition/native.rs crates/defra-agent/src/hook/persistence/message_spawn.rs crates/defra-agent/src/hook/persistence/orchestration.rs crates/defra-agent/tests/
git commit -m "feat(#575): stamp spawn_target_did on every subagent bridge (both producers)"
```

---

## Phase D — Templates + reconciler (Rust)

### Task 5: Add the Rust `Scope::PerCollection` variant + two templates

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`

**Interfaces:**
- Produces: `DidSource`, `CollectionRule`, `Scope::PerCollection { rules }`, templates `subagent-coordinator`/`subagent-host`, `scope_filter(scope, collections, peer_did, local_did)` (4-arg).
- Consumed by: Task 2 (conformance), Task 6 (engine).

- [ ] **Step 1: Add the types.** In `templates.rs`, after the `Scope` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DidSource {
    Local,
    Peer,
}

#[derive(Debug, Clone)]
pub struct CollectionRule {
    pub collection: &'static str,
    pub field: &'static str,
    pub source: DidSource,
}
```

Add a variant to `Scope`:

```rust
pub enum Scope {
    PeerDid { field: &'static str },
    Unscoped,
    PerCollection { rules: &'static [CollectionRule] },
}
```

- [ ] **Step 2: Define the rule sets + templates.** Add the constants and two catalog entries:

```rust
const SUBAGENT_COORDINATOR_COLLECTIONS: &[&str] = &["AgentRequest", "AgentToolCall"];

const SUBAGENT_COORDINATOR_RULES: &[CollectionRule] = &[
    CollectionRule { collection: "AgentRequest",  field: "agent_did",        source: DidSource::Local },
    CollectionRule { collection: "AgentToolCall", field: "spawn_target_did", source: DidSource::Peer },
];

// subagent-host reuses CONVERSATION_COLLECTIONS; one rule per collection, all (agent_did, Local).
const SUBAGENT_HOST_RULES: &[CollectionRule] = &[
    CollectionRule { collection: "AgentRequest",      field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentResponse",     field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentMessage",      field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentToolCall",     field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentToolResult",   field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentSession",      field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "AgentConversation", field: "agent_did", source: DidSource::Local },
    CollectionRule { collection: "CompactionEntry",   field: "agent_did", source: DidSource::Local },
];
```

Append to `BUILTIN_TEMPLATES`:

```rust
    ScopeTemplate {
        id: "subagent-coordinator",
        collections: SUBAGENT_COORDINATOR_COLLECTIONS,
        scope: Scope::PerCollection { rules: SUBAGENT_COORDINATOR_RULES },
        delivery: Delivery::Push,
    },
    ScopeTemplate {
        id: "subagent-host",
        collections: CONVERSATION_COLLECTIONS,
        scope: Scope::PerCollection { rules: SUBAGENT_HOST_RULES },
        delivery: Delivery::Push,
    },
```

- [ ] **Step 3: Extend `scope_filter` to take `local_did`.** Replace the signature and add the arm:

```rust
pub fn scope_filter(
    scope: &Scope,
    collections: &[&str],
    peer_did: &str,
    local_did: &str,
) -> PairingFilters {
    match scope {
        Scope::PeerDid { field } => collections
            .iter()
            .map(|&col| {
                (col.to_string(), FilterPredicate { field: (*field).to_string(), value: peer_did.to_string() })
            })
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
        Scope::PerCollection { rules } => rules
            .iter()
            .map(|r| {
                let value = match r.source { DidSource::Local => local_did, DidSource::Peer => peer_did };
                (r.collection.to_string(), FilterPredicate { field: r.field.to_string(), value: value.to_string() })
            })
            .collect(),
    }
}
```

- [ ] **Step 4: Build (callers will break — that's Task 6).**

Run: `cargo build -p defra-agent 2>&1 | head -30`
Expected: FAIL only at `scope_filter` call sites (arity). Confirm the failures are exactly those (engine.rs + any tests), not within `templates.rs`.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/templates.rs
git commit -m "feat(#575): add subagent-coordinator/-host templates + PerCollection scope"
```

### Task 6: Thread `local_did` through the reconciler; install directional legs

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs`

**Interfaces:**
- Consumes: `scope_filter(scope, collections, peer_did, local_did)`, the reconciler's local node DID (already available — the loader "sanitizes [agent_did] to this node's DID" per the existing engine comment).
- Produces: `PairingDesired` with the directional filters; `Push` ⇒ empty subscription set.

- [ ] **Step 1: Add `local_did` to `desired_from_pairing_row`.** Change its signature to `fn desired_from_pairing_row(row: PairingStateRow, local_did: &str) -> Result<PairingDesired>` and update its caller (`load_desired`) to pass the node's local DID (locate the existing local-DID source the loader already uses for data-plane self-sanitization; thread it in).

- [ ] **Step 2: Pass `local_did` to `scope_filter` and extend the blank-DID guard.** In `desired_from_pairing_row`, change the call to:

```rust
let replicator_filter = scope_filter(&template.scope, template.collections, peer_did, local_did);
```

Extend the pre-existing blank-DID guard so a `PerCollection` template with a blank DID it needs is refused:

```rust
let needs_peer = matches!(template.scope, Scope::PeerDid { .. })
    || matches!(&template.scope, Scope::PerCollection { rules }
        if rules.iter().any(|r| matches!(r.source, DidSource::Peer)));
let needs_local = matches!(&template.scope, Scope::PerCollection { rules }
    if rules.iter().any(|r| matches!(r.source, DidSource::Local)));
if (needs_peer && peer_did.is_empty()) || (needs_local && local_did.trim().is_empty()) {
    anyhow::bail!(
        "pairing row for template {template_id:?} is missing a required DID \
         (peer blank: {}, local blank: {}); skipping peer",
        peer_did.is_empty(), local_did.trim().is_empty()
    );
}
```

(The `Delivery::Push` → empty-subscription mapping at engine.rs:639 already does the right thing for these templates — no change needed there.)

- [ ] **Step 3: Build.**

Run: `cargo build -p defra-agent`
Expected: success (all `scope_filter` callers updated).

- [ ] **Step 4: Run the Task-2 conformance test; it should now pass.**

Run: `cargo test -p defra-agent --test conformance subagent_templates_resolve_to_directional_filters`
Expected: PASS.

- [ ] **Step 5: Add a reconciler unit test for the leg shape.** In the `engine.rs` test module, assert a `subagent-coordinator` row yields a `PairingDesired` with empty `collections` (no subscription) and `replicator_filter` = `{AgentRequest: agent_did==local, AgentToolCall: spawn_target_did==peer}`; a `subagent-host` row yields `agent_did==local` across the conversation set. (Construct a `PairingStateRow` with `template: Some("subagent-coordinator")`, `agent_did: Some(host)`, and a fixed `local_did`.)

- [ ] **Step 6: Run + commit.**

Run: `cargo test -p defra-agent --test conformance scope_templates && cargo test -p defra-agent p2p_reconcile`
Expected: PASS.

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs crates/defra-agent/tests/conformance/scope_templates.rs
git commit -m "feat(#575): reconciler installs directional subagent legs (local_did threaded)"
```

---

## Phase E — Provisioning + hardening + stopgap

### Task 7: Registry exclusion of `subagent-*`

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs`

**Interfaces:**
- Produces: `chosen_template()` returns `None` for any peer offering only `subagent-*` ids.

- [ ] **Step 1: Write the failing test.** In `discovery.rs` tests:

```rust
#[test]
fn registry_skips_subagent_templates() {
    let entry = discovered_entry_with_templates(&["subagent-host", "subagent-coordinator"]);
    assert!(entry.chosen_template().is_none(), "registry must not auto-materialize subagent roles");
}
```

(Use the existing test constructor for `DiscoveredEntry`; if none, build one matching the struct.)

- [ ] **Step 2: Run; confirm failure** (today it resolves `subagent-host`).

Run: `cargo test -p defra-agent registry_skips_subagent_templates`
Expected: FAIL.

- [ ] **Step 3: Skip `subagent-*` in `chosen_template`.** Filter offered ids before resolution:

```rust
let offered: Vec<&str> = self
    .templates
    .iter()
    .map(|t| t.trim())
    .filter(|t| !t.is_empty() && !t.starts_with("subagent-"))
    .collect();
```

(Keep the rest of the function unchanged.)

- [ ] **Step 4: Run; confirm pass. Commit.**

Run: `cargo test -p defra-agent registry_skips_subagent_templates`
Expected: PASS.

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/discovery.rs
git commit -m "feat(#575): exclude subagent-* templates from registry auto-materialization"
```

### Task 8: Invite/join writes the complementary role

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/p2p/join.rs`
- Modify: `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (add `complement_subagent_template` helper near `resolve_pairing_template`)

**Interfaces:**
- Consumes: `InviteToken.template`.
- Produces: a joiner row whose template is the complement of the inviter's subagent role.

> Design note (refines spec §4a): no new `--subagent-role` CLI flag and no token-struct change. The inviter issues with `--template subagent-coordinator` (the existing flag, now valid since the template is in the catalog). `join` maps the token's subagent role to its complement before writing. Two explicit `p2p pairings set` commands (one per node) also fully provision the pair with no join change — this task covers the invite/join path for the demo.

- [ ] **Step 1: Write the failing unit test** for the complement helper in `pairings.rs` tests:

```rust
#[test]
fn complement_maps_subagent_roles_and_passes_others() {
    assert_eq!(complement_subagent_template("subagent-coordinator"), "subagent-host");
    assert_eq!(complement_subagent_template("subagent-host"), "subagent-coordinator");
    assert_eq!(complement_subagent_template("conversation"), "conversation");
}
```

- [ ] **Step 2: Run; confirm failure** (function undefined).

Run: `cargo test -p defra-agent-cli complement_maps_subagent_roles`
Expected: FAIL.

- [ ] **Step 3: Implement the helper** in `pairings.rs`:

```rust
pub(super) fn complement_subagent_template(template: &str) -> String {
    match template.trim() {
        "subagent-coordinator" => "subagent-host".to_string(),
        "subagent-host" => "subagent-coordinator".to_string(),
        other => other.to_string(),
    }
}
```

- [ ] **Step 4: Use it at the join write site.** In `join.rs`, before `write_pairing_desired(...)`, map the template:

```rust
let template = crate::commands::p2p::pairings::complement_subagent_template(&template);
```

(Place it so the joiner writes the complemented template; non-subagent templates are unchanged.)

- [ ] **Step 5: Run the helper test; confirm pass. Commit.**

Run: `cargo test -p defra-agent-cli complement_maps_subagent_roles`
Expected: PASS.

```bash
git add crates/defra-agent-cli/src/commands/p2p/pairings.rs crates/defra-agent-cli/src/commands/p2p/join.rs
git commit -m "feat(#575): join writes complementary subagent role from invite token"
```

### Task 9: Claim-time target gate on the trusted path

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/subagent_source.rs`

**Interfaces:**
- Consumes: `spawn_target_did` (now a top-level bridge field), `snapshot.local_did`.
- Produces: trusted-path materialization refused when the spawn's resolved target ≠ local.

- [ ] **Step 1: Write the failing test.** Add a `subagent_source.rs` unit/integration test: a trusted-peer bridge whose resolved target DID ≠ local DID must NOT materialize a child.

```rust
#[tokio::test]
async fn trusted_path_refuses_spawn_targeting_other_host() {
    // bridge: parent authored by a paired peer; resolved target DID = some OTHER host
    let src = subagent_source_with_paired_peer().await;
    let outcome = src.try_materialize_bridge(bridge_targeting("did:key:other-host")).await.unwrap();
    assert!(outcome.is_none(), "must not materialize a spawn addressed to a different host");
}
```

(Reuse the existing `subagent_source` test scaffolding; `bridge_targeting` sets `spawn_target_did`/`SpawnArgs.agent_did`.)

- [ ] **Step 2: Run; confirm failure** (today the trusted path skips the target check).

Run: `cargo test -p defra-agent trusted_path_refuses_spawn_targeting_other_host`
Expected: FAIL (child materializes).

- [ ] **Step 3: Add the gate.** In the trusted-paired-peer branch (`subagent_source.rs:510`), after the cross-deployment-allow check and before materialization, add:

```rust
// Even on the trusted path, only the node that owns the spawn's resolved
// target may materialize it. The §2 replicator filter should already keep a
// bridge from reaching the wrong host; this is the defense-in-depth gate.
let local_did = snapshot.local_did.trim();
let resolved_target = resolved_target_did
    .as_deref()
    .map(str::trim)
    .filter(|v| !v.is_empty());
if let Some(target) = resolved_target {
    if target != local_did {
        tracing::debug!(
            parent_request_id = %parent_request_id,
            target_did = %target,
            local_did = %local_did,
            "trusted-path spawn not addressed to this node; skipping (claim-time gate)",
        );
        return Ok(None);
    }
}
```

(`resolved_target_did` is computed at `subagent_source.rs:624`; if the gate needs it earlier, hoist that computation above the trusted branch.)

- [ ] **Step 4: Run; confirm pass. Run the existing cross-deployment tests for no regression.**

Run: `cargo test -p defra-agent subagent_source && cargo test -p defra-agent --test conformance r5`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/src/trigger_engine/subagent_source.rs
git commit -m "feat(#575): trusted-path claim gate on resolved spawn target"
```

### Task 10: Restart stopgap warning (defradb.rs#1074)

**Files:**
- Modify: the pairing reconciler apply path in `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (where `InstallReplicator`/`TeardownReplicator` ops are applied).

**Interfaces:**
- Produces: an observable `tracing::warn!` when a `subagent-*` pairing replicator op is applied.

- [ ] **Step 1: Emit the warning.** Where replicator ops are applied for a pairing whose template is `subagent-*`, log once per apply:

```rust
tracing::warn!(
    peer_id = %peer_id,
    template = %template_id,
    "applied a subagent pairing replicator change; inbound authorization may not take \
     effect on the peer until it restarts (defradb.rs#1074). Restart the target node if \
     delegation is rejected with 'not authorized for collection' / 'accepted replication direction'."
);
```

(Thread `template_id` to the apply site if not already present; gate on `template_id.starts_with("subagent-")`.)

- [ ] **Step 2: Build + commit.**

Run: `cargo build -p defra-agent`
Expected: success.

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs
git commit -m "feat(#575): observable restart-required warning for subagent pairings (defradb.rs#1074)"
```

---

## Phase F — Cancel conformance + E2E

### Task 11: Lean cancel-propagation contract case + Rust mirror + ledger

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/CancelPropagation.lean`
- Modify: `Proofs/Conformance/ContractCases/Types.lean`, `Proofs/Conformance/Contracts.lean`, `Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/src/lean_vocab_test/background_transcript.rs` (Rust struct + loader)
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs` (ledger entry)
- Modify: `crates/defra-agent/tests/conformance/structure.rs` (declare home)

**Interfaces:**
- Produces: `CancelPropagationCase` (Lean + Rust `LeanCancelPropagationCase`), emitted as contract JSON, with a declared conformance home `conformance/cancel_propagation.rs`.

- [ ] **Step 1: Define the Lean case type** in `Types.lean`:

```lean
structure CancelPropagationCase where
  name : String
  parentDeployment : String
  childDeployment : String
  parentRequestId : String
  parentToolCallId : String
  childRequestId : String
  cancelIntentWrittenOnBridge : Bool   -- coordinator writes cancel_cascade_intent_at
  bridgeReplicatesToHost : Bool        -- forward leg carries the bridge update
  childInterruptedOnHost : Bool        -- host mirror interrupts the child
  cancelAckReturnsToCoordinator : Bool -- return leg carries child terminal/ack
  deriving Repr
```

- [ ] **Step 2: Define one cross-deployment cancel case** in `CancelPropagation.lean`:

```lean
namespace Conformance.ContractCases

def cancelPropagationCases : List CancelPropagationCase :=
  [ { name := "cancel_propagates_across_declarative_subagent_legs"
    , parentDeployment := "deployment_a"
    , childDeployment := "deployment_b"
    , parentRequestId := "cancel-lean-parent"
    , parentToolCallId := "cancel-lean-tool"
    , childRequestId := "runtime_generated"
    , cancelIntentWrittenOnBridge := true
    , bridgeReplicatesToHost := true
    , childInterruptedOnHost := true
    , cancelAckReturnsToCoordinator := true } ]

end Conformance.ContractCases
```

- [ ] **Step 3: Emit it from the contract extractor** (`Contracts.lean`) alongside the R5 cases, and register the domain in `CoverageLedger.lean` (mirror the R5 registration).

- [ ] **Step 4: Build the proofs + extract contract JSON.**

Run: `cd crates/defra-agent/proofs && lake build && lake env lean --run Proofs/Conformance/Contracts.lean | head -40`
Expected: builds clean; JSON includes a `cancelPropagationCases` array.

- [ ] **Step 5: Add the Rust mirror struct** in `lean_vocab_test/background_transcript.rs` (snake_case serde, mirroring `LeanR5CrossDeploymentCase`):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCancelPropagationCase {
    pub(crate) name: String,
    pub(crate) parent_deployment: String,
    pub(crate) child_deployment: String,
    pub(crate) parent_request_id: String,
    pub(crate) parent_tool_call_id: String,
    pub(crate) child_request_id: String,
    pub(crate) cancel_intent_written_on_bridge: bool,
    pub(crate) bridge_replicates_to_host: bool,
    pub(crate) child_interrupted_on_host: bool,
    pub(crate) cancel_ack_returns_to_coordinator: bool,
}
```

Add a `lean_cancel_propagation_cases()` loader (mirror `lean_r5_cross_deployment_cases()`).

- [ ] **Step 6: Register the conformance home + ledger entry.** In `structure.rs` `model_homes()` add `"CancelPropagation" → Module("conformance/cancel_propagation.rs")`; add the consumer entry in `conformance_consumers.rs`.

- [ ] **Step 7: Build + run the structure/coverage fence.**

Run: `cargo test -p defra-agent --test conformance structure && cargo test -p defra-agent --test conformance coverage`
Expected: PASS (no undeclared model, ledger accounts for the new domain).

- [ ] **Step 8: Commit.**

```bash
git add crates/defra-agent/proofs crates/defra-agent/src/lean_vocab_test/ crates/defra-agent/tests/support/conformance_consumers.rs crates/defra-agent/tests/conformance/structure.rs
git commit -m "proof(#575): cancel-propagation contract case + Rust mirror + ledger"
```

### Task 12: Drive the cancel case across the declarative legs (conformance)

**Files:**
- Create: `crates/defra-agent/tests/conformance/cancel_propagation.rs`
- Modify: `crates/defra-agent/tests/conformance.rs` (wire the module)

**Interfaces:**
- Consumes: `lean_cancel_propagation_cases()`, the `subagent-coordinator`/`subagent-host` pairing setup, `boot_child_agent`-style helpers from `r5_cross_deployment.rs`.

- [ ] **Step 1: Write the driver test.** Mirror `r5_cross_deployment.rs`, but install the legs via `PeerPairingDesired` rows (Task 6 reconciler), spawn a background child, then cancel the parent and assert the cancel rides the legs:

```rust
pub(super) async fn cancel_propagation_cases_drive_production_interrupt() {
    let cases = lean_cancel_propagation_cases();
    assert_eq!(cases.len(), 1);
    for case in cases {
        // boot host agent + coordinator db; write subagent-host / subagent-coordinator
        // PeerPairingDesired rows on each; let the reconciler install the legs.
        // spawn background child from the coordinator hook; wait for child on host.
        // cancel the parent request -> bridge cancel_cascade_intent_at written.
        assert!(case.cancel_intent_written_on_bridge);
        // assert the bridge update replicates to host and the child is interrupted:
        let interrupted = wait_for_child_interrupted(host.node(), &child_request_id).await;
        assert!(interrupted, "{}: host child interrupts from replicated cancel", case.name);
        // assert the ack returns to the coordinator:
        let acked = wait_for_bridge_terminal(coord.node(), &session_id, &case.parent_tool_call_id).await;
        assert!(acked, "{}: cancel ack returns to coordinator", case.name);
    }
}
```

(Build the helpers from the `r5_cross_deployment.rs` toolbox: `setup_parent_hook_on_db`, `spawn_from_parent_hook`, `wait_for_*`. Replace `install_one_way_replicator` with writing the two `PeerPairingDesired` rows.)

- [ ] **Step 2: Run; iterate to green.**

Run: `cargo test -p defra-agent --test conformance cancel_propagation -- --nocapture`
Expected: PASS (may require tuning waits; flakes are defects — fix, don't shrug).

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/tests/conformance/cancel_propagation.rs crates/defra-agent/tests/conformance.rs
git commit -m "test(#575): cancel propagation across declarative subagent legs"
```

### Task 13: Reframe the live e2e to drive the declarative templates

**Files:**
- Modify: `crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs`

**Interfaces:**
- Consumes: the full stack (templates, reconciler, spawn_target_did, claim gate).

- [ ] **Step 1: Replace hand-wired replication with declarative rows.** In `live_cross_node_subagent_delegation()`, delete the two `install_one_way_replicator(...)` calls. Keep peer connectivity setup, but provision pairing via the templates — write `PeerPairingDesired{ template: "subagent-coordinator" }` on the coordinator (peer = host DID) and `PeerPairingDesired{ template: "subagent-host" }` on the host (peer = coordinator DID), then let the reconciler install the legs. (Reuse the existing `write_pairing` helper, extended to take a `template` argument, or write the rows inline with `upsert_PeerPairingDesired`.)

- [ ] **Step 2: Keep the existing completion assertion** (child runs on host, result projects back to the parent bridge).

- [ ] **Step 3: Add the no-third-party assertion.** After delegation completes, assert the host holds no `AgentRequest` owned by a DID other than the coordinator or the host:

```rust
let foreign = query_agent_requests_not_owned_by(
    host.node(), &[coordinator_did.as_str(), host_did.as_str()],
).await;
assert!(foreign.is_empty(), "host must not hold third-party AgentRequest rows: {foreign:?}");
```

- [ ] **Step 4: Run the live e2e.**

Run: `cargo test -p defra-agent --test e2e_live live_cross_node_subagent_delegation -- --nocapture`
Expected: PASS — delegation completes via the declarative templates; no third-party rows.

- [ ] **Step 5: Full gate.**

Run: `cargo test -p defra-agent`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs
git commit -m "test(#575): drive subagent delegation e2e through declarative templates"
```

---

## Self-Review notes (carried for the executor)

- **Spec coverage:** §0 Lean → Tasks 1, 11; §1 schema → Task 3; §2 templates → Tasks 1/5; §3 reconciler → Task 6; §4 lifecycle stamping (both producers) → Task 4; §4a provisioning → Tasks 7 (registry exclusion), 8 (join complement); §5 claim hardening → Task 9; §6 tests → Tasks 2, 12, 13; §7 restart stopgap → Task 10; cancel obligation → Tasks 11, 12.
- **Known investigation point (Task 6, Step 1):** the exact local-DID source in the reconciler loader must be located — the engine comment ("the loader first sanitizes [agent_did] to this node's DID") confirms it exists; wire from there.
- **Known verification point (Task 3, Step 3):** whether `@immutable` is honored for a patch-added field on upgraded DBs. Functional correctness holds regardless (bridge only creates the field), but record the finding.
- **Refinement of spec §4a:** no `--subagent-role` CLI flag (the existing `--template` flag validates `subagent-*` via `resolve_pairing_template` once the catalog has them); provisioning is two explicit `set` calls or the invite/join complement (Task 8).
