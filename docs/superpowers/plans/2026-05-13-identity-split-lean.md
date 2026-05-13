# Identity-Split Lean Module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Proofs/Identity/` Lean module that puts the `AgentPrincipal` / `AgentBehavior` / `AgentDeployment` boundary under proof, with structural conformance vectors and a deferred-enforcement contract for the future permission decision engine.

**Architecture:** Four Lean files (`State`, `Permission`, `Properties`, `Conformance`) under `Proofs/Identity/`, an aggregator `Identity.lean`, and one import line into `Proofs.lean`. Permission is a **type parameter** — engine-agnostic over Cedar / Zanzibar. The load-bearing predicate `RespectsPrincipal` says the decision factors through `b.principal`. Conformance emits structural witness cases (runnable today) and a named `identity.respects_principal_boundary` contract (enforced when the runtime permission engine lands per #193).

**Tech Stack:** Lean 4 + Lake (proof side), Rust + serde + cargo test (conformance side).

**Spec:** `docs/superpowers/specs/2026-05-13-identity-split-lean-design.md`.

**Issue:** #185. Parent tracker #183. Closes #185, refs #183 + #9 + #193.

**Coordination note:** Five sibling streams concurrently touch `crates/defra-agent/proofs/`. The only shared editing surface is `crates/defra-agent/proofs/Proofs.lean` (the import list). This plan adds **one line** to it. If a sibling lands first, rebase that one line.

---

## File Structure

| Path | Purpose | Status |
|------|---------|--------|
| `crates/defra-agent/proofs/Proofs/Identity/State.lean` | `Principal`, `Behavior`, `Deployment` records; `World`; `WellFormed` predicate | Create |
| `crates/defra-agent/proofs/Proofs/Identity/Permission.lean` | `GrantStore`, `Decide`, `RespectsPrincipal`, `canonicalDecide` + witness theorem | Create |
| `crates/defra-agent/proofs/Proofs/Identity/Properties.lean` | I1 sharing, I2 isolation, I3 no-escalation, I4 behavior-id-determines-principal, I5 co-hostable-share-principal | Create |
| `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean` | `IdentityStructuralCase` record, six structural witness cases, JSON serializer, `identity.respects_principal_boundary` contract declaration | Create |
| `crates/defra-agent/proofs/Proofs/Identity.lean` | Aggregator that imports the four submodules | Create |
| `crates/defra-agent/proofs/Proofs.lean` | Repository-wide import list | Modify (one line added: `import Proofs.Identity`) |
| `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` | `snapshotJson` aggregator | Modify (two keys added: `identity_structural_cases`, `identity_contracts`) |
| `crates/defra-agent/src/lean_vocab_test.rs` | Rust deserializers for the contract JSON | Modify (add `LeanIdentityStructuralCase`, `LeanIdentityContract`, extend `LeanContractSnapshot`, add extractor helpers) |
| `crates/defra-agent/tests/identity_conformance.rs` | Rust conformance consumer for Identity | Create |

Each file has one responsibility. `State` defines the entities; `Permission` defines the algebra; `Properties` proves the invariants; `Conformance` emits data. The aggregator pattern matches `Proofs/CommandPolicy.lean`.

---

## Task 1: Bootstrap module skeleton + Proofs.lean import

Establishes the file footprint and the one shared edit, before any proofs. Verifies the build is green so subsequent tasks have a known-good baseline.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Identity/State.lean`
- Create: `crates/defra-agent/proofs/Proofs/Identity/Permission.lean`
- Create: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`
- Create: `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean`
- Create: `crates/defra-agent/proofs/Proofs/Identity.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean` (add `import Proofs.Identity`)

- [ ] **Step 1.1: Create the four submodule files as empty stubs**

`Proofs/Identity/State.lean`:

```lean
/-!
# Identity — State

Records and well-formedness for the `AgentPrincipal` /
`AgentBehavior` / `AgentDeployment` split (#185).
-/

namespace Identity

end Identity
```

Repeat the same skeleton for `Permission.lean`, `Properties.lean`, `Conformance.lean`, changing only the doc-comment title (`Identity — Permission`, `Identity — Properties`, `Identity — Conformance`).

- [ ] **Step 1.2: Create the aggregator `Proofs/Identity.lean`**

```lean
import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Identity.Conformance

/-!
# Identity

Barrel for the `AgentPrincipal` / `AgentBehavior` / `AgentDeployment`
split model. See `docs/superpowers/specs/2026-05-13-identity-split-lean-design.md`.
-/
```

- [ ] **Step 1.3: Add the import to `Proofs.lean`**

Open `crates/defra-agent/proofs/Proofs.lean` and add `import Proofs.Identity` at the end of the existing import list (after `import Proofs.ReversePairingHandlers`).

- [ ] **Step 1.4: Build to confirm the skeleton is green**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds, no warnings about `Identity`.

- [ ] **Step 1.5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity \
        crates/defra-agent/proofs/Proofs/Identity.lean \
        crates/defra-agent/proofs/Proofs.lean
git commit -m "proofs: scaffold Proofs/Identity module (#185)"
```

---

## Task 2: State.lean — record types

The three structures from the spec. Schema parity (`displayName`, `enabled`) without any field that the permission predicate could legitimately depend on.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/State.lean`

- [ ] **Step 2.1: Write the records**

Replace the stub body (between `namespace Identity` and `end Identity`) with:

```lean
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
  principal   : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

structure Deployment where
  id        : DeploymentId
  principal : DID
  hostId    : String
  enabled   : Bool
  deriving DecidableEq, Repr
```

- [ ] **Step 2.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

- [ ] **Step 2.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/State.lean
git commit -m "proofs(Identity): add Principal, Behavior, Deployment records (#185)"
```

---

## Task 3: State.lean — World + WellFormed

Finite collection plus FK closure / id uniqueness. Property #4 from #185's acceptance falls out of `WellFormed`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/State.lean`

- [ ] **Step 3.1: Add the Mathlib import**

At the top of `State.lean`, before `namespace Identity`, add:

```lean
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
```

- [ ] **Step 3.2: Add `World` and `WellFormed` (inside the namespace, after the records)**

```lean
structure World where
  principals  : Finset Principal
  behaviors   : Finset Behavior
  deployments : Finset Deployment

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ p₂ : Principal, p₁ ∈ w.principals → p₂ ∈ w.principals →
      p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ b₂ : Behavior, b₁ ∈ w.behaviors → b₂ ∈ w.behaviors →
      b₁.id = b₂.id → b₁ = b₂) ∧
  (∀ d₁ d₂ : Deployment, d₁ ∈ w.deployments → d₂ ∈ w.deployments →
      d₁.id = d₂.id → d₁ = d₂) ∧
  (∀ b : Behavior, b ∈ w.behaviors →
      b.principal ∈ w.principals.image (·.did)) ∧
  (∀ d : Deployment, d ∈ w.deployments →
      d.principal ∈ w.principals.image (·.did))
```

- [ ] **Step 3.3: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

If the build complains about the `∈` notation on a `Finset`, check that `Mathlib.Data.Finset.Basic` is imported. If `.image (·.did)` fails, the alternate form is `Finset.image Principal.did w.principals`.

- [ ] **Step 3.4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/State.lean
git commit -m "proofs(Identity): add World + WellFormed predicate (#185)"
```

---

## Task 4: Permission.lean — abstract machinery

`GrantStore`, `Decide`, `RespectsPrincipal`, `canonicalDecide`. `Permission` is a free type parameter — the model commits to no vocabulary.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Permission.lean`

- [ ] **Step 4.1: Add the import and the machinery**

Replace the stub body with:

```lean
import Proofs.Identity.State

/-!
# Identity — Permission

Engine-agnostic permission decision interface. `Permission` is a
free type parameter — Cedar's `(action, resource)`, Zanzibar's
`(relation, object)`, or any future representation instantiates it
at the use site. The load-bearing predicate is `RespectsPrincipal`:
the decision must factor through `b.principal`. The slim `Behavior`
struct (see `State.lean`) enforces this by construction over
modeled fields.
-/

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

/-- Canonical decide: a behavior is allowed iff its principal is
    granted. Proves `RespectsPrincipal` is inhabited. -/
def canonicalDecide {Permission : Type} (g : GrantStore Permission) :
    Decide Permission :=
  fun b p => g.granted b.principal p

end Identity
```

- [ ] **Step 4.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

- [ ] **Step 4.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Permission.lean
git commit -m "proofs(Identity): add GrantStore, Decide, RespectsPrincipal, canonicalDecide (#185)"
```

---

## Task 5: Permission.lean — canonical witness theorem

Proves `canonicalDecide` satisfies `RespectsPrincipal`. The witness commit before the property theorems lean on it.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Permission.lean`

- [ ] **Step 5.1: Add the theorem inside `namespace Identity`, after `canonicalDecide`**

```lean
theorem canonicalDecide_respectsPrincipal
    {Permission : Type} (g : GrantStore Permission) :
    RespectsPrincipal (canonicalDecide g) := by
  intro b₁ b₂ p heq
  unfold canonicalDecide
  rw [heq]
```

- [ ] **Step 5.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds, zero `sorry`.

If `unfold` doesn't reduce, try `simp only [canonicalDecide, heq]` instead. If `rw [heq]` fails because the goal is already definitionally equal, replace the body with `simp [canonicalDecide, heq]`.

- [ ] **Step 5.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Permission.lean
git commit -m "proofs(Identity): prove canonicalDecide_respectsPrincipal (#185)"
```

---

## Task 6: Properties.lean — I1 sharing

The direct-application theorem. Same principal ⇒ same decision under any `RespectsPrincipal` decide.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`

- [ ] **Step 6.1: Add the import and the theorem**

Replace the stub body with:

```lean
import Proofs.Identity.State
import Proofs.Identity.Permission

/-!
# Identity — Properties

I1–I5: the load-bearing theorems for the AgentPrincipal /
AgentBehavior / AgentDeployment boundary.
-/

namespace Identity

/-- **I1 Sharing.** Any decide that respects the principal boundary
    gives the same answer for two behaviors with the same principal. -/
theorem sharing
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (heq : b₁.principal = b₂.principal) :
    decide b₁ p = decide b₂ p :=
  h b₁ b₂ p heq

end Identity
```

- [ ] **Step 6.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

- [ ] **Step 6.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Properties.lean
git commit -m "proofs(Identity): prove I1 sharing (#185)"
```

---

## Task 7: Properties.lean — I2 isolation (contrapositive of I1)

A permission divergence is observable evidence of a principal split.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`

- [ ] **Step 7.1: Add `isolation` inside `namespace Identity`, after `sharing`**

```lean
/-- **I2 Isolation** (contrapositive of I1). If two behaviors get
    different permission outcomes, they have different principals. -/
theorem isolation
    {Permission : Type} (decide : Decide Permission)
    (h : RespectsPrincipal decide)
    (b₁ b₂ : Behavior) (p : Permission)
    (hneq : decide b₁ p ≠ decide b₂ p) :
    b₁.principal ≠ b₂.principal := by
  intro heq
  exact hneq (h b₁ b₂ p heq)
```

- [ ] **Step 7.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

- [ ] **Step 7.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Properties.lean
git commit -m "proofs(Identity): prove I2 isolation (#185)"
```

---

## Task 8: Properties.lean — I3 no_escalation

Under the canonical construction, a behavior's effective decision is exactly its principal's grant. No `Behavior` field widens access.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`

- [ ] **Step 8.1: Add `no_escalation` inside `namespace Identity`, after `isolation`**

```lean
/-- **I3 No-escalation** (under the canonical construction).
    A behavior's effective decision is entirely determined by its
    principal's grants; no field of `Behavior` can widen access. -/
theorem no_escalation
    {Permission : Type} (g : GrantStore Permission)
    (b : Behavior) (p : Permission) :
    canonicalDecide g b p = g.granted b.principal p := rfl
```

- [ ] **Step 8.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

If `rfl` fails (it shouldn't — `canonicalDecide` is defined as the RHS), fall back to `by unfold canonicalDecide`.

- [ ] **Step 8.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Properties.lean
git commit -m "proofs(Identity): prove I3 no_escalation (#185)"
```

---

## Task 9: Properties.lean — I4 behavior_id_determines_principal

Behavior id is unique by `WellFormed`, so behavior_id → principal is a function.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`

- [ ] **Step 9.1: Add `behavior_id_determines_principal` inside `namespace Identity`, after `no_escalation`**

```lean
/-- **I4 Behavior-id functionally determines principal.** In any
    well-formed world, `Behavior.id` is unique and therefore
    `behavior_id → principal` is a function. Closes the
    "`(did, behavior_id)` uniqueness" criterion. -/
theorem behavior_id_determines_principal
    (w : World) (hw : w.WellFormed)
    (b₁ b₂ : Behavior)
    (h₁ : b₁ ∈ w.behaviors) (h₂ : b₂ ∈ w.behaviors)
    (hid : b₁.id = b₂.id) :
    b₁.principal = b₂.principal := by
  have hbeh := hw.2.1
  have heq : b₁ = b₂ := hbeh b₁ b₂ h₁ h₂ hid
  rw [heq]
```

- [ ] **Step 9.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

If `hw.2.1` doesn't project to the behavior arm, inspect the `WellFormed` shape with `#check @World.WellFormed` and adjust the projection chain (`hw.right.left`, `hw.left.right`, etc.). The behavior-uniqueness arm is the **second** conjunct of `WellFormed`, so projection is right-then-left.

- [ ] **Step 9.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Properties.lean
git commit -m "proofs(Identity): prove I4 behavior_id_determines_principal (#185)"
```

---

## Task 10: Properties.lean — I5 co_hostable_share_principal

The structural fence against accidental cross-principal co-location on a deployment.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Properties.lean`

- [ ] **Step 10.1: Add `Deployment.canHostBehavior` and `co_hostable_share_principal`**

Append to `namespace Identity`, after `behavior_id_determines_principal`:

```lean
/-- A deployment can host a behavior iff their principals match. -/
def Deployment.canHostBehavior (d : Deployment) (b : Behavior) : Bool :=
  d.principal == b.principal

/-- **I5 Deployment-hosting respects principal boundary.** Two
    behaviors hostable on the same deployment must share a principal.
    Discharges the "amy-general and amy-rumination cannot accidentally
    co-locate" constraint at the structural level. -/
theorem co_hostable_share_principal
    (d : Deployment) (b₁ b₂ : Behavior)
    (h₁ : d.canHostBehavior b₁ = true)
    (h₂ : d.canHostBehavior b₂ = true) :
    b₁.principal = b₂.principal := by
  unfold Deployment.canHostBehavior at h₁ h₂
  have e₁ : d.principal = b₁.principal := by
    simpa [beq_iff_eq] using h₁
  have e₂ : d.principal = b₂.principal := by
    simpa [beq_iff_eq] using h₂
  exact e₁.symm.trans e₂
```

- [ ] **Step 10.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

If `beq_iff_eq` is not available, replace each `simpa` with `simp [Deployment.canHostBehavior, beq_eq_true_iff] at h₁; exact h₁` (adjusted similarly for `h₂`). The underlying fact is `(a == b) = true ↔ a = b` for `DecidableEq` types.

- [ ] **Step 10.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Properties.lean
git commit -m "proofs(Identity): prove I5 co_hostable_share_principal (#185)"
```

---

## Task 11: Conformance.lean — case data + six structural cases

Six named scenarios from the spec, declared as Lean values. Each carries an expected `wellFormed : Bool` so Rust can mirror the verdict.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean`

- [ ] **Step 11.1: Replace the stub body with case structures and the case list**

```lean
import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties

/-!
# Identity — Conformance

Structural witness cases and the deferred-enforcement contract for
the runtime permission decision engine (tracked in #193).
-/

namespace Identity.Conformance

/-- Flat principal payload for JSON emission. -/
structure PrincipalCase where
  did     : String
  enabled : Bool
  deriving Repr

/-- Flat behavior payload for JSON emission. -/
structure BehaviorCase where
  id        : String
  principal : String
  enabled   : Bool
  deriving Repr

/-- Flat deployment payload for JSON emission. -/
structure DeploymentCase where
  id        : String
  principal : String
  hostId    : String
  enabled   : Bool
  deriving Repr

/-- One named scenario: a snapshot of principals/behaviors/deployments
    plus the expected `WellFormed` verdict. -/
structure IdentityStructuralCase where
  name        : String
  principals  : List PrincipalCase
  behaviors   : List BehaviorCase
  deployments : List DeploymentCase
  wellFormed  : Bool
  deriving Repr

def structuralCases : List IdentityStructuralCase :=
  [ { name        := "amy_general_and_amy_code_share_principal"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := "did:agent:amy", enabled := true }
        , { id := "amy-code",    principal := "did:agent:amy", enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "amy_rumination_separate_principal"
    , principals  :=
        [ { did := "did:agent:amy",        enabled := true }
        , { did := "did:agent:rumination", enabled := true } ]
    , behaviors   :=
        [ { id := "amy-general",     principal := "did:agent:amy",        enabled := true }
        , { id := "amy-rumination",  principal := "did:agent:rumination", enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := "did:agent:rumination"
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "dangling_behavior_fk_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "orphan", principal := "did:agent:ghost", enabled := true } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "duplicate_behavior_id_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := "did:agent:amy", enabled := true }
        , { id := "amy-general", principal := "did:agent:amy", enabled := false } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "deployment_fk_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   := []
    , deployments :=
        [ { id := "ghost-deploy"
          , principal := "did:agent:ghost"
          , hostId := "host-3.local"
          , enabled := true } ]
    , wellFormed  := false
    }
  , { name        := "two_deployments_different_principals_ok"
    , principals  :=
        [ { did := "did:agent:amy",        enabled := true }
        , { did := "did:agent:rumination", enabled := true } ]
    , behaviors   := []
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := "did:agent:rumination"
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  ]

end Identity.Conformance
```

- [ ] **Step 11.2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

- [ ] **Step 11.3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Conformance.lean
git commit -m "proofs(Identity): add structural conformance cases (#185)"
```

---

## Task 12: Conformance.lean — JSON serializer + contract declaration

The serializer matches the existing `Conformance.Contracts` JSON style. The contract declaration is one named property for deferred enforcement.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` (imports + helper visibility check)

- [ ] **Step 12.1: Open `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` and confirm the helpers `jsonString`, `jsonArray`, `boolString` are exported**

They're defined earlier in the same file and live under `namespace Conformance.Contracts`. The Identity conformance file will import them.

- [ ] **Step 12.2: Append to `Proofs/Identity/Conformance.lean` (inside `namespace Identity.Conformance`)**

Add this import line at the top of the file (after the existing imports):

```lean
import Proofs.Conformance.Contracts.Json
```

Append, after the `structuralCases` definition:

```lean
open Conformance.Contracts

def principalCaseJson (c : PrincipalCase) : String :=
  "{"
    ++ "\"did\":" ++ jsonString c.did ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def behaviorCaseJson (c : BehaviorCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def deploymentCaseJson (c : DeploymentCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"host_id\":" ++ jsonString c.hostId ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def identityStructuralCaseJson (c : IdentityStructuralCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"deployments\":" ++ jsonArray (c.deployments.map deploymentCaseJson) ++ ","
    ++ "\"well_formed\":" ++ boolString c.wellFormed
    ++ "}"

def structuralCasesJson : String :=
  jsonArray (structuralCases.map identityStructuralCaseJson)

/-- A named property the runtime permission engine must satisfy. -/
structure IdentityContract where
  name      : String
  statement : String
  enforced  : Bool
  trackedBy : String
  deriving Repr

def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "For any two AgentBehavior rows b₁, b₂ with " ++
        "b₁.agent_did == b₂.agent_did, the runtime's permission " ++
        "decision function MUST return identical results for any " ++
        "permission."
    , enforced  := false
    , trackedBy := "#193"
    }
  ]

def identityContractJson (c : IdentityContract) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"statement\":" ++ jsonString c.statement ++ ","
    ++ "\"enforced\":" ++ boolString c.enforced ++ ","
    ++ "\"tracked_by\":" ++ jsonString c.trackedBy
    ++ "}"

def identityContractsJson : String :=
  jsonArray (identityContracts.map identityContractJson)
```

- [ ] **Step 12.3: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds.

If `jsonString`/`jsonArray`/`boolString` are not found at the Identity import site, they may be declared in `Proofs/Conformance/ContractTypes.lean` rather than `Json.lean`. Adjust the import: `import Proofs.Conformance.ContractTypes`.

- [ ] **Step 12.4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Conformance.lean
git commit -m "proofs(Identity): add JSON serializers + RespectsPrincipal contract declaration (#185)"
```

---

## Task 13: Wire Identity into `snapshotJson`

Add two keys to the top-level snapshot so Rust can deserialize them.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`

- [ ] **Step 13.1: Add the import**

At the top of `Conformance/Contracts/Json.lean`, in the import block, add:

```lean
import Proofs.Identity.Conformance
```

- [ ] **Step 13.2: Add two keys to `snapshotJson`**

Open `snapshotJson` (search for `def snapshotJson : String :=`). Before the closing `++ "}"`, immediately after the existing `"coverage_ledger"` line, insert:

```lean
    ++ ",\"identity_structural_cases\":"
      ++ Identity.Conformance.structuralCasesJson
    ++ ",\"identity_contracts\":"
      ++ Identity.Conformance.identityContractsJson
```

The result inside `snapshotJson` should end with:

```lean
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ ",\"identity_structural_cases\":"
      ++ Identity.Conformance.structuralCasesJson
    ++ ",\"identity_contracts\":"
      ++ Identity.Conformance.identityContractsJson
    ++ "}"
```

- [ ] **Step 13.3: Build, then run the JSON emitter to eyeball the output**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean
```

Expected: a single JSON blob bracketed by `---BEGIN DEFRA LEAN CONTRACT JSON---` / `---END DEFRA LEAN CONTRACT JSON---`. Grep for `identity_structural_cases` and `identity_contracts` in the output — both should be present with six structural cases and one contract entry.

- [ ] **Step 13.4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean
git commit -m "proofs(Conformance): emit identity_structural_cases + identity_contracts in snapshotJson (#185)"
```

---

## Task 14: Rust extractor helpers in `lean_vocab_test.rs`

Add typed deserializers for the new JSON keys and a helper that pulls the structural cases out of the snapshot.

**Files:**
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`

- [ ] **Step 14.1: Add the new structs**

Locate the existing `#[derive(Debug, Deserialize)]` cluster (around line 26 onward, where `LeanContractSnapshot` is declared). Add the following structs in the same style (place them next to `LeanCommandPolicyCase`/`LeanLiveOverlayCase`, which are siblings in spirit):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPrincipal {
    pub(crate) did: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityBehavior {
    pub(crate) id: String,
    pub(crate) principal: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityDeployment {
    pub(crate) id: String,
    pub(crate) principal: String,
    pub(crate) host_id: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityStructuralCase {
    pub(crate) name: String,
    pub(crate) principals: Vec<LeanIdentityPrincipal>,
    pub(crate) behaviors: Vec<LeanIdentityBehavior>,
    pub(crate) deployments: Vec<LeanIdentityDeployment>,
    pub(crate) well_formed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityContract {
    pub(crate) name: String,
    pub(crate) statement: String,
    pub(crate) enforced: bool,
    pub(crate) tracked_by: String,
}
```

- [ ] **Step 14.2: Extend `LeanContractSnapshot`**

In the existing `LeanContractSnapshot` struct (around line 27), add two fields next to `coverage_ledger`:

```rust
    pub(crate) identity_structural_cases: Vec<LeanIdentityStructuralCase>,
    pub(crate) identity_contracts: Vec<LeanIdentityContract>,
```

- [ ] **Step 14.3: Add accessor helpers**

After the existing `lean_command_policy_case` / `lean_command_sandbox_case` helpers (search the file for `pub(crate) fn lean_command_policy_case`), add:

```rust
pub(crate) fn lean_identity_structural_cases() -> Vec<LeanIdentityStructuralCase> {
    lean_contract_snapshot().identity_structural_cases.clone()
}

pub(crate) fn lean_identity_contracts() -> Vec<LeanIdentityContract> {
    lean_contract_snapshot().identity_contracts.clone()
}
```

- [ ] **Step 14.4: Verify Rust compiles**

Run: `cargo check -p defra-agent`
Expected: clean compile. If serde complains about an unknown field, the JSON key in Lean and the Rust struct field disagree — recheck Task 13's two key names match `identity_structural_cases` and `identity_contracts` exactly.

- [ ] **Step 14.5: Commit**

```bash
git add crates/defra-agent/src/lean_vocab_test.rs
git commit -m "test-support: add Lean Identity case deserializers (#185)"
```

---

## Task 15: `tests/identity_conformance.rs` — structural cases test

The runnable conformance test. Each Lean case is reconstructed in Rust; a Rust mirror of `WellFormed` runs against it; the verdicts must agree.

**Files:**
- Create: `crates/defra-agent/tests/identity_conformance.rs`

- [ ] **Step 15.1: Write the failing test scaffold**

```rust
use std::collections::HashSet;

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_identity_structural_cases, LeanIdentityBehavior, LeanIdentityDeployment,
    LeanIdentityPrincipal, LeanIdentityStructuralCase,
};

/// Rust mirror of `Identity.World.WellFormed` from
/// `Proofs/Identity/State.lean`. Returns true iff:
///   - principal DIDs are unique
///   - behavior ids are unique
///   - deployment ids are unique
///   - every behavior.principal references an existing principal
///   - every deployment.principal references an existing principal
fn rust_well_formed(case: &LeanIdentityStructuralCase) -> bool {
    let principal_dids: HashSet<&str> =
        case.principals.iter().map(|p| p.did.as_str()).collect();
    if principal_dids.len() != case.principals.len() {
        return false;
    }

    let behavior_ids: HashSet<&str> =
        case.behaviors.iter().map(|b| b.id.as_str()).collect();
    if behavior_ids.len() != case.behaviors.len() {
        return false;
    }

    let deployment_ids: HashSet<&str> =
        case.deployments.iter().map(|d| d.id.as_str()).collect();
    if deployment_ids.len() != case.deployments.len() {
        return false;
    }

    if case
        .behaviors
        .iter()
        .any(|b: &LeanIdentityBehavior| !principal_dids.contains(b.principal.as_str()))
    {
        return false;
    }

    if case
        .deployments
        .iter()
        .any(|d: &LeanIdentityDeployment| !principal_dids.contains(d.principal.as_str()))
    {
        return false;
    }

    true
}

#[test]
fn identity_structural_cases_match_lean_verdicts() {
    let cases = lean_identity_structural_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit at least one identity structural case"
    );

    for case in &cases {
        let rust_verdict = rust_well_formed(case);
        assert_eq!(
            rust_verdict, case.well_formed,
            "case {:?}: Rust WellFormed = {}, Lean WellFormed = {}",
            case.name, rust_verdict, case.well_formed
        );
    }
}

#[test]
fn identity_structural_cases_cover_named_scenarios() {
    let cases = lean_identity_structural_cases();
    let names: HashSet<String> = cases.iter().map(|c: &LeanIdentityStructuralCase| c.name.clone()).collect();

    for expected in [
        "amy_general_and_amy_code_share_principal",
        "amy_rumination_separate_principal",
        "dangling_behavior_fk_violates",
        "duplicate_behavior_id_violates",
        "deployment_fk_violates",
        "two_deployments_different_principals_ok",
    ] {
        assert!(
            names.contains(expected),
            "missing expected identity conformance case: {expected}"
        );
    }
}

fn _suppress_unused_principal_field(p: &LeanIdentityPrincipal) -> bool {
    p.enabled
}
```

- [ ] **Step 15.2: Run the test — expect it to pass**

```bash
cargo test -p defra-agent --test identity_conformance --no-run
cargo test -p defra-agent --test identity_conformance identity_structural_cases_match_lean_verdicts -- --exact --nocapture
cargo test -p defra-agent --test identity_conformance identity_structural_cases_cover_named_scenarios -- --exact --nocapture
```

Expected: both tests pass. If `match_lean_verdicts` fails, eyeball the case name in the failure message and verify Lean and Rust agree on the FK closure logic (i.e., the Lean case's `wellFormed` is correct given the rules).

- [ ] **Step 15.3: Commit**

```bash
git add crates/defra-agent/tests/identity_conformance.rs
git commit -m "test(Identity): structural conformance — Rust mirrors Lean WellFormed verdicts (#185)"
```

---

## Task 16: `tests/identity_conformance.rs` — RespectsPrincipal contract presence

The deferred-enforcement test. Today it asserts the contract is **declared**; the future runtime permission engine PR flips it to assert the contract is **enforced**.

**Files:**
- Modify: `crates/defra-agent/tests/identity_conformance.rs`

- [ ] **Step 16.1: Extend the test file**

Add to the top of `identity_conformance.rs`, alongside the existing `use`:

```rust
use lean_vocab_test::{lean_identity_contracts, LeanIdentityContract};
```

Append at the bottom of the file:

```rust
#[test]
fn identity_respects_principal_contract_is_declared() {
    let contracts = lean_identity_contracts();
    let target = contracts
        .iter()
        .find(|c: &&LeanIdentityContract| c.name == "identity.respects_principal_boundary")
        .expect(
            "Lean must emit the identity.respects_principal_boundary contract \
             — this is the spec the future runtime permission engine (#193) lands against",
        );

    // The contract is declared today and not yet enforced by a runtime
    // permission decision module. When that module lands (#193), flip
    // `enforced` to `true` in Proofs/Identity/Conformance.lean AND
    // replace the assertion below with a property-based test driving
    // the runtime decide function on the structural cases.
    assert!(
        !target.enforced,
        "identity.respects_principal_boundary is marked enforced=true in Lean, \
         but the Rust runtime permission decision module is not yet wired up. \
         Either revert the Lean flag or extend this test to drive the runtime."
    );
    assert_eq!(
        target.tracked_by, "#193",
        "tracked_by must point at the runtime-refactor tracker so the deferred \
         enforcement has a discoverable owner"
    );
    assert!(
        target.statement.contains("agent_did"),
        "contract statement must mention agent_did so a reader unfamiliar with the \
         Lean model can grasp the boundary; statement was: {}",
        target.statement
    );
}
```

- [ ] **Step 16.2: Run the new test**

```bash
cargo test -p defra-agent --test identity_conformance identity_respects_principal_contract_is_declared -- --exact --nocapture
```

Expected: PASS. If the JSON field name is wrong, serde reports an "unknown field" or "missing field"; recheck Task 13's keys.

- [ ] **Step 16.3: Run the whole test file once more**

```bash
cargo test -p defra-agent --test identity_conformance
```

Expected: 3 tests pass.

- [ ] **Step 16.4: Commit**

```bash
git add crates/defra-agent/tests/identity_conformance.rs
git commit -m "test(Identity): assert RespectsPrincipal contract is declared (deferred enforcement, #193) (#185)"
```

---

## Task 17: Final build + open PR

End-to-end verification before publishing.

**Files:** None.

- [ ] **Step 17.1: Full Lean build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: success, zero `sorry`. Verify with:

```bash
cd crates/defra-agent/proofs && grep -rn "sorry" Proofs/Identity/
```

Expected: no matches (or only matches inside string literals, which `grep` won't have).

- [ ] **Step 17.2: Full Rust check + targeted tests**

```bash
cargo check -p defra-agent --tests
cargo test -p defra-agent --test identity_conformance
```

Expected: clean compile, all 3 Identity tests pass.

- [ ] **Step 17.3: Run the broader conformance test suite to confirm nothing regressed in `snapshotJson`**

```bash
cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain -- --exact
cargo test -p defra-agent --test state_machine_conformance lean_deviation_metadata_is_empty_or_explicitly_classified -- --exact
```

Expected: both pass. These exercise the snapshot's overall shape; if the Identity keys broke deserialization of the snapshot, they fail loudly.

- [ ] **Step 17.4: Push the branch and open the PR**

```bash
git push -u origin proofs/issue-185-identity-split
gh pr create --title "Add Lean model for AgentPrincipal / AgentBehavior / AgentDeployment split" --body "$(cat <<'EOF'
Closes #185
Refs #183 (parent tracker)
Refs #9 (the original refactor; closed — describes the desired model)
Refs #193 (runtime refactor tracker — lands against this Lean contract)

## Summary

Adds `Proofs/Identity/` — a four-file Lean module that puts the principal/behavior/deployment boundary under proof. Closes audit gap #5 from `docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`.

### Structures (`Proofs/Identity/State.lean`)
- `Principal` (DID-backed identity, permission/audit boundary)
- `Behavior` (id + principal FK; slim by design — no prompt/tool/backend fields)
- `Deployment` (id + principal FK + opaque `hostId`)
- `World` + `WellFormed` (FK closure, id uniqueness)

### Permission boundary (`Proofs/Identity/Permission.lean`)
- `Permission` is a free type parameter — engine-agnostic over Cedar / Zanzibar
- `RespectsPrincipal (decide)` — the load-bearing predicate
- `canonicalDecide` — witness that the predicate is inhabited

### Invariants (`Proofs/Identity/Properties.lean`)
- **I1 sharing** — same principal ⇒ same permissions under any `RespectsPrincipal` decide
- **I2 isolation** — different permission outcomes ⇒ different principals (contrapositive of I1)
- **I3 no_escalation** — `Behavior` fields cannot widen access (under canonical construction)
- **I4 behavior_id_determines_principal** — `(did, behavior_id)` uniqueness
- **I5 co_hostable_share_principal** — deployments are principal-scoped; cross-principal co-location is rejected structurally

### Conformance (`Proofs/Identity/Conformance.lean`)
- **Six structural witness cases** emitted as JSON, consumed by `tests/identity_conformance.rs`:
  - amy-general + amy-code (shared principal) — `WellFormed = true`
  - amy-rumination (separate principal) — `WellFormed = true`
  - dangling behavior FK — `WellFormed = false`
  - duplicate behavior id — `WellFormed = false`
  - deployment FK violation — `WellFormed = false`
  - two deployments, different principals — `WellFormed = true`
- **`identity.respects_principal_boundary`** — named property declared today, marked `enforced=false`, `tracked_by="#193"`. When the runtime permission engine lands per #193, this flips to `enforced=true` and the conformance test extends to property-based testing against the runtime decide function.

### Modeling boundaries (intentional out-of-scope)
- **Authentication** (#180) — claim verification, not boundary semantics
- **Deployment routing** — the routing memory's "(did, behavior_id) lives on exactly one deployment" assumption is taken as given
- **CommandPolicy** — `Proofs/CommandPolicy/*` is the downstream argv/sandbox layer; composition with principal grants is future work
- **Permission engine internals** — Cedar / Zanzibar specifics are out; the proof is parametric over `Permission`

### Lead vs follow
The Lean model **leads** the Rust refactor. The runtime `DefaultAgent` (`crates/defra-agent/src/agent.rs:86`) still conflates principal + behavior; #193 tracks the refactor that lands against this Lean contract. No Rust permission engine exists today; `RespectsPrincipal` is the contract Cedar/Zanzibar wiring must satisfy when it ships.

## Test plan
- [x] `lake build` succeeds in `crates/defra-agent/proofs/` with zero `sorry`
- [x] `cargo test -p defra-agent --test identity_conformance` — 3 tests pass
- [x] `cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain` — broader snapshot deserialization unchanged
- [x] `grep -rn "sorry" Proofs/Identity/` — no matches

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. Report it back.

---

## Self-review

Spec coverage check:

| Spec requirement | Plan task(s) |
|---|---|
| Module shape (5 Lean files + Proofs.lean line) | Task 1 |
| `Principal`, `Behavior`, `Deployment` records | Task 2 |
| `World` + `WellFormed` | Task 3 |
| `RespectsPrincipal` is strict (slim Behavior enforces by construction) | Task 2 (slim Behavior), Task 4 (predicate definition) |
| Canonical witness | Tasks 4 + 5 |
| I1 sharing | Task 6 |
| I2 isolation | Task 7 |
| I3 no_escalation | Task 8 |
| I4 behavior_id_determines_principal | Task 9 |
| I5 co_hostable_share_principal | Task 10 |
| Six structural witness cases | Task 11 |
| JSON serializers | Task 12 |
| `identity.respects_principal_boundary` contract declaration | Task 12 |
| Wire into `snapshotJson` | Task 13 |
| Rust deserializers + helpers | Task 14 |
| Structural conformance test | Task 15 |
| Contract-presence test referencing #193 | Task 16 |
| Zero `sorry` | Task 17 (final grep) |
| PR body acceptance checks | Task 17 |

No placeholders. All code blocks contain complete content. Type names consistent across tasks (`Principal`/`Behavior`/`Deployment`/`World`/`WellFormed`/`Decide`/`RespectsPrincipal`/`canonicalDecide` in Lean; `LeanIdentityPrincipal`/`LeanIdentityBehavior`/`LeanIdentityDeployment`/`LeanIdentityStructuralCase`/`LeanIdentityContract` in Rust). Property names in Lean (`sharing`/`isolation`/`no_escalation`/`behavior_id_determines_principal`/`co_hostable_share_principal`) match between the design spec, the proof file, and the PR body.

Fallback tactics are noted inline for the Lean proofs that might need iteration (Tasks 5, 8, 9, 10). The plan does not introduce `sorry` anywhere — fallbacks adjust tactic syntax, not proof completeness.
