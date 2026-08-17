# Eager Session-Index Sync (#1141) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A freshly paired client renders the full session list (AgentConversation + AgentSession) with no transcript-plane replication and no env gate.

**Architecture:** Follow the foundation flow: first repair the small pre-existing Lean/Rust catalog drift and extend the Lean scope-template catalog, then mirror the new template in Rust and pin conformance. Finally, replace the env-gated bulk pull with a concurrent sync of exactly the two index collections: once after the saved-peer bootstrap loop, plus once after a successful interactive peer add (branchable-collection DAG pull is node-global and unfiltered, so the client sees *all* sessions, not just its requester slice). Make the pairing row request the `machine` template explicitly.

**Tech Stack:** Rust (gents workspace), Lean 4 (crates/gents/proofs), DefraDB embedded node (defradb.rs pinned rev).

**Spec:** `docs/superpowers/specs/2026-08-17-mobile-session-sync-design.md`

## Review Clarifications

- The Lean catalog currently predates `DatastoreToolSurface` in the shared config plane and `PersonaConfigRequest` in `machine`. Task 1 repairs that drift before adding `client-index`; otherwise the claim that Lean and Rust mirror one another would remain false.
- #1141 registers `client-index`, but does not select it for the desktop pairing row. The eager unfiltered DAG pull is what supplies the complete historical index in this issue; the pairing row remains `machine` so the existing requester-authored control plane keeps working. Wiring a pairing to `client-index` is outside this issue.
- In this plan, “no transcript-plane replication” means the eager historical catch-up requests only the two index collections. The existing `machine` grant still has requester-scoped transcript routes, and the desktop still subscribes to new collection heads. If the acceptance criterion instead means *zero transcript transport of any kind*, the approved design and Task 5 conflict and must be redesigned before implementation.
- The old env-gated bulk sync is called both during saved-peer bootstrap (`bootstrap.rs`) and interactive peer add (`writes.rs`). Both paths must be rewired. `supervisor.rs` does not call the helper and is not part of this change.
- “Concurrent” is an acceptance detail, not just a comment: the fixed two collection sync/retry futures run together with `tokio::try_join!` under one shared deadline.

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

theorem clientIndex_covers_exactly_index_collections :
    clientIndexTemplate.collections = clientIndexCollections.toFinset := rfl
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

### Task 4: Bootstrap — ungated index sync, once per bootstrap

**Files:**
- Modify: `crates/gents-desktop-core/src/client/schema.rs`
- Modify: `crates/gents-desktop-core/src/client/core/bootstrap.rs`
- Modify: `crates/gents-desktop-core/src/client/core/writes.rs`
- Modify: `crates/gents-desktop-core/src/client/core/tests.rs`

**Interfaces:**
- Produces: `pub fn index_collection_names() -> [&'static str; 2]` in `schema.rs`; `sync_index_collections_with_retry(node, p2p, timeout) -> Result<Vec<String>>` in `bootstrap.rs` (label parameter dropped — the operation is node-global, not per-peer). The two per-collection retry futures run concurrently and return names in stable index-list order.
- Consumes: `p2p_sync_branchable_collection(p2p, &collection_id)` (existing, `core/p2p_ops.rs`).

- [ ] **Step 1: Write the failing test for the index list**

In `crates/gents-desktop-core/src/client/schema.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn index_collections_are_the_session_index_and_are_branchable() {
        let index = super::index_collection_names();
        assert_eq!(index, ["AgentConversation", "AgentSession"]);
        for name in index {
            assert!(
                gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                "{name} must be branchable for DAG sync"
            );
        }
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p gents-desktop-core index_collections_are
```
Expected: FAIL — `index_collection_names` not found.

- [ ] **Step 3: Implement `index_collection_names`**

In `schema.rs`, after `branchable_collection_names`:

```rust
/// The eager session index: conversation cards (title, preview, lineage)
/// and session lifecycle rows. Synced unfiltered at bootstrap so a paired
/// client renders the complete session list; everything transcript-shaped
/// stays lazy (#1142).
pub fn index_collection_names() -> [&'static str; 2] {
    ["AgentConversation", "AgentSession"]
}
```

- [ ] **Step 4: Rewire bootstrap**

In `bootstrap.rs`:

a. Rename `sync_branchable_collections_with_retry` to `sync_index_collections_with_retry` and drop the `label: &str` parameter. Resolve the two collection IDs up front, then run one retry future per collection with `tokio::try_join!` under the same deadline. Preserve deterministic output order (`AgentConversation`, then `AgentSession`) and use the error message `"timed out syncing index collection {collection_name}: {error}"`.

b. Delete `branchable_pair_sync_enabled()` and the `BRANCHABLE_PAIR_SYNC_ENV` constant. Confirm with `rg -n "env_flag_enabled|env_flag_value" crates/gents-desktop-core/src` that the private parsing helpers have no other callers, then delete them too.

c. In the peer loop, delete the entire `if branchable_pair_sync_enabled() { … } else { tracing::debug!(… "skipping opt-in branchable collection sync after pairing") }` block (the `configure_local_runtime_pairing` match keeps its `Ok(()) => {}` arm trivial and its `Err` arm unchanged).

d. After the peer loop completes (immediately after the `statuses.push(status);` loop ends), add a single node-global index sync, gated only on at least one successful dial. Because this call is outside the loop, multiple saved peers still trigger only one pair of collection requests:

```rust
    if options.install_replicators_on_bootstrap
        && statuses.iter().any(|s| s.dial_succeeded)
    {
        match sync_index_collections_with_retry(node, p2p, BOOTSTRAP_OPERATION_TIMEOUT).await {
            Ok(synced) => {
                tracing::info!(
                    target: "gents_desktop_core::peer",
                    synced_collections = ?synced,
                    "session index sync complete"
                );
            }
            Err(error) => {
                let message = format!("session index sync failed: {error}");
                tracing::warn!(target: "gents_desktop_core::peer", %message);
                errors.push(message);
            }
        }
    }
```

Remove the now-unused `branchable_collection_names` import from `bootstrap.rs` and delete the helper itself now that it has no production callers; the schema test can check `BRANCHABLE_COLLECTION_NAMES` directly.

e. In `writes.rs`, replace the second env-gated call site in `ClientCore::add_peer`. After reverse pairing succeeds, call `sync_index_collections_with_retry` ungated when `connected` is true, using `PEER_ADD_OPERATION_TIMEOUT`. If the peer was saved but did not connect, skip the immediate pull; the next successful bootstrap performs it. Update the log/warning text from “branchable” to “session index”, and remove the deleted env-helper imports.

- [ ] **Step 5: Add focused sync regression coverage**

Extend `RecordingP2P` in `core/tests.rs` (or add a narrow test double) to record `sync_branchable_collection` IDs. Add a test that:

1. starts a local test node so the real collection-name → collection-ID lookup is exercised;
2. calls `sync_index_collections_with_retry`;
3. asserts exactly the `AgentConversation` and `AgentSession` collection IDs were requested, with no transcript collection, and the returned names retain stable index-list order; and
4. keeps the concurrency implementation explicit with `tokio::try_join!`, without adding timing-sensitive test scaffolding.

- [ ] **Step 6: Run the tests**

```bash
cargo test -p gents-desktop-core index_collections_are
cargo test -p gents-desktop-core index_sync_requests_exactly
cargo test -p gents-desktop-core
```
Expected: PASS. `core/supervisor.rs` has no old-helper call site and should remain unchanged.

- [ ] **Step 7: Commit**

```bash
git add crates/gents-desktop-core/src/client/schema.rs crates/gents-desktop-core/src/client/core/bootstrap.rs crates/gents-desktop-core/src/client/core/writes.rs crates/gents-desktop-core/src/client/core/tests.rs
git commit -m "feat(desktop-core): ungated eager session-index sync at bootstrap, replacing the env-gated 16-collection pull (#1141)"
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
5. Verify logs show one two-collection index sync, not the old 16-collection pass, and inspect the resulting `PeerPairingDesired` row (`template: "machine"`, `collections: null`).

- [ ] **Step 6: Review the final diff and commits**

```bash
git status --short
git diff origin/main...HEAD --stat
git log --oneline origin/main..HEAD
```

Expected: only scoped implementation/docs changes plus the pre-existing unrelated `Info.plist` modification in the worktree. Stop ready-to-push; do not push unless explicitly requested.
