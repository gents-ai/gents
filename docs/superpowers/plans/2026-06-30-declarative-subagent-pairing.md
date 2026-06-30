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

- [ ] **Step 1b: Extend the executable scope vocabulary.** In `Executable.lean`, add a `perCollection` case to `ScopeKind`, `ScopeKind.ofScope`, `toContract`, `fromContract?`, and the round-trip theorem branches. This file is currently exhaustive over `peerDid` / `unscoped`; `lake build` should fail until the executable vocabulary knows about the new `Scope.perCollection` constructor.

- [ ] **Step 2: Define the rules, the template values, and a CONCRETE `builtinCatalog`.** The existing model is catalog-*parametric* (`abbrev Catalog := List Template`; `resolveTemplate (cat) (id)` is ∀-quantified over `cat`) — there is no concrete catalog value, so proving facts about the rule constants alone is **vacuous** as a catalog fence. Add a concrete `builtinCatalog` mirroring the Rust `BUILTIN_TEMPLATES`, so Lean fails if a subagent entry is missing or malformed. In `State.lean` (or a new `Catalog.lean` under `ScopeTemplates/`, added to the barrel):

```lean
def subagentHostCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry"]

def subagentCoordinatorRules : List CollectionRule :=
  [ { collection := "AgentRequest",  field := "agent_did",        source := .localDid }
  , { collection := "AgentToolCall", field := "spawn_target_did", source := .peerDid  } ]

def subagentHostRules : List CollectionRule :=
  subagentHostCollections.map (fun c => { collection := c, field := "agent_did", source := .localDid })

def subagentCoordinatorTemplate : Template :=
  { id := "subagent-coordinator", collections := {"AgentRequest", "AgentToolCall"},
    scope := .perCollection subagentCoordinatorRules, delivery := .push }

def subagentHostTemplate : Template :=
  { id := "subagent-host", collections := subagentHostCollections.toFinset,
    scope := .perCollection subagentHostRules, delivery := .push }

/-- Concrete catalog mirroring Rust `BUILTIN_TEMPLATES` (id + scope + delivery),
so resolution theorems are non-vacuous. Conversation/agent-config/backup/
discovery/network-control mirror the existing Rust entries; the two subagent
templates are the additions. -/
def builtinCatalog : Catalog :=
  [ conversationTemplate, agentConfigTemplate, backupTemplate,
    discoveryTemplate, networkControlTemplate,
    subagentCoordinatorTemplate, subagentHostTemplate ]
```

(Define the five pre-existing `*Template` values to match the Rust catalog if they don't already exist; their `scope`/`delivery` mirror `templates.rs`. The conformance structure-fence keeps Rust and this Lean catalog in parity.)

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

- [ ] **Step 4: State and prove crossing-soundness theorems.** Add to `Derivation.lean`. **The exact-equality theorems are the primary obligation** — they pin the precise collections, fields, AND values, so they catch wrong field names or missing collection coverage (the general value-membership lemma below does not, since `DidSource` has only two constructors and would hold for any rule set). Prove both exact-equality theorems:

```lean
theorem subagentCoordinator_filter_eq (peerDid localDid : String) :
    scopeFilter (.perCollection subagentCoordinatorRules) [] peerDid localDid
      = [ { collection := "AgentRequest",  field := "agent_did",        value := localDid }
        , { collection := "AgentToolCall", field := "spawn_target_did", value := peerDid  } ] := by
  simp [scopeFilter, subagentCoordinatorRules]

theorem subagentHost_filter_eq (peerDid localDid : String) :
    scopeFilter (.perCollection subagentHostRules) [] peerDid localDid
      = subagentHostCollections.map
          (fun c => { collection := c, field := "agent_did", value := localDid }) := by
  simp [scopeFilter, subagentHostRules]
```

These two ARE the crossing-soundness proof for the template layer: the coordinator leg carries exactly `{AgentRequest@agent_did==local, AgentToolCall@spawn_target_did==peer}` and the host leg exactly `{conversation-set@agent_did==local}` — no other collection, field, or value. Zero `sorry`; follow the proof style already used for `scopeFilter_peerDid` in this file.

- [ ] **Step 4a: Prove catalog resolution (the non-vacuous membership fence).** These make Lean fail if the `builtinCatalog` entry is missing or malformed (`decide`/`rfl` on the concrete catalog):

```lean
theorem subagentCoordinator_in_catalog :
    resolveTemplate builtinCatalog "subagent-coordinator" = some subagentCoordinatorTemplate := by
  decide

theorem subagentHost_in_catalog :
    resolveTemplate builtinCatalog "subagent-host" = some subagentHostTemplate := by
  decide
```

Then the end-to-end fence (resolution → exact filter) is a corollary chaining `subagentCoordinator_in_catalog` with `subagentCoordinator_filter_eq` (both concrete), so a missing/malformed catalog entry OR a wrong derivation breaks the build.

- [ ] **Step 4b: Add the no-third-party lemma as a supporting (secondary) corollary** — it backs the spec §0 "no third-party documents" statement but is not load-bearing on its own:

```lean
theorem subagent_filter_values_local_or_peer
    (rules : List CollectionRule) (peerDid localDid : String)
    (k : CollectionFilterKey)
    (hk : k ∈ scopeFilter (.perCollection rules) [] peerDid localDid) :
    k.value = localDid ∨ k.value = peerDid := by
  simp [scopeFilter] at hk
  obtain ⟨r, _, hr⟩ := hk
  cases hsrc : r.source <;> simp [hsrc] at hr <;> subst hr <;> simp
```

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

- [ ] **Step 1: Write the failing conformance test.** Append to `scope_templates.rs`, mirroring the Lean `subagentCoordinator_filter_eq` / `subagentHost_filter_eq`. Also extend the existing import list to include `FilterPredicate`:

```rust
/// Mirrors Lean `subagentCoordinator_filter_eq` / `subagentHost_filter_eq` and
/// the catalog-resolution theorems. Asserts EXACTNESS (no extra collections or
/// filters), not just that required keys are present.
#[test]
fn subagent_templates_resolve_to_directional_filters() {
    const CONVERSATION: &[&str] = &[
        "AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
        "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry",
    ];

    let coord = resolve_template("subagent-coordinator").expect("coordinator in catalog");
    assert_eq!(coord.delivery, Delivery::Push);
    assert_eq!(coord.collections, &["AgentRequest", "AgentToolCall"]); // exact set, exact order
    let f = scope_filter(&coord.scope, coord.collections, "did:key:host", "did:key:coord");
    assert_eq!(f.len(), 2, "coordinator filter is exactly two collections");
    assert_eq!(f["AgentRequest"], FilterPredicate { field: "agent_did".into(), value: "did:key:coord".into() });
    assert_eq!(f["AgentToolCall"], FilterPredicate { field: "spawn_target_did".into(), value: "did:key:host".into() });

    let host = resolve_template("subagent-host").expect("host in catalog");
    assert_eq!(host.delivery, Delivery::Push);
    assert_eq!(host.collections, CONVERSATION); // exact conversation set
    let f = scope_filter(&host.scope, host.collections, "did:key:coord", "did:key:host");
    assert_eq!(f.len(), CONVERSATION.len(), "host filter covers exactly the conversation set");
    for col in CONVERSATION {
        assert_eq!(f[*col], FilterPredicate { field: "agent_did".into(), value: "did:key:host".into() });
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

- [ ] **Step 3: Verify immutable enforcement — this is a GATE, not a note.** `@immutable` on the filter field is load-bearing for the filtered-replication soundness argument (#1033 DAG-completeness: a doc must not be able to drift in/out of a filtered scope after creation). It is NOT sufficient that the runtime only writes the field once — a buggy or hostile writer that mutated `spawn_target_did` could drift a bridge across host scopes, breaking the §0 crossing-soundness guarantee. So:
  - Confirm DefraDB enforces `@immutable` on the **fresh-DB SDL path** (the directive is in the SDL). Add a test that updating `spawn_target_did` after create is rejected:

```rust
#[tokio::test]
async fn spawn_target_did_is_immutable() {
    let db = test_db().await;
    let id = create_tool_call_with_spawn_target(&db, "did:key:host-a").await;
    let err = update_tool_call_spawn_target(&db, &id, "did:key:host-b").await;
    assert!(err.is_err(), "spawn_target_did must reject post-create mutation");
}
```

  - Confirm the **upgraded-DB patch path** also enforces immutability. Read `defradb.rs` schema-patch handling to see whether a patch-added field can carry the immutable flag. If the Kind-11 patch cannot mark the field immutable, **that is a blocker for this task** — resolve it before proceeding by: (a) extend the patch to set the **schema-level** immutable flag (preferred — find the field-property the SDL path sets and replicate it in the patch JSON). Enforcement must be at the **DefraDB/schema layer** (enforced on local write *and* remote merge). A defra-agent helper-path check is **not** acceptable as the fallback — a replication merge from a peer bypasses it, so it would not preserve the §0 soundness guarantee. Do not proceed to Phase D until upgraded DBs enforce it at the schema layer; record the mechanism in the commit message.

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
- Modify: `crates/defra-agent-cli/src/commands/p2p/templates.rs` (the `scope_str` match — currently exhaustive over `PeerDid`/`Unscoped`; will fail to compile without a `PerCollection` arm)

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

- [ ] **Step 4: Add the `PerCollection` arm to the CLI template listing.** In `defra-agent-cli/src/commands/p2p/templates.rs`, the `scope_str(s: &Scope)` match (lines ~37-42) is exhaustive over `PeerDid`/`Unscoped` and will fail to compile. Add:

```rust
        Scope::PerCollection { rules } => {
            // e.g. "per-collection(AgentRequest:agent_did, AgentToolCall:spawn_target_did)"
            let parts: Vec<String> = rules
                .iter()
                .map(|r| format!("{}:{}", r.collection, r.field))
                .collect();
            format!("per-collection({})", parts.join(", "))
        }
```

- [ ] **Step 5: Build (runtime callers will break — that's Task 6).**

Run: `cargo build -p defra-agent 2>&1 | head -30`
Expected: FAIL only at `scope_filter` call sites (arity). Confirm the failures are exactly those (engine.rs + any tests), not within `templates.rs` or `defra-agent-cli/src/commands/p2p/templates.rs`. Do **not** require `cargo build -p defra-agent-cli` in this step: the CLI package depends on `defra-agent`, so it cannot compile while the runtime crate intentionally has arity errors.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/templates.rs crates/defra-agent-cli/src/commands/p2p/templates.rs
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

Run: `cargo test -p defra-agent --test conformance scope_templates && cargo test -p defra-agent p2p_reconcile && cargo build -p defra-agent-cli`
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

> Design note (per spec §4a, now aligned): no new `--subagent-role` CLI flag and no token-struct change. The inviter issues with `--template subagent-coordinator` (the existing flag, valid once the template is in the catalog). `join` maps the token's subagent role to its complement before writing. Two explicit `p2p pairings set` commands (one per node) also fully provision the pair with no join change — this task covers the invite/join path for the demo.

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

- [ ] **Step 4: Use it at the join write site — ONLY for the token-derived template, not an explicit `--template` override.** Current `join` semantics make an explicit `--template` win over the token (`join.rs:66`, `:240`). The complement expresses "the token states the *inviter's* (coordinator's) role; I, the joiner, take the complementary host role." An explicit `--template subagent-host` from the operator is already *their own* role and must NOT be flipped. So apply the complement only on the branch where the template came from the token:

```rust
// token path: token carries the inviter's role; joiner takes the complement
let template = crate::commands::p2p::pairings::complement_subagent_template(&token.template);
// explicit --template override path: use as-is (operator stated their own role)
let template = args.template.clone(); // unchanged — no complement
```

(Locate the existing token-vs-override branch at `join.rs:66`/`:240` and apply `complement_subagent_template` only on the token branch.)

- [ ] **Step 4b: Test both branches.** Add join tests: (a) token `subagent-coordinator`, no `--template` → joiner row is `subagent-host`; (b) explicit `--template subagent-host` override → joiner row is `subagent-host` (NOT flipped to coordinator); (c) token `conversation`, no override → `conversation`.

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

- [ ] **Step 1: Write the failing tests.** Add `subagent_source.rs` tests keyed on the **top-level `spawn_target_did`**: (a) a trusted-peer bridge whose `spawn_target_did` ≠ local DID must NOT materialize; (b) a bridge whose top-level `spawn_target_did` and in-`args` target disagree must NOT materialize.

```rust
#[tokio::test]
async fn trusted_path_refuses_spawn_targeting_other_host() {
    let src = subagent_source_with_paired_peer().await; // local_did = did:key:me
    // top-level spawn_target_did = a DIFFERENT host, args agree
    let bridge = bridge_with(/*spawn_target_did*/ "did:key:other-host", /*args target*/ "did:key:other-host");
    assert!(src.try_materialize_bridge(bridge).await.unwrap().is_none());
}

#[tokio::test]
async fn trusted_path_refuses_field_args_mismatch() {
    let src = subagent_source_with_paired_peer().await; // local_did = did:key:me
    // top-level field routed here (== local), but args claim a different target
    let bridge = bridge_with(/*spawn_target_did*/ "did:key:me", /*args target*/ "did:key:other-host");
    assert!(src.try_materialize_bridge(bridge).await.unwrap().is_none());
}
```

(Reuse the existing `subagent_source` test scaffolding; `bridge_with` sets the top-level `spawn_target_did` and the `SpawnArgs.agent_did` in `args` independently.)

- [ ] **Step 2: Run; confirm failure** (today the trusted path skips the target check).

Run: `cargo test -p defra-agent trusted_path_refuses_spawn_targeting_other_host`
Expected: FAIL (child materializes).

- [ ] **Step 3a: Ensure the bridge row query selects `spawn_target_did`.** The gate must key on the **top-level `spawn_target_did`** — the field the replication filter actually trusted to route the bridge here — not only the `SpawnArgs.agent_did` parsed from `args`. Add `spawn_target_did` to the `AgentToolCall` selection in the bridge-row query/struct that `build_intent_for_tool_call_doc` loads (`subagent_source.rs`), exposing it as `row.spawn_target_did: Option<String>`.

- [ ] **Step 3b: Add the gate.** In the trusted-paired-peer branch (`subagent_source.rs:510`), after the cross-deployment-allow check and before materialization:

```rust
// Even on the trusted path, only the node that owns the spawn's target may
// materialize it. The §2 replicator filter keys on the TOP-LEVEL
// spawn_target_did, so the gate must validate THAT field (what routing
// trusted) — and reject a bridge whose top-level field and in-args target
// disagree (a forged/inconsistent bridge).
let local_did = snapshot.local_did.trim();
let bridge_target = row
    .spawn_target_did
    .as_deref()
    .map(str::trim)
    .filter(|v| !v.is_empty());
match bridge_target {
    Some(target) if target == local_did => { /* addressed to us: proceed */ }
    Some(target) => {
        tracing::debug!(
            parent_request_id = %parent_request_id,
            spawn_target_did = %target,
            local_did = %local_did,
            "trusted-path spawn not addressed to this node; skipping (claim-time gate)",
        );
        return Ok(None);
    }
    None => {
        tracing::warn!(
            parent_request_id = %parent_request_id,
            "trusted-path bridge missing spawn_target_did; refusing to materialize",
        );
        return Ok(None);
    }
}
// Reject inconsistency between the routed field and the in-args target.
if let Some(args_target) = resolved_target_did.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
    if Some(args_target) != bridge_target {
        tracing::warn!(
            parent_request_id = %parent_request_id,
            spawn_target_did = ?bridge_target,
            args_agent_did = %args_target,
            "bridge spawn_target_did disagrees with args target; refusing to materialize",
        );
        return Ok(None);
    }
}
```

(`resolved_target_did` is computed at `subagent_source.rs:624`; hoist that computation above the trusted branch so the mismatch check can use it.)

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
        // write subagent-host / subagent-coordinator PeerPairingDesired rows on each
        // node, each carrying the PEER'S listen multiaddr in replicator_addresses
        // (never []; Task 13 Step 1), then let both reconcilers install their legs
        // (first install calls add_replicator -> live auth update on both sides).
        // DETERMINISTIC WAIT (no sleeps): before spawning, poll until both legs are
        // installed on both nodes — wait_for_replicator_installed(coord, host_addr)
        // and wait_for_replicator_installed(host, coord_addr) — same helper as Task 13,
        // since the reconciler is sweep-driven (engine.rs:23) and a bare write races it.
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

- [ ] **Step 1: Replace hand-wired replication with declarative rows that carry the peer's listen address.** In `live_cross_node_subagent_delegation()`, delete the two `install_one_way_replicator(...)` calls. Provision pairing via the templates — write `PeerPairingDesired{ template: "subagent-coordinator" }` on the coordinator (peer = host DID) and `{ template: "subagent-host" }` on the host (peer = coordinator DID). **Two non-obvious requirements:**
  - **`replicator_addresses` MUST be the peer's actual listen multiaddr, never `[]`.** The reconciler only emits `InstallReplicator` for desired addresses (`diff.rs:96`), and the existing `write_pairing` helper writes `replicator_addresses: []` (`subagent_delegation_live.rs:992`) — which both installs nothing *and* trips the DefraDB empty-list sharp edge (`[]` types as `JsonArray`). Resolve each node's listen address first (the e2e already waits for listen addresses for the old `install_one_way_replicator`), and write it into the row: coordinator's row carries the **host's** address; host's row carries the **coordinator's** address. Emit `null` (not `[]`) only if a list is genuinely empty — here it never is.
  - **Auth-gate freshness (defradb.rs#1074):** `test_p2p_db` builds the P2P `EmbeddedNode` (where the `Controlled` auth gate lives) *before* any row can be written, with `load_persisted_collections: false` (`tests/support/mod.rs:60-69`). So there is no "write before node boot" option, and no boot rehydration to lean on. In this harness the live gate is populated entirely by the reconciler's **first install** (`add_replicator` → `register_replicator_access`, which live-updates auth on the installing node) — first install is never the #1074 no-op (that bug bites only on *changes* where `get_replicators`'s persisted view already matches desired). So writing rows + letting both reconcilers install their legs yields a correct live gate on both sides. **If** the test is ever observed flaky from stale auth, the deterministic fix is to **restart the target `EmbeddedNode` after the rows are written** (boot rehydration), not a sleep. Do NOT claim "before agent boot" avoids #1074 — it doesn't.

- [ ] **Step 1b: Deterministically confirm both legs are installed before spawning (no sleeps).** The reconciler is sweep-driven (`engine.rs:23` `PAIRING_SWEEP_INTERVAL`) plus on-`Update`; a bare spawn races first install. Poll until both outbound legs are installed on both nodes:

```rust
wait_for_replicator_installed(coord.node(), /*to*/ host_addr.as_str()).await; // coordinator -> host
wait_for_replicator_installed(host.node(), /*to*/ coord_addr.as_str()).await;  // host -> coordinator
```

Add `wait_for_replicator_installed` as a bounded-poll helper. **Flakes here are defects** — fix the wait, never add a bare sleep.

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
- **Known verification point (Task 3, Step 3):** whether `@immutable` is honored for a patch-added field on upgraded DBs. This is a **gate** (DAG-completeness), not a soft note — "the runtime only writes it once" is NOT sufficient, because immutability must also hold against remote merges and any other writer. The fallback enforcement must be **DefraDB/schema-level** (which DefraDB enforces on local write *and* remote merge), not a defra-agent helper-path check (which a replication merge bypasses). Blocker for Phase D if not enforced.
- **Spec §4a alignment:** spec and plan now agree — no `--subagent-role` CLI flag (the existing `--template` flag validates `subagent-*` via `resolve_pairing_template` once the catalog has them); provisioning is two explicit `set` calls or the invite/join complement (Task 8). Spec §4a updated to match.
- **Immutability is a gate (Task 3), not a note:** upgraded DBs must enforce `@immutable` on `spawn_target_did` (filtered-replication DAG-completeness); blocker if the patch path can't, with two listed remedies.
- **Deterministic reconcile waits (Tasks 12, 13):** tests poll for applied legs on both nodes before exercising behavior; the reconciler is sweep-driven, so write-then-act races. No bare sleeps.
- **Claim gate keys on the top-level `spawn_target_did` (Task 9)** — the field replication trusted — and rejects field/args mismatch.
