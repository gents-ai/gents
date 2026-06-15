# Scope Templates + Filtered Replication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Reframe P2P pairing around named scope templates over filtered replication: `(collections + scope + delivery)` bundles that make the conversation slice scoped-by-agent-DID the default, retire raw profiles / whole-collection-default / dual-install, and replicate only one agent's slice to a peer.

**Architecture:** A small hardcoded `ScopeTemplate` catalog resolves to `(collections, scope→filter, delivery)`. The pairing reconciler installs a **filtered push** replicator for `Push` templates (no collection subscription) and the existing subscribe+replicate for `Replicate` templates. Filters cross the `RemoteP2pAdmin` seam as our own type and translate to defradb.rs #1033's `ReplicationFilters` in the embedded/HTTP adapters. Lean's replicator dimension gains the filter; template resolution is a proven pure function.

**Tech Stack:** Rust, Lean 4 + mathlib, GraphQL/DefraDB, defra.rs #1033 (filtered replication — pin bumps at merge).

**Spec:** `docs/superpowers/specs/2026-06-13-scope-templates-filtered-replication-design.md`

---

## Execution notes (read first)

- **Worktree:** `../defra-agent-cli-normalization` (branch `cli-normalization`). Mathlib cache symlinked under `crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build`; never `lake exe cache get`.
- **Gates:** `cargo test -p defra-agent` (FULL), `cargo test -p defra-agent-cli`, `lake build`, `cargo fmt --check`. Zero `sorry`. No fixed `sleep` in tests (poll-with-timeout).
- **Sharp edges:** `defra_agent::graphql::escape_graphql_string()` for interpolation; never emit `[]` (emit `null`); `tracing` not `println`.
- **#1033 dependency boundary (critical):** #1033 is NOT pinned yet. Build all logic against **our own** filter type (`PairingFilters` defined in T2) at the `RemoteP2pAdmin` seam so everything compiles and tests now. The ONLY code that touches defradb's `ReplicationFilters` is the embedded/HTTP adapter translation in T2, behind a single clearly-marked seam. Until the pin bumps, the embedded adapter translates non-empty filters to the closest current behavior and logs a `// TODO(#1033 pin)`; the end-to-end "only scoped docs replicate" assertion (T7) is the one bump-gated test — mark it `#[ignore = "enable at #1033 pin bump"]`. Everything else is green now.
- **Pin-bump closeout (done at defradb.rs #1033 merge, NOT in this plan's tasks):** bump the workspace `Cargo.toml` defradb.rs tag, flip the T2 translation seam to call the real filtered `add_replicator`, un-ignore the T7 e2e test. One small commit.
- **Templates to mirror:** registry feature (`crates/defra-agent/src/agent/p2p_reconcile/{engine,diff,discovery,registry}.rs`, `Proofs/PeerRegistryDiscovery/`, the conformance harness) — same patterns, same quality bar (no-vacuity Lean; conformance calls real engine fns).

---

### Task T0: `@immutable agent_did` scope key (gating, no #1033 dep)

**Files:**
- Modify schemas: `crates/defra-agent-schemas/schemas/agent/{agent_request,agent_response,agent_tool_result,agent_conversation}.graphql` (mark `agent_did @immutable`); `{agent_message,agent_tool_call,agent_session,compaction_entry}.graphql` (ADD `agent_did: String @index @immutable`).
- Modify: `crates/defra-agent/src/migration.rs` (additive field-add migration for the 4 session-keyed collections; mirror an existing `ensure_*_migrations` / field-add patch).
- Modify: every create site for those 4 collections to stamp `agent_did` (grep `create_AgentMessage|AgentToolCall|AgentSession|CompactionEntry` upserts; the owning agent is in scope — it's the session/request owner).
- Test: a guard test that rewriting `agent_did` on a created AgentRequest is rejected; migration test that the 4 added fields exist.

- [ ] **Step 1: Audit `agent_did` write-once.** Grep every writer of AgentRequest/Response/ToolResult/Conversation; confirm `agent_did` is set only at create, never in an update mutation. Record findings in the commit message. If any writer rewrites it, STOP and report (the immutability assumption is wrong).
- [ ] **Step 2: Failing migration test** for the 4 added fields (mirror the peer-registry migration test): fresh node → conversation migrations → `AgentMessage`/`AgentToolCall`/`AgentSession`/`CompactionEntry` each have an `agent_did` field. Run, confirm FAIL.
- [ ] **Step 3: Schemas** — mark `@immutable` on the 4 that have `agent_did`; add `agent_did: String @index @immutable` to the 4 that lack it.
- [ ] **Step 4: Migration** — additive field-add for the 4 session-keyed collections (mirror `migration.rs` field-add patches; `Kind` = String = 11). Wire into the startup migration sequence.
- [ ] **Step 5: Stamp at create** — at each create site for the 4, set `agent_did` to the owning agent (resolve from the session/request context already present). Run `cargo build -p defra-agent`.
- [ ] **Step 6: Guard test** — create an AgentRequest, attempt to update its `agent_did`, assert rejected (this also smoke-tests the `@immutable` declaration end-to-end on the embedded node).
- [ ] **Step 7:** `cargo test -p defra-agent` full green. Commit `feat(schema): immutable agent_did scope key across conversation collections`.

---

### Task T1: ScopeTemplate model + catalog + resolution + `p2p templates list`

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/templates.rs` — `ScopeTemplate`, `Scope`, `Delivery`, the hardcoded `BUILTIN_TEMPLATES`, `resolve_template(id) -> Option<&ScopeTemplate>`, `template_collections(id)`, `scope_filter(scope, peer_did) -> PairingFilters` (PairingFilters defined here as a `BTreeMap<String, FilterPredicate>` where `FilterPredicate { field, value }` — our seam type, #1033-independent).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/mod.rs` (exports).
- Modify: `crates/defra-agent-cli/src/cli/args.rs` + `commands/p2p/` — `p2p templates list`.
- Test: unit tests in `templates.rs`; CLI parse/handler test.

- [ ] **Step 1: Failing unit tests** in `templates.rs`:
```rust
#[test]
fn conversation_is_scoped_push_with_eight_collections() {
    let t = resolve_template("conversation").unwrap();
    assert_eq!(t.delivery, Delivery::Push);
    assert!(matches!(t.scope, Scope::PeerDid { ref field } if field == "agent_did"));
    assert_eq!(t.collections.len(), 8);
    assert!(t.collections.contains(&"AgentRequest"));
    assert!(!t.collections.contains(&"CodexThreadProjection")); // excluded (issue #494)
}
#[test]
fn agent_config_includes_behavior_excludes_principal() {
    let t = resolve_template("agent-config").unwrap();
    assert_eq!(t.delivery, Delivery::Replicate);
    assert!(matches!(t.scope, Scope::Unscoped));
    assert!(t.collections.contains(&"AgentBehavior"));
    assert!(!t.collections.contains(&"AgentPrincipal"));
}
#[test]
fn scope_filter_builds_per_collection_agent_did_equality() {
    let t = resolve_template("conversation").unwrap();
    let f = scope_filter(&t.scope, &t.collections, "did:key:bob");
    assert_eq!(f.get("AgentRequest").unwrap().field, "agent_did");
    assert_eq!(f.get("AgentRequest").unwrap().value, "did:key:bob");
    assert_eq!(f.len(), 8); // one predicate per collection
}
#[test]
fn backup_is_unscoped_replicate() { /* delivery Replicate, scope Unscoped */ }
#[test]
fn unknown_template_is_none() { assert!(resolve_template("nope").is_none()); }
```
- [ ] **Step 2: Run** `cargo test -p defra-agent templates` — FAIL.
- [ ] **Step 3: Implement** the types + the 3-entry `BUILTIN_TEMPLATES` catalog (exact collection sets from the spec table) + the resolution/`scope_filter` fns. `scope_filter(Unscoped, ..)` → empty `PairingFilters`.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: `p2p templates list`** — add `P2pCommand::Templates { command: P2pTemplatesCommand::List(..) }` (or a flat `Templates(P2pAccessArgs)`); handler prints the catalog (id, collections, scope, delivery) as table/json. Reuse `OutputFormat`. Parse + render test.
- [ ] **Step 6:** `cargo test -p defra-agent templates` + `cargo test -p defra-agent-cli --bins templates` + `cargo fmt --check`. Commit `feat(p2p): scope-template catalog + p2p templates list`.

---

### Task T2: filter seam — `RemoteP2pAdmin::add_replicator` gains filters (adapter translation is the only #1033-gated line)

**Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/trait_def.rs` — `add_replicator(&self, addresses, collections, filters: &PairingFilters)`.
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (`EmbeddedRemoteP2pAdmin`) + `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs` (`HttpRemoteP2pAdmin`) — accept filters; translate at the seam.
- Test: a unit/contract test that a non-empty `PairingFilters` is carried to the adapter call (recording mock), and the empty case is byte-identical to today.

- [ ] **Step 1: Failing test** — extend the existing `RemoteP2pAdmin` test double / conformance store to record the `filters` arg; assert `add_replicator` with a non-empty `PairingFilters` records it, and empty filters preserve current behavior.
- [ ] **Step 2: Run** — FAIL (signature lacks filters).
- [ ] **Step 3: Implement** — add `filters: &PairingFilters` to the trait method and all impls/callers (the discovery/pairing callers pass empty for now; T4 wires real filters). **Translation seam:** in the embedded + HTTP impls, add a `fn to_defra_filters(&PairingFilters) -> <defradb ReplicationFilters>` with a single `// TODO(#1033 pin): once defradb.rs is bumped, pass these to the filtered add_replicator`. Until the pin bump: empty filters → exactly today's call; non-empty filters → today's call + a `tracing::warn!("filtered replication pending #1033 pin")` (so the code path is exercised and visible, not silently dropped). This is the ONLY place defradb's filter type is referenced; everything else uses `PairingFilters`.
- [ ] **Step 4: Run** — PASS. `cargo test -p defra-agent -p defra-agent-desktop-core`.
- [ ] **Step 5: Commit** `feat(p2p): filters at the RemoteP2pAdmin seam (adapter translation gated on #1033 pin)`.

---

### Task T3: Lean — replicator-with-filter dimension + template resolution

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/{State,Transition,Convergence,Executable}.lean`
- Create: `crates/defra-agent/proofs/Proofs/ScopeTemplates/...` (template resolution: pure, total, deterministic)
- Modify: structure fence + coverage ledger registration (mirror existing).

- [ ] **Step 1:** Extend the replicator identity in `PairingReconcile/State.lean`: a replicator is now `(address, filter)` (filter an abstract key, `none` = unfiltered). `reconcileInstallReplicator`/`Teardown` carry it; the diff treats a changed filter as teardown-then-install. Re-prove convergence / idempotence / no-flap over the enriched identity (mirror how the collection/replicator dimensions were already handled — `cases` over all transitions; reconcile cases discharged from guards).
- [ ] **Step 2:** New `ScopeTemplates` module: `resolveTemplate : TemplateId → Option Template`, `Template = (collections, scope, delivery)`; prove resolution is deterministic and total over the catalog, and `scopeFilter (PeerDid f) did c = some ⟨f, did⟩` / `Unscoped → none`. Keep it a pure function (no transition system needed) — like the registry derivation sits beside the reconciler.
- [ ] **Step 3:** Executable contract round-trip for any new transition-kind vocabulary; register both in structure fence + coverage ledger.
- [ ] **Step 4:** `lake build` — zero `sorry`. Commit `proof(pairing): replicator filter dimension + scope-template resolution`.
- [ ] **NO-VACUITY BAR:** the filter-reinstall theorem must be quantified over all transitions with satisfiable hypotheses; convergence must not be a fixpoint-only claim mislabeled as reachability (follow the PairingReconcile honesty precedent). I will read these proofs.

---

### Task T4: reconciler — template resolution → Push (filtered, no subscribe) / Replicate

**Files:**
- Modify: `crates/defra-agent-schemas/schemas/agent/peer_pairing_desired.graphql` — add `template: String` (+ migration; the resolved `filters` are derived at reconcile time from template+peer, not stored, OR stored as a JSON column — choose stored-derived to keep the row self-describing; decide in impl matching the Lean).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/{engine,diff}.rs` — resolve `template` → `(collections, scope, delivery)`; `Push` → filtered `add_replicator`, NO `add_p2p_collections`; `Replicate` → today's subscribe+replicate. Filter is part of the replicator's desired identity in the diff (changed filter ⇒ reinstall), mirroring T3.
- Modify: operator write paths (`pairings.rs`) to set `template`.
- Test: unit tests — Push template installs filtered replicator + does NOT subscribe; Replicate template subscribes + replicates; filter change reinstalls.

- [ ] **Step 1: Failing tests** (mirror the discovery/engine test style with the recording admin):
```rust
#[test] fn push_template_installs_filtered_replicator_without_subscription() { /* conversation: add_replicator with agent_did filter, no add_p2p_collections */ }
#[test] fn replicate_template_subscribes_and_replicates() { /* backup/agent-config: add_p2p_collections + unfiltered add_replicator */ }
#[test] fn filter_change_reinstalls_replicator() { /* changing the scoped DID tears down + reinstalls */ }
```
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** the template-driven branch in `reconcile_peer_tick`; add `template` to `PeerPairingDesired` (schema+migration); resolve scope→filter via T1's `scope_filter`; pass filters through T2's seam. Keep ownership (`source`) + applied-set teardown unchanged.
- [ ] **Step 4: Run** + full `cargo test -p defra-agent`. Commit `feat(p2p): template-driven reconcile (filtered push vs replicate)`.

---

### Task T5: registry offers templates; discovery materializes scoped pairings

**Files:**
- Modify: `peer_registry.graphql` — `profiles` → `templates` (or add `templates`; keep `profiles` only if a migration-free path is cleaner — prefer renaming the offer to `templates` with migration).
- Modify: `discovery.rs` — derive a `PeerPairingDesired` with `template` (the offered template) and resolved scope (the LOCAL node's DID is the filter value, since the offer is "your slice"); registry-owned, ownership-safe (unchanged invariant).
- Modify: `registry.rs` self-registration to advertise offered templates; CLI `p2p network register --template`.
- Test: discovery materializes a scoped registry-owned row from a template offer; ownership still holds.

- [ ] **Step 1: Failing tests** (mirror existing discovery tests) — a registry entry offering `conversation` → discovery materializes a `source="registry"`, `template="conversation"` desired row scoped to the local DID; operator rows untouched.
- [ ] **Step 2–4:** implement; full `cargo test -p defra-agent`. Commit `feat(p2p): registry advertises scope templates; discovery materializes scoped pairings`.

---

### Task T6: CLI `--template` front door + admin `--filter` + docs

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs` — `pairings invite|join|set --template <id>` (default `conversation`); demote `--profile`/`--collection` to hidden/admin.
- Modify: `commands/p2p/{invite,join,pairings}.rs` — carry template through; the invite token offers a template.
- Modify: `commands/p2p/replicators.rs` (admin) — `add --filter <collection>:<field>=<value>`.
- Modify: `docs/demo.md` (Part 3 → "pair by intent, not by schema": `--template conversation`), `docs/operations.md` (templates reference + `p2p templates list`).
- Test: parse tests for `--template`, `--filter`; help snapshot.

- [ ] **Step 1: Failing parse/handler tests.** **Step 2: Run FAIL. Step 3: Implement** (`--template` default `conversation`; `invite` embeds the offered template in the token — extend the protocol `InviteToken` with `template: String`, bump nothing else since it's additive within v2 or a v3 — match the existing token-version discipline). **Step 4:** CLI suite + fmt. Commit `feat(cli): --template pairing front door; admin replicators --filter; docs`.

---

### Task T7: integration tests

**Files:**
- Modify/Create: `crates/defra-agent-cli/tests/cli_p2p_network.rs` (or a sibling) — the scenarios; reuse the multi-node harness helpers.

- [ ] **Step 1: Install-level test (green now, no pin):** two nodes paired on `conversation` — assert the reconciler installs a **filtered** replicator carrying the `agent_did` predicate and does NOT subscribe the collections (inspect via the recording path / `p2p admin replicators list` shows the filter). `backup`/`agent-config` install subscribe+replicate.
- [ ] **Step 2: End-to-end filtering test (bump-gated):** write docs for two different `agent_did`s on the source; assert only the pairing-DID's docs appear on the peer. Mark `#[ignore = "enable at #1033 pin bump"]` with a comment pointing at the pin-bump closeout. Do NOT delete it — it's the acceptance test for the bump.
- [ ] **Step 3:** filter-change reinstall (live): change a pairing's scope DID, assert the replicator is reinstalled with the new predicate.
- [ ] **Step 4:** full `cargo test -p defra-agent`, `cargo test -p defra-agent-cli`, `lake build`, `cargo fmt --check`. No sleeps, no un-gated flakes. Commit `test(p2p): scope-template install + filtered-replication e2e (bump-gated)`.

---

## Self-review notes

- **Spec coverage:** T0 (immutable scope key), T1 (templates+catalog+resolution+list), T2 (filter seam), T3 (Lean filter dimension + resolution), T4 (template reconcile), T5 (registry offers templates), T6 (CLI front door + admin filter + docs), T7 (tests). The pin-bump closeout (Cargo.toml tag + flip T2 seam + un-ignore T7 e2e) is explicitly OUT of these tasks — done at defradb.rs #1033 merge.
- **#1033 isolation:** only T2's `to_defra_filters` seam references defradb's `ReplicationFilters`; everything else uses our `PairingFilters`. The branch compiles + tests green now; one e2e test is ignore-gated.
- **Type consistency:** `PairingFilters`/`FilterPredicate{field,value}` (T1, used T2/T4/T5); `ScopeTemplate`/`Scope`/`Delivery`/`resolve_template`/`scope_filter` (T1, used T4/T5/T6); `PeerPairingDesired.template` (T4, read T5); `InviteToken.template` (T6).
- **Retirements:** `--profile` demoted to admin (T6); whole-collection-default replaced by `conversation` scoped default (T4); subscribe dropped from Push path (T4). `P2pCollectionProfile` enum retained only for admin/raw.
- **No-vacuity (T3):** filter-reinstall + convergence quantified over all transitions, conformance calls real fns (the pairing-conformance lesson).
