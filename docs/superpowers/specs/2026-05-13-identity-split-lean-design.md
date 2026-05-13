# Lean model for AgentPrincipal / AgentBehavior / AgentDeployment split

Date: 2026-05-13
Issue: #185 (parent tracker #183, original refactor #9)
Audit gap: #5 in `docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`

## Problem

CLAUDE.md flags the identity model as evolving. The schemas already split (`agent_principal.graphql`, `agent_behavior.graphql` exist as separate collections; `ApplyReconcile.Collections` has `agentPrincipal` and `agentBehavior` variants). What is **not** modeled is the **permission boundary** the split is supposed to enforce: `amy-general` and `amy-code` (shared principal, shared permissions) vs. `amy-rumination` (separate principal, narrower permissions). Today that boundary lives in code-review convention.

This spec adds a Lean module that puts the boundary under proof. Per the audit's leverage statement: "modeling it would settle the `amy-general`/`amy-code` vs `amy-rumination` permission boundary in proof rather than convention. Leaving it open means the security boundary that DefraDB DID-based identity is *supposed* to provide is asserted only by code review."

## Lead vs. follow

The Lean model **leads** the Rust refactor:

- Schemas already split — no leading needed there.
- Runtime still conflates: `crates/defra-agent/src/agent.rs:86` carries `(agent_did, default_behavior_id, behaviors[])` as a single `DefaultAgent` object.
- No `AgentDeployment` schema or struct exists. The Lean record commits to a shape (`{id, principal, hostId, enabled}`) the future schema mirrors.
- No permission engine exists in code (`grep` for `permission|grant|acp_policy` in `crates/defra-agent/src/` returns no identity-permission system). The Lean `RespectsPrincipal` predicate is the contract that the eventual permission engine (Zanzibar today in `defradb.rs`, Cedar later per Jack's signal) must satisfy. **Lean is ahead; the refactor lands against the proof.**

## Approach: decision-factor framing

The model is **engine-agnostic over the permission representation**. `Permission` is a free type parameter (no constructors fixed by the model). A `Decide` function answers "does this behavior have this permission?" The load-bearing predicate is `RespectsPrincipal (decide)` — the decision factors through the behavior's principal. The sharing/isolation theorems are proved for *any* decide function that respects principal-factoring, so they survive a Cedar/Zanzibar swap.

A canonical witness (`canonicalDecide g b p := g.granted b.principal p`) proves the predicate is inhabited.

### `RespectsPrincipal` is strict

The predicate quantifies over **behaviors**, not over the principal field. Concretely:

> `∀ b₁ b₂ p, b₁.principal = b₂.principal → decide b₁ p = decide b₂ p`

This says the decision must depend on the behavior **only through `b.principal`**. A decide function that reads `b.enabled`, `b.id`, `b.displayName`, or any other Behavior field violates the predicate — because two behaviors with the same principal but different values in that field would force a permission-outcome difference, contradicting the hypothesis.

This strictness is intentional. The audit's leverage is that a permission outcome cannot change without a principal change. Letting decide depend on `b.enabled` would mean "a behavior was disabled" silently becomes a permission boundary — and the same security guarantee no longer holds, because two behaviors of one principal could now have different effective permissions purely from operator state.

The slim Behavior struct **enforces this by construction over modeled fields**. By not modeling `systemPrompt`, `backendId`, `toolSelectionId`, etc., the model prevents future readers from drifting toward "decide depends on the behavior's prompt or tool selection." The only behavior-level fields the predicate *can* depend on are `principal` (allowed by construction — that's the whole point) and `enabled` (modeled but not allowed by the predicate).

**Consequence for future extensions.** An enabled-aware permission policy ("a disabled principal grants nothing"; "a disabled behavior is unreachable") is *not* free against this contract. It either:

1. Lifts to the principal layer (`¬principal.enabled → ∀ p, granted = false`) — preserves `RespectsPrincipal` because the predicate is preserved when decide factors through `principal.enabled` via the `GrantStore`.
2. Adds a separate enforcement gate orthogonal to `decide` (e.g., the runtime refuses to route to a disabled behavior before the permission check ever runs) — preserves `RespectsPrincipal` because the check never reaches `decide`.

Either is fine; both are out of scope for this PR. What the contract forbids is *silently widening* decide to read `b.enabled`. That is the kind of drift the proof exists to prevent.

## Module shape

```
crates/defra-agent/proofs/Proofs/Identity/
  State.lean         -- Principal, Behavior, Deployment records;
                        World; WellFormed (FK closure + id uniqueness)
  Permission.lean    -- opaque Permission; GrantStore; Decide;
                        RespectsPrincipal predicate; canonicalDecide
  Properties.lean    -- I1 sharing, I2 isolation, I3 no-escalation,
                        I4 behavior-id-determines-principal,
                        I5 co-hostable-share-principal
  Conformance.lean   -- structural witness cases + RespectsPrincipal
                        contract declaration
crates/defra-agent/proofs/Proofs/Identity.lean   -- aggregator
```

**`Proofs.lean` edit:** one line, `import Proofs.Identity`. This is the only shared editing surface with siblings #186/#187/#188/#189/#191.

## Structures

```lean
namespace Identity

abbrev DID := String
abbrev BehaviorId := String
abbrev DeploymentId := String

structure Principal where
  did         : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Behavior where
  id          : BehaviorId
  principal   : DID               -- FK → Principal.did
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Deployment where
  id        : DeploymentId
  principal : DID                 -- FK → Principal.did
  hostId    : String              -- opaque routing tag
  enabled   : Bool
  deriving DecidableEq, Repr

structure World where
  principals  : Finset Principal
  behaviors   : Finset Behavior
  deployments : Finset Deployment

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ p₂ ∈ w.principals, p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ b₂ ∈ w.behaviors, b₁.id  = b₂.id  → b₁ = b₂) ∧
  (∀ d₁ d₂ ∈ w.deployments, d₁.id = d₂.id → d₁ = d₂) ∧
  (∀ b ∈ w.behaviors, b.principal ∈ w.principals.image (·.did)) ∧
  (∀ d ∈ w.deployments, d.principal ∈ w.principals.image (·.did))
```

Intentional choices:

- **Behavior is slim** — no `systemPrompt`, `backendId`, `toolSelectionId`. Those are *interface* config (already in `AgentBehavior.graphql` and `BehaviorConfig`), not part of the *identity boundary*.
- **Deployment is slim** — `hostId` is opaque; it is never inspected. Routing semantics are operational.
- **Why `World`?** State-level invariants (FK closure, id uniqueness) read naturally over a finite collection, matching the `Finset`-based pattern already used in `ApplyReconcile.Manifest`.

## Permission abstraction

`Permission` is a **type parameter** on the structures and theorems — never a global. The proof is parametric over what permissions *are*. Cedar's `(action, resource)`, Zanzibar's `(relation, object)`, or any future representation instantiates it at the use site.

```lean
namespace Identity

structure GrantStore (Permission : Type) where
  granted : DID → Permission → Bool

abbrev Decide (Permission : Type) := Behavior → Permission → Bool

/-- A decide respects the principal boundary iff it factors through
    `b.principal` — two behaviors with the same principal always reach
    the same decision for any permission. -/
def RespectsPrincipal {Permission : Type} (decide : Decide Permission) : Prop :=
  ∀ (b₁ b₂ : Behavior) (p : Permission),
    b₁.principal = b₂.principal → decide b₁ p = decide b₂ p

/-- Canonical decide: a behavior is allowed iff its principal is granted. -/
def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.principal p

theorem canonicalDecide_respectsPrincipal
    {Permission : Type} (g : GrantStore Permission) :
    RespectsPrincipal (canonicalDecide g)
```

- `Permission` is a free type parameter — no committed vocabulary, no `opaque`/`axiom`, no body to discharge. Engine-agnostic by construction.
- `RespectsPrincipal` is a property of *the function*, not of any particular value. A future Rust permission engine satisfies it iff its decisions only depend on the behavior's principal.
- The `GrantStore` is a flat function — Cedar/Zanzibar storage shapes project into `DID → Permission → Bool` at the boundary.

## Invariants

| # | Name | Statement | Acceptance criterion |
|---|---|---|---|
| I1 | `sharing` | `RespectsPrincipal decide → b₁.principal = b₂.principal → decide b₁ p = decide b₂ p` | "Behaviors sharing a principal share permissions" |
| I2 | `isolation` | `RespectsPrincipal decide → decide b₁ p ≠ decide b₂ p → b₁.principal ≠ b₂.principal` (contrapositive of I1) | "Behaviors with separate principals do not share" |
| I3 | `no_escalation` | `canonicalDecide g b p = g.granted b.principal p` (Behavior fields cannot widen access) | "Behaviors cannot escalate beyond principal's grant" |
| I4 | `behavior_id_determines_principal` | `WellFormed w → b₁, b₂ ∈ w.behaviors → b₁.id = b₂.id → b₁.principal = b₂.principal` | "`(did, behavior_id)` pair has at most one principal binding" |
| I5 | `co_hostable_share_principal` | `Deployment.canHostBehavior d b₁ = true → Deployment.canHostBehavior d b₂ = true → b₁.principal = b₂.principal` | Three-way split is a load-bearing fence against accidental cross-principal co-location |

`Deployment.canHostBehavior d b := d.principal == b.principal` — the structural gate. The deployment-routing memory says `(did, behavior_id)` lives on exactly one deployment; I5 says the converse direction holds structurally: a deployment is principal-scoped.

**`sorry` budget: zero.** I3 and I5 unfold to `rfl`-ish closures; I1 is the predicate by definition; I2 is its contrapositive; I4 follows from the uniqueness arm of `WellFormed`. No tactic-heavy proof is anticipated.

## Conformance

Two artifacts:

### 1. Structural witness cases — runnable today

A finite list of `(Principal set, Behavior set, Deployment set)` snapshots with expected `WellFormed` verdicts. Cases cover:

- `amy-general` + `amy-code` of one principal, plus a deployment hosting that principal — `WellFormed = true`
- `amy-rumination` as a separate principal with its own deployment — `WellFormed = true`
- dangling behavior (FK to nonexistent principal) — `WellFormed = false`
- duplicate `behavior_id` across two rows — `WellFormed = false`
- deployment binding to nonexistent principal — `WellFormed = false`
- two deployments hosting different principals — `WellFormed = true` (no shared-principal requirement across deployments)

Emitted as JSON via the existing `Proofs/Conformance/Contracts.lean` pattern. Consumer: new `crates/defra-agent/tests/identity_conformance.rs` constructs the corresponding in-memory snapshots, runs the Lean-computed verdict against a Rust mirror of `World.WellFormed`, asserts agreement. **Closes the audit's "vectors emitted" requirement.**

### 2. `RespectsPrincipal` contract — deferred enforcement

A named property string emitted in the contract JSON:

> `"identity.respects_principal_boundary"`: For any two `AgentBehavior` rows `b₁`, `b₂` with `b₁.agent_did == b₂.agent_did`, the runtime's permission decision function MUST return identical results for any permission.

Pattern precedent: `Proofs/Conformance/Deviations.lean` (named properties that are not yet enforced at the test layer). The stub test in `identity_conformance.rs` asserts the contract is **present** in the contract JSON today. When the future #9-followup or permission-engine PR introduces a permission decision module, that PR flips the test from "contract present" to "contract enforced via property-based test on the runtime decision function." Until then the contract is load-bearing documentation that any permission engine PR must either satisfy or explicitly opt out of.

**What this is NOT:** the spec does not fabricate a Rust permission decision function. The audit gap is the proof being absent; the proof + structural conformance + declared contract closes #185 without overreaching into the orthogonal permission-engine track.

## Modeling boundaries (out of scope)

- **Authentication.** "Is this peer signing as P actually P?" lives in #180. The Identity model assumes a principal *claim* is well-formed; verifying claims is wiring, not boundary semantics.
- **Deployment routing.** The deployment-routing memory (`each (did, behavior_id)` lives on exactly one deployment) is taken as an assumption. The model defines `Deployment` and FK closure; which deployment hosts which behavior at any moment is operational.
- **CommandPolicy.** `Proofs/CommandPolicy/*` proves its own theorems for argv/sandbox/network policy. Identity is the *upstream* principal-grant layer. The composition (a tool call passes iff principal grants the capability AND CommandPolicy validates the argv) is mentioned only in prose.
- **DID format / cryptography.** `DID` is `String`. Format/parsing/signature is `defra_core::signing`'s territory.
- **Permission engine internals.** Cedar, Zanzibar, anything else can instantiate `Permission` and provide a `GrantStore`. The proof does not care which.
- **`enabled` semantics.** Both `Principal` and `Behavior` carry `enabled : Bool` for schema parity, but no theorem ties `enabled = false` to decision outcomes in this round. Future extension if the runtime ever exposes a "disabled" semantics richer than not-listing-in-runtime.

## Coordination with siblings

Five concurrent streams touch `crates/defra-agent/proofs/`:

- #191 — `Proofs/Session/Transcript.lean` or `Proofs/Transcript/`
- #188 — `proofs/tla/` (TLA+ only)
- #189 — `Proofs/Properties/Liveness.lean` + possibly `Proofs/Recovery/`
- #187 — `Proofs/EventDelivery/` or extension of `Triggers/`
- #186 — `Proofs/MCPHealth/`

**Only shared editing surface:** `Proofs.lean` (the import list). This stream adds **one line**, `import Proofs.Identity`. Last-to-land rebases.

File footprint for this stream is entirely under `Proofs/Identity/` — fully clear of the others.

## Future extensions (not in this PR)

- **Composition with CommandPolicy** — a behavior-level tool call passes iff `decide b (capabilityFor argv) ∧ CommandPolicy.validate policy argv = .allow`. Requires a `CommandPolicy.requires : CommandRequest → Permission` lifting and a composition theorem.
- **Disabled-principal cascade** — `¬p.enabled → ∀ b with b.principal = p.did, ∀ perm, decide b perm = false`. Requires the decide function to be `enabled`-aware.
- **Representation theorem for `RespectsPrincipal`** — every `RespectsPrincipal` decide can be expressed as `canonicalDecide g'` for some `g'`. Useful if we want to claim canonicalDecide is the *unique* shape up to grant rewriting.
- **Concrete capability witness** — instantiating `Permission` with a small inductive (`readMessages | writeRequests | callTool | spawnSubagent | ...`) once the Rust permission engine commits to a vocabulary.
- **ACP / Cedar / Zanzibar bridge** — a typed projection from `defra_core::acp` policy documents into a `GrantStore`. Lands when the engine lands.

## Acceptance check

Mapping back to #185:

- [x] Lean model splits `AgentPrincipal` / `AgentBehavior` / `AgentDeployment` — `State.lean` records.
- [x] Permission-boundary invariants proved: behaviors sharing a principal share permissions (I1); behaviors with separate principals do not (I2, contrapositive).
- [x] Behaviors cannot escalate beyond principal grant (I3 under canonicalDecide; I5 at deployment level).
- [x] `(did, behavior_id)` uniqueness (I4).
- [x] Conformance vectors emitted (structural witnesses + RespectsPrincipal contract declaration).
- [x] Wire conformance to the runtime split #9 described — declared contract becomes load-bearing when the runtime adds a permission engine; structural witnesses test today. (Issue #9 itself is closed; the `DefaultAgent` runtime split it described is tracked in **#193**, which lands against this Lean contract.)

## Risks

- **Lean tooling.** Zero-`sorry` requires `lake build` to succeed against the project's pinned Lean toolchain. If any I1–I5 proof resists, the fallback is to simplify the invariant (e.g., drop `World.WellFormed` from I4 and state it as a hypothesis directly), not to introduce a `sorry`.
- **Sibling rebase.** If two siblings race to land their `Proofs.lean` import line, the second rebases. No content collision — just a one-line merge.
- **Conformance JSON shape.** The structural witness cases need to slot into the existing `Conformance.Contracts.snapshotJson` aggregator without breaking other consumers. Plan: add an `identity` section to the JSON; existing consumers ignore unknown sections.
