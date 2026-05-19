# Identity permission decision + deployment hostability runtime design

Status: design pass — implementation deferred to a follow-up cycle.
Date: 2026-05-19
Tracking: audit item #3 of `docs/superpowers/audits/2026-05-19-conformance-audit.md` §9 Identity.
Related audit: `docs/superpowers/audits/2026-05-15-lean-spec-gap-audit.md` § "#185 / #193: Principal / Behavior / Deployment".
Related specs: `docs/superpowers/specs/2026-05-13-identity-split-lean-design.md`,
`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md`.

## Goal

Close the last open piece of #193: consume the four Lean fields
`expectedActorAllowed` / `expectedPeerAllowed` / `expectedActorHostable` /
`expectedPeerHostable` (`crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:142-148`)
in `crates/defra-agent/tests/identity_conformance.rs:177`, so the coverage
ledger row at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317-321`
promotes from `consumerWithFollowUpCoverage` to `consumerCoverage`.

The audit framed this as "introduce a runtime permission-decision entry
point and a deployment hostability lookup." This spec resolves where those
entry points already exist (DefraDB ACP and the loader's principal-equality
check) and what the smallest delta is to bind the conformance witness to
them. Implementation lands in a follow-up cycle; this PR is design-only.

## Problem

### What is bound in Rust today

#193 closed the principal/behavior split (commit `3d76af9`):

- `AgentPrincipal` at `crates/defra-agent/src/identity.rs:38` owns the
  signing identity, the agent DID, and the default behavior id.
- `AgentBehavior` at `crates/defra-agent/src/config.rs:29` holds
  `principal: Arc<AgentPrincipal>` as a back-ref. The doc comment at
  `:22-27` records that this makes Lean's
  `behavior_id_determines_principal`
  (`crates/defra-agent/proofs/Proofs/Identity/Properties.lean:46`)
  structural at the type level.
- All snapshot construction funnels through
  `assemble_principal_and_behaviors` at
  `crates/defra-agent/src/agent/principal_assembly.rs:54`. That function
  is the only place `Arc::new(AgentPrincipal { ... })` runs during a
  snapshot build; every `Arc<AgentBehavior>` clones the same principal Arc.
- The deployment-hosts-behavior check is the inline guard at
  `crates/defra-agent/src/agent/document_view/apply.rs:24`:
  `if principal.agent_did != agent_did { return Irrelevant; }`. By the
  #193 single-principal-per-process invariant
  (`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:30-57`),
  this IS the deployment hostability check — the running daemon's
  `agent_did` is the deployment's principal DID, and any behavior whose
  `principal.agent_did` does not match is structurally not hostable.

Routing today (`crates/defra-agent/src/agent/runtime/router.rs:50-117`)
resolves a behavior id from `(requested_behavior_id, session_pinned_behavior_id, default_behavior_id)`
and dispatches to its executor. There is no per-request permission check
in the router — every DB op the runtime issues is signed via
`behavior.principal_identity()` (`crates/defra-agent/src/config.rs:123`),
defra-node receives `Identity::Authenticated(did)`, and DefraDB ACP
decides.

### What Lean emits but Rust does not consume

`IdentityPermissionCase` at
`crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:130-149` carries
nine fields beyond the routing witness. The four currently-unconsumed
fields are derived as follows:

- `expectedActorAllowed` / `expectedPeerAllowed` — set by
  `permissionDecision` at `Conformance.lean:200`, which calls
  `canonicalDecide (grantStoreFromCases grants)` (`Permission.lean:31`).
  `canonicalDecide` is defined as `g.granted b.principal p` — a behavior
  is allowed iff its principal is granted the permission. The Lean cases
  populate `grants : List PermissionGrantCase` per case
  (`Conformance.lean:282-328`).
- `expectedActorHostable` / `expectedPeerHostable` — set by
  `hostabilityDecision` at `Conformance.lean:206`, which calls
  `Deployment.canHostBehavior` (`Properties.lean:57`). That function
  evaluates `d.principal == b.principal` — a deployment can host a
  behavior iff their principals match.

The Rust deserializer `LeanIdentityPermissionCase` at
`crates/defra-agent/src/lean_vocab_test/command_identity_queue.rs:104-124`
already exposes all four fields. The conformance test at
`crates/defra-agent/tests/identity_conformance.rs:177` asserts the
principal-routing fields (`expected_actor_principal`,
`expected_peer_principal`, `same_principal`) and ignores the four
decision fields. The ledger row at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317-321`
records this as `consumerWithFollowUpCoverage` with the follow-up text:

> "Issue #193 replaces the Rust mirror in
> identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape
> with the runtime permission decision module and deployment hostability
> lookup."

Lean is shape-complete: the four fields are emitted, deterministic, and
deserialized. No Lean change is required.

## What is already true about the runtime

Two facts shape the design space.

**DefraDB ACP is the production permission decider.** The acp crate at
`/Users/johnzampolin/go/src/github.com/sourcenetwork/defradb.rs/crates/acp/src/lib.rs:43-57`
exports `DocumentACP`, `LocalDocumentACP`, `DocumentPermission`,
`Identity`, and `RelationTuple` directly. The trait at
`crates/acp/src/dac.rs:88-95` defines:

```rust
async fn check_doc_access(
    &self,
    identity: &Identity,
    permission: DocumentPermission,
    policy_id: &str,
    resource_name: &str,
    doc_id: &str,
) -> Result<bool>;
```

`Identity::Authenticated(did)` is the actor; `LocalDocumentACP` is an
in-memory implementation suitable for tests. Per the access rules at
`crates/acp/src/dac.rs:81-87`: unregistered docs allow all; anonymous
identity is denied; owners are allowed; otherwise the relation graph is
consulted. This is exactly the Lean `canonicalDecide` semantics applied
to a (DID, permission) grant store, instantiated on a concrete substrate.

#193 made this stance explicit
(`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:59-72`):

> "DefraDB ACP is the decider, and it is already DID-keyed. No new Rust
> decide function is needed; introducing one would be a parallel
> implementation of permissions that we'd then have to keep in sync with
> ACP. The runtime's contribution is routing: every site that today reads
> `behavior.identity` to sign a DB op switches to
> `behavior.principal.identity`. Two behaviors with the same principal
> supply the same `Identity::Authenticated(Did)` to ACP by construction,
> so ACP returns identical answers for them — that is the form
> `RespectsPrincipal` takes in this system."

**Hostability is structural principal equality.** Lean's
`Deployment.canHostBehavior` (`Properties.lean:57`) is
`d.principal == b.principal`. In the runtime, the daemon's deployment
principal DID is `agent_did`, and `apply.rs:24` already enforces
`principal.agent_did == agent_did` at the document-view loader. The
#193 scope reduction
(`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:30-57`)
collapsed `AgentDeployment` to "the installation IS the deployment" — one
process, one principal — so the hostability decision is the boolean
equality that the loader already checks.

The audit's "introduce a runtime permission-decision entry point and a
deployment hostability lookup" therefore reduces to: bind the conformance
witness to facilities that already exist, and consume the four Lean fields.

## Design options

### Option A — Test-only ACP-driven witness (zero new production code)

The conformance test gains a per-case harness that constructs a
`LocalDocumentACP`, translates `case.grants` into ACP relation tuples,
and calls `check_doc_access` for actor and peer behaviors. Hostability
is verified directly in the test as `host_deployment.principal == behavior.principal.agent_did`.
`apply.rs:24` is untouched.

Concrete shape:

1. Add the `acp` crate from defradb.rs to defra-agent's dev-dependencies
   (workspace already pins `crypto`, `defra-core`, `defra-node`,
   `identity` from the same source per `Cargo.toml:39-48`).
2. In `crates/defra-agent/tests/identity_conformance.rs`, add helpers:
   - `build_local_acp_from_lean_case(case: &LeanIdentityPermissionCase) -> LocalDocumentACP`
     that registers a minimal synthetic policy + one row document keyed
     by `case.row_owner`, then writes one `RelationTuple` per
     `case.grants[i]` mapping `(grant.principal, "reader", row_doc_id)`.
   - `acp_actor_for(behavior: &AgentBehavior) -> Identity` returning
     `Identity::Authenticated(Did::from(behavior.principal.agent_did.clone()))`.
3. Extend `identity_permission_cases_pin_runtime_permission_contract_shape`
   (`tests/identity_conformance.rs:177`) to additionally assert, for
   each Lean case:
   - `acp.check_doc_access(actor_identity, Read, policy_id, resource_name, row_doc_id).await? == case.expected_actor_allowed`
   - `acp.check_doc_access(peer_identity, Read, policy_id, resource_name, row_doc_id).await? == case.expected_peer_allowed`
   - `host_deployment.principal == actor.principal.agent_did` matches
     `case.expected_actor_hostable`
   - `host_deployment.principal == peer.principal.agent_did` matches
     `case.expected_peer_hostable`
4. Promote `Proofs/Conformance/CoverageLedger.lean:317-321` from
   `consumerWithFollowUpCoverage` to `consumerCoverage` and drop the
   follow-up text.

Pros:

- Zero new production code in defra-agent. No new module, no new trait,
  no new function. The "entry point" is `DocumentACP::check_doc_access`,
  which the runtime already routes through whenever a behavior signs a
  DB op.
- Does not reintroduce the parallel-to-ACP abstraction #193 spec
  explicitly rejected
  (`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:59-72`).
- Exercises the same `Identity::Authenticated(did)` shape that
  production uses. A regression in the production routing (e.g., a
  behavior emitting a different DID than its principal's) would also
  flip this test, because both paths derive the actor from
  `behavior.principal.agent_did`.
- Smallest delta: three files change.
- Conformance witness is self-contained — no DB, no defra-node, no
  network. `LocalDocumentACP` is in-memory.

Cons:

- The minimal synthetic policy is not DPI-validated. A drift in DefraDB's
  DPI policy requirements (e.g., new mandatory permission expressions)
  could pass this test while breaking real policies. Mitigation:
  validation of production policies happens elsewhere (DefraDB's own
  test suite at `crates/acp/tests/zanzibar_acp_tests.rs`); this test is
  scoped to the `(principal, permission)` decision shape, not policy
  authoring.
- Adds `acp` as a dev-dependency of defra-agent. It is already a
  transitive dependency via `defra-node`, so this is bookkeeping rather
  than a new build edge.
- Hostability is asserted directly in the test rather than through a
  named helper. A future reader looking for "the hostability lookup"
  finds an inline equality. Option B addresses this if reviewers want a
  named symbol.

### Option B — Option A + one named hostability helper

Same as A, plus extract the inline check at `apply.rs:24` into a
free function `pub(crate) fn deployment_hosts_behavior(deployment_principal_did: &str, behavior: &AgentBehavior) -> bool`
(~3 lines). `apply.rs` calls it; the conformance test calls it.

Where the helper lives: a new flat file
`crates/defra-agent/src/identity_decisions.rs` containing this single
function and nothing else, declared as `mod identity_decisions` in
`lib.rs`. No directory rename of `identity.rs` (it stays a flat 620-line
file of signing/identity-backend code, which is its actual purpose).

Pros over A:

- Satisfies the audit's literal "introduce a deployment hostability
  lookup" wording with a named symbol that grep finds.
- Single point of edit if hostability semantics evolve (e.g., when
  multi-principal-per-process becomes real per the #193 forward note at
  `docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:48-57`).
- 3 lines of production code; one extra file in the smallest-delta set.

Cons over A:

- One more file to maintain. The helper has only one production caller
  today; the conformance test is the second caller.
- The named symbol risks suggesting that "hostability" is a richer
  decision than equality, when today it is exactly equality. A future
  reader might expect to find more logic there than the function body
  contains.

### Option C — Standalone `identity/permission.rs` with `decide` + `can_host` mirrors

Reorganize `identity.rs` into a directory (`identity/mod.rs` +
`identity/permission.rs`). New module exposes two free functions:

```rust
pub fn decide(behavior: &AgentBehavior, permission: &str, grants: &[Grant]) -> bool;
pub fn can_host(deployment_principal_did: &str, behavior: &AgentBehavior) -> bool;
```

`decide` reimplements Lean's `canonicalDecide` in Rust:
`grants.iter().any(|g| g.principal_did == behavior.principal.agent_did && g.permission == permission)`.
The conformance test drives both functions. `apply.rs:24` calls `can_host`.

Pros:

- One named pair of symbols for the two concerns named by the audit.
- Mirrors the Lean `Proofs/Identity/` directory layout
  (`Permission.lean` + `Properties.lean` siblings of `State.lean`).
- No dependency on the `acp` crate at the conformance test layer.

Cons:

- **Reintroduces the parallel-to-ACP abstraction #193 spec explicitly
  rejected** (`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:59-72`).
  `decide` would be a `grants.iter().any(...)` reimplementation of what
  ACP already does on a real grant graph; keeping the two in sync is a
  maintenance liability with no production caller for `decide`.
- Larger delta (`identity.rs` → directory rename + new module + new
  re-export + apply.rs callsite + test consumption + ledger promotion =
  six files).
- A future change that makes `decide` async (when an ACP shim replaces
  the body) is a breaking change to the function signature with one
  production caller — manageable but a real friction point.
- The `Grant` value type duplicates ACP's `RelationTuple` shape on a
  smaller, defra-agent-local footprint.

## Recommendation: Option A

Choose A. The audit's "entry point" already exists as
`DocumentACP::check_doc_access`, the runtime is already wired through it
structurally per #193, and adding a parallel Rust decider creates the
exact synchronization burden #193 spec was written to avoid. Hostability
is principal equality enforced at the loader today; promoting it to a
named symbol is optional (Option B) and not required to consume the four
Lean fields.

A is also the smallest delta consistent with the audit text at
`docs/superpowers/audits/2026-05-19-conformance-audit.md:573-580`:

> "Introduce a runtime permission-decision entry point and a deployment
> hostability lookup, then consume `expected_actor_allowed` /
> `expected_peer_allowed` / `expected_actor_hostable` /
> `expected_peer_hostable` in
> `crates/defra-agent/tests/identity_conformance.rs:177`. Once both
> runtime modules exist and pass the four named Lean cases, promote
> `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317`
> from `consumerWithFollowUpCoverage` to `consumerCoverage`. Lean is
> already shape-complete; no Lean delta is needed."

The audit speaks of "runtime modules" plural; this design reads
"runtime entry point" as a runtime call site rather than necessarily a
new module. The call site is the conformance test's invocation of
`check_doc_access`, which reaches the same DefraDB ACP path production
DB ops reach. That is "runtime" in the sense of "exercises the runtime
permission flow"; it is not a new module.

If reviewers want a named hostability helper, fall back to Option B.
The choice is mechanical and can be made during implementation review;
it does not change the conformance shape.

## Smallest delta

Three files (Option A):

1. **`crates/defra-agent/tests/identity_conformance.rs`** — extend
   `identity_permission_cases_pin_runtime_permission_contract_shape`
   with the ACP-driven assertions for the four `expected_*` fields and a
   per-case `LocalDocumentACP` setup helper. Estimated ~80 LOC added,
   no existing assertions removed.

2. **`crates/defra-agent/Cargo.toml`** — add `acp` from defradb.rs as a
   dev-dependency. The workspace `Cargo.toml:39-48` already pins
   `defra-core`, `defra-node`, `identity` from the same git source;
   `acp` follows the same convention.

3. **`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`** —
   change line 317 from `consumerWithFollowUpCoverage` to
   `consumerCoverage`, remove the follow-up text at line 320.

If Option B is chosen, add a fourth file:

4. **`crates/defra-agent/src/identity_decisions.rs`** (new) — contains
   the single `pub(crate) fn deployment_hosts_behavior` helper, plus
   `mod identity_decisions` declaration in `lib.rs`. Edit
   `crates/defra-agent/src/agent/document_view/apply.rs:24` to call it.

No Lean spec changes. No production routing changes. No new schemas. No
new `AgentDeployment` Rust type.

## Conformance consequences

Concrete, asserted after implementation:

- `crates/defra-agent/tests/identity_conformance.rs:177` —
  `identity_permission_cases_pin_runtime_permission_contract_shape`
  drives `DocumentACP::check_doc_access` per Lean case, asserting
  agreement with `case.expected_actor_allowed`,
  `case.expected_peer_allowed`, `case.expected_actor_hostable`,
  `case.expected_peer_hostable` across all four cases produced by
  `identityPermissionCases` at
  `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:282-328`:
  - `same_principal_row_owner_grant_allows_shared_behaviors`
  - `separate_principal_without_grant_blocks_peer`
  - `separate_principal_with_grant_allows_peer`
  - `behavior_id_lookup_selects_declared_principal`

- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317-321`
  flips from:
  ```
  , consumerWithFollowUpCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape"
      "Issue #193 replaces the Rust mirror..."
  ```
  to:
  ```
  , consumerCoverage
      "identity_permission_cases"
      "IdentityPermissionCases"
      "identity_conformance::identity_permission_cases_pin_runtime_permission_contract_shape"
  ```

- The `cross-cutting drift test at
  crates/defra-agent/tests/state_machine_conformance/coverage.rs:391`
  (which enforces ledger ↔ snapshot agreement and zero unreferenced
  consumers per the audit at lines 38-43) continues to pass — the
  consumer string is unchanged.

- The §9 audit row "Top three actionable gaps" item 3 at audit lines
  67-74 closes. `identity_permission_cases` becomes Fully bound; #193's
  last open piece is closed.

- The Rust assertion at `identity_conformance.rs:268-340`
  (`identity_respects_principal_contract_enforced_by_runtime_routing`)
  is unaffected — it already runs against `target.enforced == true`
  per #193.

## Risks

| Risk | Mitigation |
|------|-----------|
| `Did` parse from a `did:agent:amy` Lean fixture string fails for `Did::from_str` because the `identity` crate's parser rejects unregistered DIDs | The Lean cases use synthetic DIDs that may not be valid wire-format DIDs. If `Did::from_str` is strict, the test harness wraps `behavior.principal.agent_did` as `Did::from_unchecked(...)` or uses a `Did::test_only(...)` constructor. Risk surfaces during implementation, not at design time; fall back to a `String`-typed actor field if `Did` requires a registered key (the conformance test never signs). |
| `LocalDocumentACP` API drift between defradb.rs revisions | The workspace pins a specific git rev for `defra-node`; `acp` will pin to the same rev. Pin churn is shared with other defra-node consumers. The drift test at `state_machine_conformance/coverage.rs:391` does not enforce acp-crate API stability, but the conformance test's compile failure on rev bump is loud and local. |
| Adding `acp` as a dev-dependency increases test build time | `acp` is already a transitive dependency of `defra-node`, which is already in defra-agent's dependency closure. The added bookkeeping is on the order of one extra crate name; the underlying compilation already happens. |
| Reviewers ask why "a runtime permission-decision entry point" doesn't introduce a new Rust module | The spec must be explicit (this section, plus the recommendation rationale) that the entry point already exists as `DocumentACP::check_doc_access`, that the runtime is structurally wired through it via signed DB ops per #193, and that adding a parallel Rust decider is the exact anti-pattern #193 spec was written to prevent. If the wording matters more than the substance, fall back to Option B for the hostability helper. |
| A future multi-principal-per-process implementation changes the hostability semantics from equality to set membership | The #193 forward note at spec lines 48-57 records this is a known future evolution. When it lands, `Deployment` becomes a real Rust type and `can_host_behavior` grows non-trivial — at which point Option B's named helper (or Option C's module) becomes attractive on its own merit, not for this audit row. |
| The conformance test grows DB-shaped without DB infrastructure | `LocalDocumentACP` is the in-memory, no-DB implementation. The test does not spin up `defra-node`, does not write to disk, and does not need P2P. Its surface is comparable to the existing `state_machine_conformance::generated_recovery_sweep_cases_drive_startup_recovery_contract` test in shape (in-memory test with structured per-case setup). |
| The Lean `permission` strings are formatted as `"row:<owner_did>:<resource>.<verb>"` and the test needs to parse them | The parser is a small string split inside the test harness; the format is fixed by the Lean fixture at `Conformance.lean:273-277`. If the format changes, both the Lean fixture and the Rust parser update together. Test failure is the canary. |

## Open questions

These resolve during implementation; the design holds either way.

- **Does the test set up one `LocalDocumentACP` per case, or one shared
  ACP with per-case policy ids?** One-per-case is simpler and more
  obviously isolated; one-shared is faster but adds policy-id
  bookkeeping. Recommendation: one-per-case unless a measurable
  test-time problem appears.
- **Should hostability be asserted via the inline `==` form, or through
  Option B's named helper?** Reviewer preference. The conformance shape
  is identical either way.
- **Should the permission string `"row:did:agent:amy:memory.read"` map
  to `DocumentPermission::Read` directly, or should the harness honor
  the suffix verb?** All four Lean cases use `.read`. Map to `Read`
  uniformly for the first implementation; if a Lean case adds an
  `.update`/`.delete` suffix later, the parser extends.

## What is not in scope

- Changing the Lean spec. It is shape-complete per the audit at
  `docs/superpowers/audits/2026-05-19-conformance-audit.md:73-74`.
- Adding a Rust-side permission decider, `Permissions` trait,
  `PermissionDecider`/`HostabilityResolver` traits, or any parallel
  implementation of ACP semantics in defra-agent.
- Refactoring `identity.rs` into a directory layout. It remains a flat
  file of signing/identity-backend code.
- Refactoring `config.rs` or the `AgentBehavior` struct.
- Adding an `AgentDeployment` Rust type, `Collection::AgentDeployment`,
  or any apply-reconcile validator for deployment rows. #193 already
  resolved deployment to be 1:1 with the running process's principal;
  the forward note at
  `docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md:48-57`
  records when this changes.
- An end-to-end DocumentACP integration test that drives
  `check_doc_access` against a live `defra-node` with real signed
  documents. That is a natural follow-on to this work but is not
  required to consume the four Lean fields or to promote the ledger row.
- Any change in the request hot path
  (`crates/defra-agent/src/agent/runtime/router.rs`). Per-request
  permission checks live at the ACP layer and fire when the runtime
  signs DB ops; no router-level gate is added.
- Anything in the #155 (P2P) icebox.
- Speculation about future per-deployment policy plugins beyond what is
  needed to evaluate the design options above.
- Implementing the conformance test changes, the dev-dependency edit,
  or the ledger promotion. This is a design-only PR; the implementation
  lands in a follow-up cycle.
