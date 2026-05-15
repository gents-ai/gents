# Issue #193 — Refactor `DefraAgent` into typed `AgentPrincipal` + `AgentBehavior`

Date: 2026-05-15

Branch: `design/issue-193-principal-behavior-deployment`

Related: #185 (Lean contract — closed), #219 (executable identity permission cases — closed),
#183 (parent formal-coverage tracker), #9 (original split issue — closed), #180 (P2P admin auth — sequenced after this).

## Problem

The runtime still conflates principal and behavior inside `DefraAgent`
(`crates/defra-agent/src/agent.rs:86`). `agent_did`, `default_behavior_id`, and
`Vec<BehaviorConfig>` are sibling fields on the same struct; each `BehaviorConfig`
additionally carries its own `identity: Arc<dyn AgentIdentity>`. Desired-state
schemas (`agent_principal.graphql`, `agent_behavior.graphql`) and the apply-time
`Collection` enum already split. The Lean contract (`#185`) and executable
identity permission cases (`#219`) already split. **The runtime split is the
remaining gap.**

The success criterion is observable: `tests/identity_conformance.rs::identity_respects_principal_contract_is_declared`
currently asserts `!target.enforced`. This PR makes the contract enforced by
the runtime and flips that assertion to `target.enforced == true`.

## What was reconsidered during brainstorming

Two scope decisions diverge from the issue body as written.

### `AgentDeployment` is dropped from this PR

The issue body lists `AgentDeployment` schema + apply-reconcile collection
variant as in scope. After discussion: in this codebase, the **installation
*is* the deployment**, and `AgentPrincipal` is the top-level runtime concept.
One defra-agent process hosts exactly one principal. The Lean `Deployment`
record stays as a model abstraction (it proves I5 co-hostability) but
corresponds 1:1 to `AgentPrincipal` in the running system.

Implications:

- No new `AgentDeployment` GraphQL schema.
- No new `Collection::AgentDeployment` variant.
- No new apply-reconcile validator for deployment rows.
- The Lean `Deployment` record is unchanged.
- The Rust `AgentPrincipal` type carries the identity, default behavior, and
  enabled flag for the deployment; "deployment metadata" (host id, install
  time) is not modeled in this PR.

The #193 issue body should be updated to record this scope reduction.

### No new Rust-side permission decider

The issue body lists "permission decision module that satisfies the Lean
`RespectsPrincipal` predicate" as in scope. After reviewing defradb.rs's ACP
crate (`DocumentACP` trait, `DocumentPermission`, `Identity::Authenticated(Did)`),
the conclusion: **DefraDB ACP is the decider**, and it is already DID-keyed.
No new Rust decide function is needed; introducing one would be a parallel
implementation of permissions that we'd then have to keep in sync with ACP.

The runtime's contribution is **routing**: every site that today reads
`behavior.identity` to sign a DB op switches to `behavior.principal.identity`.
Two behaviors with the same principal supply the same `Identity::Authenticated(Did)`
to ACP by construction, so ACP returns identical answers for them — that is
the form `RespectsPrincipal` takes in this system.

## Design

### Runtime data model

Three changes in `crates/defra-agent/src/`:

**New type `AgentPrincipal`** (`identity.rs` or new `principal.rs`):

```rust
pub struct AgentPrincipal {
    pub agent_did: String,
    pub identity: Arc<dyn AgentIdentity>,
    pub default_behavior_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
}
```

Single instance per deployment. Owns the signing identity used for every
DefraDB op the runtime issues.

**Rename `BehaviorConfig` → `AgentBehavior`** (`config.rs`):

- Rename the struct.
- Rename the `name` field to `behavior_id` (matches the schema).
- **Remove** the `identity: Arc<dyn AgentIdentity>` field.
- **Add** `principal: Arc<AgentPrincipal>` field — back-reference to the
  deployment's principal.
- Add convenience methods:
  - `fn agent_did(&self) -> &str { &self.principal.agent_did }`
  - `fn principal_identity(&self) -> &Arc<dyn AgentIdentity> { &self.principal.identity }`

The back-reference makes the type invariant structural: an `AgentBehavior`
cannot exist without its principal. Lean's
`behavior_id_determines_principal` theorem becomes observable in the type
system — no construction path produces a behavior with a dangling `agent_did`.

**`DefraAgent` keeps its public name, internals re-typed** (`agent.rs`):

```rust
pub struct DefraAgent {
    node: Arc<EmbeddedNode>,
    principal: Arc<AgentPrincipal>,            // was: agent_did + default_behavior_id
    behaviors: Vec<Arc<AgentBehavior>>,        // was: Vec<Arc<BehaviorConfig>>
    unavailable_behaviors: HashMap<String, String>,
    // unchanged: document_runtime_context, mcp_pool, local_hostname,
    // local_subnet, retry_policy, hook_failure_policy,
    // process_state_observer, manual_trigger_handle
}
```

Accessors:
- `pub fn principal(&self) -> &AgentPrincipal` — new.
- `pub fn agent_did(&self) -> &str` — kept, delegates to `principal`.
- `pub fn default_behavior_id(&self) -> &str` — kept, delegates to `principal`.
- `pub fn behaviors(&self) -> &[Arc<AgentBehavior>]` — kept; return type changed.

`unavailable_behaviors` stays as a deployment-level diagnostic snapshot
(behavior IDs that exist in DefraDB but failed to resolve their config); it
is not identity state.

### Where principal and behavior surface in the runtime

The refactor's observability claim: every site that emits or routes on
identity now sources it from `behavior.principal.identity`, not from a
duplicated `behavior.identity` field. Audit:

- **Request materialization** (`lifecycle/materialize.rs`,
  `trigger_engine/production_materializer.rs`): `AgentRequest` already has
  `agent_did` + `behavior_id` columns; the materializer already populates both.
  No schema or row-shape change. Internal contexts that previously took
  `&BehaviorConfig` take `&AgentBehavior` and source the DID via
  `behavior.agent_did()`.

- **Watcher / claim path** (`watcher.rs`, `lifecycle/claim.rs`):
  `LifecycleWatcher::new(node, agent_did)` unchanged — watches all requests
  for the deployment's principal.

- **Hooks / audit** (`hook.rs`): `Hooks` already carries `agent_did`. Where
  the hook also needs behavior identity, it sources it from the
  `&AgentBehavior` it already holds. Signing identity for hook DB writes
  comes from `behavior.principal_identity()`.

- **Trace export** (`trace_export.rs`): already has `agent_did: Option<String>`
  and `behavior_id: Option<String>` as separate fields. No shape change.

- **Background completion / background tools**
  (`background_completion.rs`, `background_tools.rs`): rows already carry
  both columns; call sites switch from `behavior.identity` to
  `behavior.principal_identity()`.

- **Runtime status** (`runtime_status.rs`): principal-scoped (one row per
  deployment). Unchanged.

- **Subagent authorization** (`background_tools.rs:133`): per-behavior tool
  config check, no identity dependency. Unchanged.

- **MCP pool, streaming, compaction**: type-only renames where they hold
  references to `BehaviorConfig`. None of these layers call ACP; the move
  is mechanical.

**Net effect:** zero schema changes, no new audit columns. The contribution
is **typing** — call sites that previously took `&BehaviorConfig` (carrying
both behavior config and identity) now take `&AgentBehavior` and source
identity exclusively via the principal back-ref.

### Conformance test flip

The success criterion is making `identity.respects_principal_boundary`
enforced. The flip pivots on the runtime-routing witness; defradb.rs's ACP
is the decider and is already DID-keyed.

**Lean change** (`Proofs/Identity/Conformance.lean:436`): sharpen the
contract statement to be routing-explicit, and flip `enforced := false` →
`enforced := true`. New statement (draft text — exact wording finalized in
implementation):

> "The runtime's `behavior_id → agent_did` resolution is single-valued: for
> any two `AgentBehavior` rows b₁, b₂ with `b₁.agent_did == b₂.agent_did`,
> the runtime supplies the same `Identity::Authenticated(did)` as the actor
> for any DefraDB ACP check. Any DID-keyed permission decision therefore
> returns identical results."

The statement keeps the `agent_did` substring (existing Rust assertion).
The new Rust test additionally asserts the statement names routing.

**Rust changes** in `tests/identity_conformance.rs`:

1. **Replace local Rust mirrors with runtime types.** Add a helper:

   ```rust
   fn build_runtime_behaviors_from_lean_case(
       case: &LeanIdentityPermissionCase,
   ) -> (HashMap<String, Arc<AgentPrincipal>>, Vec<Arc<AgentBehavior>>) {
       // Construct one Arc<AgentPrincipal> per Lean principal row using a
       // stub `AgentIdentity` keyed by did. Then construct one
       // Arc<AgentBehavior> per Lean behavior row, with the matching
       // principal back-ref.
   }
   ```

   Rewrite `identity_permission_cases_pin_runtime_permission_contract_shape`
   so the assertions go through `behavior.principal.agent_did` rather than
   the local `case.behaviors[i].principal`. The Lean row is the fixture;
   the test now exercises runtime construction.

2. **Rename and rewrite the contract test:**

   ```rust
   #[test]
   fn identity_respects_principal_contract_enforced_by_runtime_routing() {
       let target = /* find identity.respects_principal_boundary */;
       assert!(target.enforced, "...");
       assert!(target.statement.contains("agent_did"), "...");
       assert!(
           target.statement.contains("routing")
               || target.statement.contains("Identity::Authenticated")
               || target.statement.contains("resolution"),
           "statement must name the routing interpretation"
       );
       // Drive runtime construction over all 4 Lean rows.
       for case in lean_identity_permission_cases() {
           let (principals, behaviors) = build_runtime_behaviors_from_lean_case(case);
           let by_id: HashMap<&str, &AgentBehavior> = behaviors.iter()
               .map(|b| (b.behavior_id.as_str(), b.as_ref()))
               .collect();
           let actor = by_id[case.actor_behavior.as_str()];
           let peer = by_id[case.peer_behavior.as_str()];
           assert_eq!(actor.principal.agent_did, case.expected_actor_principal);
           assert_eq!(peer.principal.agent_did,  case.expected_peer_principal);
           assert_eq!(
               actor.principal.agent_did == peer.principal.agent_did,
               case.same_principal,
           );
       }
   }
   ```

3. **Proptest for the routing invariant** (new test): generate random
   identity worlds (1..6 principals, 1..20 behaviors distributed across
   them), build runtime back-refs, verify `Arc::ptr_eq(&b1.principal,
   &b2.principal) ⇒ b1.principal.agent_did == b2.principal.agent_did`.
   Structurally true by Arc sharing; the test fences the construction code
   that wires up back-refs.

**What does NOT change:**

- No new `Permissions` trait, no `GrantStorePermissions`, no Rust decide
  function.
- The Lean row's `grants` / `permission` / `expected_actor_allowed` fields
  stay; they remain useful fixtures inside Lean for proving
  `canonicalDecide_respectsPrincipal`. From Rust's perspective these become
  unused data on the row (the existing
  `pin_runtime_permission_contract_shape` test continues to exercise them
  as a Lean shape-pin).
- No defradb.rs `DocumentACP` driver added in this PR. An end-to-end ACP
  integration test that drives `check_doc_access` for both behaviors and
  verifies identical outcomes is a natural follow-on; deferred.

### Apply path and schemas

**No apply-path or schema changes.** The schemas, Collection variants, and
desired-state types already exist:

- `crates/defra-agent-protocol/schemas/agent/agent_principal.graphql` —
  present.
- `crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql` —
  present.
- `Collection::AgentPrincipal` and `Collection::AgentBehavior` — present in
  `crates/defra-agent/src/collection.rs:13`, apply-ordered correctly
  (behavior=1 < principal=3).
- `DesiredAgentPrincipal` / `DesiredAgentBehavior` — present in
  `crates/defra-agent-cli/src/desired_state/mod.rs:31,40`.
- FK closure (behavior.agent_did → existing principal) — enforced by the
  existing Lean `ApplyReconcile.Manifest.WellFormed` plus the Rust
  validator.

**Loader-side changes** in `crates/defra-agent/src/`:

1. Extend `document_view::load_document_runtime_view` and
   `runtime_snapshot::ResolvedRuntimeSnapshot` to surface `display_name`
   and `enabled` from the AgentPrincipal row in addition to the existing
   `default_behavior_id`.
2. Construct `Arc<AgentPrincipal>` in `DefraAgent::from_default_behavior_documents`
   (and in the reconcile snapshot-rebuild path) from those fields plus the
   `Arc<dyn AgentIdentity>` passed at startup.
3. Construct `Arc<AgentBehavior>` instances with the principal back-ref.
4. If no AgentPrincipal row exists, fall back to defaults (existing
   behavior preserved): `AgentPrincipal { agent_did: from_identity,
   identity, default_behavior_id: derived, display_name: None, enabled: true }`.

Reconcile already swaps the snapshot atomically; one extra Arc clone per
behavior per reconcile is negligible.

## Sequencing

Bite-sized commits, TDD where applicable, each independently buildable and
testable. Each commit carries the `Co-Authored-By: Claude Opus 4.7 (1M
context) <noreply@anthropic.com>` trailer.

1. **Lean — sharpen statement text** for `identity.respects_principal_boundary`
   in `Proofs/Identity/Conformance.lean`. Keep `enforced := false` for this
   commit. `lake build` green.

2. **Rust — add `AgentPrincipal` struct** in `src/identity.rs` (or new
   `principal.rs`). No production callers yet. `cargo check` green.

3. **Rust — rename `BehaviorConfig` → `AgentBehavior`**, drop `identity`
   field, add `principal: Arc<AgentPrincipal>`, add `behavior_id` (renamed
   from `name`) and helper methods. Compiler-driven refactor across every
   call site. `cargo check --workspace --all-targets` green;
   `cargo test -p defra-agent` green.

4. **Rust — extend the loader** (`document_view.rs` / `runtime_snapshot.rs`)
   to read `display_name` + `enabled` from the AgentPrincipal row.

5. **Rust — rework `DefraAgent` construction** to build
   `Arc<AgentPrincipal>` and clone it into every `Arc<AgentBehavior>`.
   `DefraAgent::agent_did()` and `default_behavior_id()` delegate.
   Reconcile snapshot-rebuild path follows the same model.

   *Suggested PR-open point.* The typed runtime types are now visible and
   reviewers can compare against the spec while the conformance work
   finishes.

6. **Rust — rewrite
   `identity_permission_cases_pin_runtime_permission_contract_shape`** to
   drive runtime types from the Lean rows. Delete (or comment-mark for
   Lean shape-pin only) `rust_canonical_permission_decision` and
   `rust_hostability_decision`.

7. **Rust — rewrite the contract test:** rename
   `identity_respects_principal_contract_is_declared` →
   `identity_respects_principal_contract_enforced_by_runtime_routing`.
   Asserts `target.enforced == true`, statement language, and exercises
   runtime construction over all 4 Lean rows.

8. **Lean — flip `enforced := false` → `enforced := true`**. `lake build`
   green; the Rust test from step 7 also green.

9. **Rust — add proptest** for the routing invariant.

10. **Docs — update issue body for #193** to record `AgentDeployment` scope
    reduction with a link to this spec.

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Hidden callers of `BehaviorConfig.identity` in test fixtures and dev-only paths | Compiler-driven rename surfaces each site as a compile error. Add a `test_support::principal_and_behaviors()` helper to keep test fixture churn low. |
| Public-API leakage: `defra-agent-cli` and other crates may import `BehaviorConfig` as `defra_agent::BehaviorConfig` | `cargo check --workspace` covers this. Add a transitional `pub use AgentBehavior as BehaviorConfig;` re-export if widely consumed outside the crate; remove once callers updated. |
| Lean statement text change breaks the existing `.contains("agent_did")` assertion | New statement keeps the `agent_did` substring and additionally names routing. The Rust assertion in step 7 strengthens to check both. |
| Reconcile snapshot rebuild churn — every behavior gets a fresh `Arc<AgentPrincipal>` on principal mutation | Reconcile already does atomic snapshot swaps; one extra Arc clone per behavior per reconcile is negligible. No behavioral change. |
| Compaction / streaming / MCP pool secretly depend on per-behavior identity | Audit during step 3 via `cargo check`; each compile error is one mechanical fix. None of these layers should call ACP, so the move from `behavior.identity` → `behavior.principal_identity()` is type-only. |
| Lean statement change might affect other downstream consumers | The only Rust consumer today is `identity_respects_principal_contract_is_declared`. No other ledger consumer reads the statement text. |

## Success criteria

Mirrors PROMPT.md operating rules.

- `cd crates/defra-agent/proofs && lake build` — zero `sorry`s, all
  existing theorems still proven.
- `cargo fmt --all` clean.
- `cargo check --workspace --all-targets
  --exclude agent-subagent-v2-to-v3-lens
  --exclude agent-tool-call-lifecycle-v1-to-v2-lens` clean.
- `cargo test -p defra-agent --lib --tests` fully green, including:
  - `identity_structural_cases_match_lean_verdicts` (unchanged, green)
  - `identity_structural_cases_cover_named_scenarios` (unchanged, green)
  - `identity_permission_cases_pin_runtime_permission_contract_shape`
    (rewritten to drive runtime types, green)
  - `identity_respects_principal_contract_enforced_by_runtime_routing`
    (new test, green with `enforced == true`)
  - new proptest for the routing invariant (green)
- `cargo test -p defra-agent-cli` fully green.
- `identity.respects_principal_boundary` row in Lean has `enforced := true`
  and a routing-explicit statement.
- PR open against `main` after step 5; updated as remaining steps land.

## Out of scope

- `AgentDeployment` schema, `Collection::AgentDeployment` variant, or any
  apply-reconcile validator for deployment rows.
- Cedar / Zanzibar engine selection or evaluation. The Lean contract is
  engine-agnostic; ACP is the chosen substrate.
- New `Permissions` trait or Rust-side decide function.
- P2P admin authentication (#180 sits on top of this once principal is
  split).
- Compaction / streaming / persistence reshapes.
- End-to-end DocumentACP integration test that drives `check_doc_access`
  for two behaviors and asserts identical outcomes (natural follow-on; not
  required to flip `enforced`).
- Renaming `DefraAgent` itself (e.g., to `AgentRuntime`). The public API
  stays.
