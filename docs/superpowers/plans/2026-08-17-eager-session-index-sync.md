# Eager Session-Index Sync (#1141) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A freshly paired client renders the full session list (AgentConversation + AgentSession) with no transcript-plane replication and no env gate.

**Architecture:** Follow the foundation flow: first repair the small pre-existing Lean/Rust catalog drift and extend the Lean scope-template catalog, then mirror the new template in Rust and pin conformance. Finally, replace the env-gated bulk pull with a supervisor-owned concurrent request for exactly the two index collections. The supervisor dispatches after startup, for a new peer, and after reconnect/repair without blocking launch or add-peer. BranchableSync is node-global and unfiltered, so the trusted full client sees all sessions rather than only its requester slice. Make the pairing row request the `machine` template explicitly.

**Tech Stack:** Rust (gents workspace), Lean 4 (crates/gents/proofs), DefraDB embedded node (defradb.rs pinned rev).

**Spec:** `docs/superpowers/specs/2026-08-17-mobile-session-sync-design.md`

## Review Clarifications

- The Lean catalog currently predates `DatastoreToolSurface` in the shared config plane and `PersonaConfigRequest` in `machine`. Task 1 repairs that drift before adding `client-index`; otherwise the claim that Lean and Rust mirror one another would remain false.
- #1141 registers `client-index`, but does not select it for the desktop pairing row. The eager unfiltered DAG pull is what supplies the complete historical index in this issue; the pairing row remains `machine` so the existing requester-authored control plane keeps working. Wiring a pairing to `client-index` is outside this issue.
- In this plan, “no transcript-plane replication” means the eager historical catch-up requests only the two index collections. The existing `machine` grant still has requester-scoped transcript routes, and the desktop still subscribes to new collection heads. If the acceptance criterion instead means *zero transcript transport of any kind*, the approved design and Task 5 conflict and must be redesigned before implementation.
- The pinned IROH adapter returns when it dispatches per-peer sends; it does not expose merge completion. Logs and return values therefore say “requested,” never “complete.” Supervisor lifecycle events provide re-request opportunities, while exact progress remains #1144/upstream work.
- BranchableSync checks collection-level access but does not apply the `client-index` requester predicate. Cross-requester session-card visibility is an explicit trust decision for the current full-client product, not a tenant-safe property proved by the template.
- Existing template-absent pairings previously resolved to `conversation`. Writing `machine` changes their effective filter and causes one teardown/reinstall/full replay on upgrade; this migration cost is explicitly accepted.
- `GENTS_DESKTOP_SYNC_BRANCHABLE_ON_PAIR` is removed with no 16-collection replacement. Transcript history remains lazy until #1142.
- “Concurrent” is an acceptance detail: the fixed two collection request futures run together with `tokio::try_join!`.
- The preferred follow-up is a paginated, cursor-based index protocol with bounded document-ID pages, explicit lineage policy, observable progress, and resumability.

## Global Constraints

- Zero `sorry`s in Lean; catalog counts are exact on both sides (Rust test asserts the count; Lean `builtinCatalog` is a literal list).
- Always `graphql::escape_graphql_string()` for anything interpolated into GraphQL.
- Never emit `[]` in a DefraDB mutation — emit `null` for empty lists.
- Gate with `cargo test -p gents` (never `--lib`), and `cargo check --workspace --all-targets` before push.
- `tracing`, never `println`.
- Branch: `agent/ui-mobile-debug`. Reference issue #1141 in commits.
- Preserve unrelated worktree changes, including the existing generated iOS `Info.plist` modification.

---

### Task 1: Lean — add the `client-index` template to the model

**Files:**
- Modify: `crates/gents/proofs/Proofs/ScopeTemplates/State.lean`
- Modify: `crates/gents/proofs/Proofs/ScopeTemplates/Derivation.lean`

**Interfaces:**
- Repairs: the Lean catalog includes the already-shipped Rust entries `DatastoreToolSurface` (config/conversation/machine/discovery) and `PersonaConfigRequest` (machine, requester-scoped).
- Produces: `clientIndexCollections : List String`, `clientIndexRules : List CollectionRule`, `clientIndexTemplate : Template`, and `builtinCatalog` now ending `…, appCollectionsTemplate, clientIndexTemplate]`. Task 2's Rust catalog must match this order and content exactly.

- [ ] **Step 1: Repair the existing catalog mirror in `State.lean`**

Before adding the new template, bring the Lean collection literals up to the current Rust catalog:

```lean
def agentConfigCollections : List String :=
  ["AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill", "DatastoreToolSurface"]

def machineCollections : List String :=
  conversationCollections ++ ["PersonaConfigRequest", "AgentDirectoryEntry"]

def discoveryCollections : List String :=
  ["AgentNetwork", "NetworkMembership", "PeerEndpoint", "NetworkJoinRequest",
   "AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill", "DatastoreToolSurface"]
```

Add the missing machine rule between `conversationRules` and the directory rule:

```lean
def machineRules : List CollectionRule :=
  conversationRules ++
    [ { collection := "PersonaConfigRequest", field := "requester_did", source := .peerDid }
    , { collection := "AgentDirectoryEntry", field := "source_did", source := .homeDid } ]
```

Update the existing `machine_filter_eq` theorem in `Derivation.lean` to expect the requester-scoped `PersonaConfigRequest` predicate before the home-scoped directory predicate. Update the collection-set theorem/name so its statement includes all three filtered parts (conversation transcript, persona request, directory), rather than silently omitting the already-shipped persona rule.

- [ ] **Step 2: Add collections, rules, and template to `State.lean`**

In `crates/gents/proofs/Proofs/ScopeTemplates/State.lean`, directly after the `subagentHostCollections` definition, add:

```lean
def clientIndexCollections : List String :=
  ["AgentConversation", "AgentSession"]
```

Directly after the `subagentHostRules` definition, add:

```lean
def clientIndexRules : List CollectionRule :=
  [ { collection := "AgentConversation", field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",      field := "requester_did", source := .peerDid } ]
```

Directly after the `appCollectionsTemplate` definition, add:

```lean
def clientIndexTemplate : Template :=
  { id := "client-index"
  , collections := clientIndexCollections.toFinset
  , scope := .perCollection clientIndexRules
  , delivery := .push }
```

Then change `builtinCatalog` from:

```lean
def builtinCatalog : Catalog :=
  [ conversationTemplate
  , machineTemplate
  , agentConfigTemplate
  , backupTemplate
  , discoveryTemplate
  , networkControlTemplate
  , subagentCoordinatorTemplate
  , subagentHostTemplate
  , appCollectionsTemplate ]
```

to:

```lean
def builtinCatalog : Catalog :=
  [ conversationTemplate
  , machineTemplate
  , agentConfigTemplate
  , backupTemplate
  , discoveryTemplate
  , networkControlTemplate
  , subagentCoordinatorTemplate
  , subagentHostTemplate
  , appCollectionsTemplate
  , clientIndexTemplate ]
```

- [ ] **Step 3: Add theorems to `Derivation.lean`**

Directly after the `appCollections_unscoped_no_filter` theorem at the end of the catalog-membership block, add:

```lean
theorem clientIndex_in_catalog :
    resolveTemplate builtinCatalog "client-index" = some clientIndexTemplate := by
  decide

theorem clientIndex_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection clientIndexRules) [] peerDid localDid
      = [ { collection := "AgentConversation", field := "requester_did", value := peerDid }
        , { collection := "AgentSession",      field := "requester_did", value := peerDid } ] := by
  simp [scopeFilter, clientIndexRules]

theorem clientIndex_filters_requester_lineage (peerDid localDid : Did) :
    (scopeFilter clientIndexTemplate.scope [] peerDid localDid).all
      (fun k => k.value = peerDid ∧ k.field = "requester_did") = true := by
  simp [scopeFilter, clientIndexTemplate, clientIndexRules]

theorem clientIndex_covers_exactly_literal_index_collections :
    clientIndexTemplate.collections =
      ["AgentConversation", "AgentSession"].toFinset := by
  decide
```

If any existing theorem in `Derivation.lean` enumerates the whole catalog (e.g. a totality or count theorem proved by `decide`), re-run it unchanged — `decide` re-evaluates against the new 10-entry list; fix only if the statement hard-codes `9`.

- [ ] **Step 4: Build the proofs**

Run from `crates/gents/proofs/`:
```bash
lake build
```
Expected: success, zero `sorry`s. If `decide` times out on `clientIndex_in_catalog`, mirror whichever proof tactic `machine_in_catalog` uses (it is `decide` today).

- [ ] **Step 5: Commit**

```bash
git add crates/gents/proofs/Proofs/ScopeTemplates/State.lean crates/gents/proofs/Proofs/ScopeTemplates/Derivation.lean
git commit -m "proofs: align and extend the scope-template catalog (#1141)"
```

---

### Task 2: Rust — mirror the template in the catalog

**Files:**
- Modify: `crates/gents/src/agent/p2p_reconcile/templates.rs`

**Interfaces:**
- Consumes: the Lean catalog from Task 1 (order and content must match).
- Produces: `pub const CLIENT_INDEX_TEMPLATE: &str = "client-index"` and a `BUILTIN_TEMPLATES` entry; `resolve_template("client-index")` returns it. Task 3 pins that catalog contract; Task 4's node-global bootstrap pull is deliberately independent of template selection.

- [ ] **Step 1: Write the failing unit test**

In the `#[cfg(test)] mod tests` at the bottom of `templates.rs`, next to `builtin_template_count_is_nine`, replace that test and add one:

```rust
    #[test]
    fn builtin_template_count_is_ten() {
        assert_eq!(builtin_templates().len(), 10);
    }

    #[test]
    fn client_index_is_requester_scoped_push_of_the_session_index() {
        let t = resolve_template(CLIENT_INDEX_TEMPLATE).unwrap();
        assert_eq!(t.delivery, Delivery::Push);
        assert!(matches!(t.scope, Scope::PerCollection(_)));
        assert_eq!(t.collections, &["AgentConversation", "AgentSession"]);
        let filter = scope_filter(&t.scope, t.collections, "did:key:phone", "did:key:home");
        assert_eq!(filter.len(), 2);
        for col in ["AgentConversation", "AgentSession"] {
            let pred = filter.get(col).expect("indexed collection is filtered");
            assert_eq!(pred.field, "requester_did");
            assert_eq!(pred.value, "did:key:phone");
        }
    }
```

Delete the old `builtin_template_count_is_nine` test.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p gents --lib agent::p2p_reconcile::templates
```
Expected: FAIL — `CLIENT_INDEX_TEMPLATE` not found (compile error). (`--lib` is acceptable for the inner red/green loop only; the task gate in Step 5 uses the full suite.)

- [ ] **Step 3: Add the template**

In `templates.rs`, next to the other template-id constants (`APP_COLLECTIONS_TEMPLATE`, `MACHINE_TEMPLATE`):

```rust
pub const CLIENT_INDEX_TEMPLATE: &str = "client-index";
```

After `CONVERSATION_RULES` (or adjacent rule constants), add:

```rust
/// Requester-scoped session-index grant: conversation cards and session
/// lifecycle rows only. #1141 registers this catalog contract; the desktop's
/// complete historical index comes from an unfiltered branchable pull, and
/// selecting this template for a pairing remains a separate policy decision.
const CLIENT_INDEX_COLLECTIONS: &[&str] = &["AgentConversation", "AgentSession"];

const CLIENT_INDEX_RULES: &[CollectionRule] = &[
    CollectionRule {
        collection: "AgentConversation",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSession",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
];
```

Append to `BUILTIN_TEMPLATES` (after the `app-collections` entry, matching the Lean order):

```rust
    ScopeTemplate {
        id: CLIENT_INDEX_TEMPLATE,
        collections: CLIENT_INDEX_COLLECTIONS,
        scope: Scope::PerCollection(CLIENT_INDEX_RULES),
        delivery: Delivery::Push,
    },
```

- [ ] **Step 4: Run the unit tests**

```bash
cargo test -p gents --lib agent::p2p_reconcile::templates
```
Expected: PASS, including the two new tests.

- [ ] **Step 5: Run the conformance + fence gate**

```bash
cargo test -p gents scope_templates
cargo test -p gents embedded_all_builtin_template_filters_pass_replicator_validation
```
Expected: PASS. The second test runs every catalog template's filter through real `add_replicator` validation — `requester_did` is `@immutable` on both index collections, so `client-index` must pass. If it fails on immutability, STOP: the schema premise is wrong and the spec needs revisiting.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/agent/p2p_reconcile/templates.rs
git commit -m "feat(p2p): client-index scope template — requester-scoped session index (#1141)"
```

---

### Task 3: Conformance — pin the template's contract

**Files:**
- Modify: `crates/gents/tests/conformance/scope_templates.rs`

**Interfaces:**
- Consumes: `resolve_template("client-index")`, `scope_filter` from Task 2.

- [ ] **Step 1: Write the conformance test**

Add to `crates/gents/tests/conformance/scope_templates.rs`, following the style of `conversation_scope_excludes_another_requester_on_the_same_agent`:

```rust
/// Mirrors Lean `clientIndex_filter_eq` / `clientIndex_filters_requester_lineage`:
/// the index slice is exactly the two session-index collections, both scoped
/// to the peer's requester lineage, and nothing else rides along.
#[test]
fn client_index_scope_is_exactly_the_requester_scoped_session_index() {
    let t = resolve_template("client-index").expect("client-index in catalog");
    assert_eq!(t.delivery, Delivery::Push);
    assert_eq!(t.collections, &["AgentConversation", "AgentSession"]);

    let filter = scope_filter(&t.scope, t.collections, "did:key:phone", "did:key:home");
    assert_eq!(filter.len(), 2);
    for col in ["AgentConversation", "AgentSession"] {
        let pred = filter.get(col).expect("collection filtered");
        assert_eq!(pred.field, "requester_did");
        assert_eq!(pred.value, "did:key:phone");
    }

    // Another requester's rows do not match this peer's slice.
    let other = scope_filter(&t.scope, t.collections, "did:key:laptop", "did:key:home");
    assert_ne!(
        filter.get("AgentSession").unwrap().value,
        other.get("AgentSession").unwrap().value
    );
}
```

Match the existing imports at the top of the file (`resolve_template`, `scope_filter`, `Delivery`, `Scope` are already imported for the sibling tests; extend the `use` list only if the compiler asks).

- [ ] **Step 2: Run it**

```bash
cargo test -p gents client_index_scope_is_exactly_the_requester_scoped_session_index
```
Expected: PASS.

- [ ] **Step 3: Confirm the coverage ledger remains area-level**

```bash
rg -n "scope.?templates|ScopeTemplates" crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean
```
Expected: no per-template entries. The ledger tracks lifecycle/consumer surfaces, not individual static catalog members, so no ledger edit is required.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests/conformance/scope_templates.rs
git commit -m "test(conformance): pin client-index template contract (#1141)"
```

---

### Task 4: Supervisor — resilient, non-blocking index requests

**Files:**
- Modify: `crates/gents-desktop-core/src/client/schema.rs`
- Modify: `crates/gents-desktop-core/src/client/core/bootstrap.rs`
- Modify: `crates/gents-desktop-core/src/client/core/supervisor.rs`
- Modify: `crates/gents-desktop-core/src/client/core/writes.rs`
- Modify: `crates/gents-desktop-core/src/client/core/tests.rs`

**Interfaces:**
- Produces: public Rust `CLIENT_INDEX_COLLECTIONS`; `index_collection_names()` reuses it; `request_index_sync(node, p2p) -> Result<Vec<String>>` validates that a peer is connected and concurrently dispatches exactly two collection requests; the supervisor tracks which saved-peer connection epochs have received a request.
- Consumes: `p2p_sync_branchable_collection(p2p, &collection_id)` (existing, `core/p2p_ops.rs`).

- [ ] **Step 1: Keep one Rust source of truth for the index list**

Export `CLIENT_INDEX_COLLECTIONS` from the Rust template catalog and return it from `schema::index_collection_names()`. Keep the existing test that pins the literal names and verifies both are branchable. Lean remains the formal mirror, with a theorem against the literal collection names rather than against the definition used to construct the template.

- [ ] **Step 2: Remove the old inline paths and ineffective retry ladder**

In `bootstrap.rs`:

- remove the old env gate and 16-collection helper;
- remove index requests from `bootstrap_saved_peers` and `ClientCore::add_peer` so neither critical path waits on them;
- replace the retrying helper with `request_index_sync`, which refuses zero connected peers, resolves the two collection IDs once, and dispatches the two requests with `tokio::try_join!` once; and
- preserve `bootstrap_saved_peers` errors in `ClientCore::bootstrap_errors` rather than discarding them.

- [ ] **Step 3: Let the supervisor own request opportunities**

Maintain a small in-memory set of saved peer IDs whose current healthy connection epoch has received an index request. The supervisor:

1. begins with the set empty, causing one request after startup for healthy saved peers;
2. requests again when a newly saved healthy peer is absent from the set;
3. removes a peer from the set whenever that peer enters repair, causing a fresh request after successful reconnect; and
4. records request failures in `ClientPeerStatus.last_error`, where the UI and `saved_peer_needs_repair` already look. A successful dispatch logs that merges continue asynchronously; it never claims completion.

- [ ] **Step 4: Add focused regression coverage**

Extend `RecordingP2P` to record collection IDs. Pin that the direct request targets exactly the two real index collection IDs. Add a supervisor-state regression covering initial request, steady-state dedupe, newly saved peer, reconnect re-arm, and visible failure when transport state reports no connected peers. Avoid timing assertions.

- [ ] **Step 5: Run the tests**

```bash
cargo test -p gents-desktop-core index_collections_are
cargo test -p gents-desktop-core index_sync_request_targets_exactly
cargo test -p gents-desktop-core supervisor_requests_index
cargo test -p gents-desktop-core
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gents-desktop-core/src/client/schema.rs crates/gents-desktop-core/src/client/core/bootstrap.rs crates/gents-desktop-core/src/client/core/supervisor.rs crates/gents-desktop-core/src/client/core/writes.rs crates/gents-desktop-core/src/client/core/tests.rs
git commit -m "fix(desktop-core): retry session-index requests from the P2P supervisor (#1141)"
```

---

### Task 5: Pairing row — explicit template, no dead collections list

**Files:**
- Modify: `crates/gents-desktop-core/src/client/core/bootstrap.rs` (`write_peer_pairing_desired`)
- Modify: `crates/gents-desktop-core/src/client/core/tests.rs`

**Interfaces:**
- Consumes: existing `gents::agent::p2p_reconcile::templates::MACHINE_TEMPLATE` constant, avoiding another copy of the catalog id string.
- Produces: `PeerPairingDesired` rows carrying `template: "machine"`; the server's `desired_from_pairing_row` stops falling back to `DEFAULT_PAIRING_TEMPLATE = "conversation"`.

- [ ] **Step 1: Modify the mutations**

In `write_peer_pairing_desired`, delete the `collections` local (the `subscribed_collection_names()` join) — the server never reads `row.collections` for non-`app-collections` templates; it derives from `template.collections`.

Import `gents::agent::p2p_reconcile::templates::MACHINE_TEMPLATE`, escape it with the other interpolated values, and bind it as `template` before building either mutation.

Replace the update mutation body:

```rust
        format!(
            r#"mutation {{ update_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                input: {{
                    collections: null,
                    template: "{template}",
                    replicator_addresses: ["{replicator_addr}"],
                    agent_did: "{agent_did}",
                    created_at: "{created_at}",
                    profiles: null,
                    updated_at: "{now}"
                }}
            ) {{ _docID }} }}"#
        )
```

and the create mutation body:

```rust
        format!(
            r#"mutation {{ create_PeerPairingDesired(input: {{
                peer_id: "{peer_id}",
                agent_did: "{agent_did}",
                collections: null,
                template: "{template}",
                replicator_addresses: ["{replicator_addr}"],
                profiles: null,
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }} }}"#
        )
```

(`collections: null`, never `[]` — the empty list literal corrupts nillable array columns. `machine` rather than `client-index`: the paired app is a full client that later rides `PersonaConfigRequest`/`SessionHydrationRequest` over this same pairing; `client-index` exists for peers that should see only the index.)

- [ ] **Step 2: Pin both create and update behavior**

Reuse the existing `core/tests.rs` fixture that calls `write_peer_pairing_desired`: query `collections` and `template`, then assert `collections` is null and `template == "machine"`. Call the writer a second time with a changed requester address and assert there is still one row with the updated address, `collections: null`, and `template: "machine"`. This exercises both mutation branches without adding another embedded-node fixture.

- [ ] **Step 3: Compile and test**

```bash
cargo test -p gents-desktop-core remove_peer_retains_saved_deployment_when_p2p_cleanup_fails
cargo test -p gents-desktop-core
```
Expected: PASS. If a fixture asserts the row's `collections` contents, update it to expect null/absent and `template == "machine"`.

- [ ] **Step 4: Commit**

```bash
git add crates/gents-desktop-core/src/client/core/bootstrap.rs crates/gents-desktop-core/src/client/core/tests.rs
git commit -m "fix(desktop-core): pairing row requests the machine template explicitly, drops dead collections list (#1141)"
```

---

### Task 6: Full gates + live verification

**Files:** none new.

- [ ] **Step 1: Format and desktop package suite**

```bash
cargo fmt --all -- --check
cargo test -p gents-desktop-core
```
Expected: clean.

- [ ] **Step 2: Runtime package suite**

```bash
cargo test -p gents
```
Expected: PASS (integration tests are separate compile units — never trust `--lib`).

- [ ] **Step 3: Workspace check**

```bash
cargo check --workspace --all-targets
```
Expected: clean. This catches desktop/bridge construction sites the package suite skips.

- [ ] **Step 4: Proofs**

```bash
cd crates/gents/proofs && lake build && cd -
rg -n '\bsorry\b' crates/gents/proofs/Proofs --glob '*.lean'
```
Expected: build clean; the `rg` command returns no matches.

- [ ] **Step 5: Live two-node smoke (manual, environment-dependent)**

With the amy runtime on studio-1 reachable (see memory note / spec):

1. Record a fresh remote baseline of `_docID`s/counts for `AgentConversation` and `AgentSession`, plus the transcript collection counts.
2. Build the desktop app, wipe only its local application store, and `/status`-pair against `http://100.69.4.79:9191/status`.
3. Verify the local index docIDs/counts match the contemporaneous remote baseline for amy and that titles/previews render. Do not use the stale “129 conversations” observation as an assertion.
4. Before creating any new activity, verify historical `AgentRequest`, `AgentResponse`, `AgentMessage`, `AgentToolCall`, `AgentToolResult`, `AgentToolApproval`, and `CompactionEntry` rows were not bulk-pulled.
5. Verify logs show a two-collection index request (without claiming merge completion), not the old 16-collection pass, and inspect the resulting `PeerPairingDesired` row (`template: "machine"`, `collections: null`).

- [ ] **Step 6: Review the final diff and commits**

```bash
git status --short
git diff origin/main...HEAD --stat
git log --oneline origin/main..HEAD
```

Expected: only scoped implementation/docs changes plus the pre-existing unrelated `Info.plist` modification in the worktree. Stop ready-to-push; do not push unless explicitly requested.
