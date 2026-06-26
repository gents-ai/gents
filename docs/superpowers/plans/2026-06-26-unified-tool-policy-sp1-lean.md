# Unified Tool Policy — SP1-Lean Implementation Plan (v2, post-review)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Lean formal model of unified tool-policy composition — a per-category meet-based composition (commutative + idempotent atoms; **lower-bound safety** is the load-bearing property) with **structured (keyed key/value) endpoint scopes**, a **full-field bash product** (all six fields, incl. mode + read-only allowlist) proven sound, and the theorem that the effective tool surface is a subset of the operator ceiling (and the behavior policy) for every category — plus a Rust conformance mirror that **re-derives** the effective surface from emitted inputs and checks it against the Lean-emitted expected output. (Full meet *associativity* is intentionally out of scope — `effective` fixes the composition order `(behavior ⊓ ceiling) ⊓ runtime`, so associativity is not load-bearing; see Task 10.)

**Architecture:** A new top-level Lean model barrel `Proofs/ToolPolicy.lean` (imported from `Proofs.lean`) with submodules `ToolPolicy/{Types,Meet,Theorems,Instances,Cases}.lean`, mirroring the `CommandPolicy/` layout. Endpoint categories use a generic `EndpointScope K V` driven by a `ValueMeet V` record (value-narrowing lower bound), instantiated per category (write_tools, cli, subagent, and the simple `Unit`-valued ones). Bash is a modeled `BashPolicy` structure whose allowed/read-only gates are `EndpointScope`s (so the empty-list=allow-all wire semantics is represented soundly, and `Only(∅)`=deny-all stays distinct from `All`). Conformance: `Contracts.lean` emits inputs+expected output as JSON; a Rust meet mirror (`tool_policy_mirror.rs`) re-derives and asserts equality. SP1-Rust later deletes the mirror and points the real resolver at the same cases.

**Tech Stack:** Lean 4 `v4.18.0` + Mathlib `v4.18.0` (`Finset` concrete ops `∩`/`⊆`, no Mathlib lattice typeclasses); Rust conformance via `cargo test -p defra-agent`.

## Global Constraints

- **Lean toolchain:** `leanprover/lean4:v4.18.0`; Mathlib pinned `v4.18.0` (`proofs/lakefile.lean`, `proofs/lean-toolchain`). Do not bump.
- **Zero `sorry`s / `admit`s.** Every theorem complete. No `native_decide` shortcut on safety theorems.
- **Build gate:** `cd crates/defra-agent/proofs && lake build` clean before any `proofs/` commit.
- **Conformance gate:** `cargo test -p defra-agent` (FULL package suite, never `--lib`).
- **`autoImplicit false`** is package-wide; declare all implicit binders explicitly.
- **Root barrel is `crates/defra-agent/proofs/Proofs.lean`** (there is NO `Proofs/DefraAgent.lean`). New models are top-level barrels `Proofs/<Name>.lean` imported there; the structure fence (`tests/conformance/structure.rs:122` `proofs_models`) treats every `Proofs/<Name>.lean` as a model requiring a `model_homes()` entry.
- **Snapshot JSON keys + Rust serde are snake_case** (e.g. `apply_reconcile_cases`), no `rename_all`. Match this exactly.
- **Empty-list=allow-all soundness:** an empty `allowed_argv_prefixes` wire list means *allow-all* (`command.rs:288`); but the **meet** of two non-overlapping non-empty allow-lists is `Only(∅)` = *deny-all*, which is NOT representable as an empty wire list. The model therefore carries bash gates as `EndpointScope`, and `Only(∅)` ≠ `All`. Representing `Only(∅)` back on the wire is an explicit SP1-Rust concern (noted in Task 18).
- **Four pre-flight constraints** (spec `2026-06-26-unified-tool-policy-design.md`): (1) agent_did-scoped load = SP1-Rust; (2) bash empty-allowed=all + the `Only(∅)` trap = Tasks 5–6; (3) MCP health strictness = SP1-Rust (runtime availability is a pure *input* here, model is general — Task 19 note, no Lean boundary needed); (4) `load_skill`/generated tools = the `skills` Surface field (Task 2) + its subset lemma (Task 8).
- **This plan is Lean + conformance ONLY.** No `src/` runtime/resolver changes beyond the test-only mirror; no schema/protocol/desktop edits — those are SP1-Rust.

---

## File Structure

**New Lean files (under `crates/defra-agent/proofs/Proofs/`):**
- `ToolPolicy.lean` — barrel: `import Proofs.ToolPolicy.Types` … `.Cases`.
- `ToolPolicy/Types.lean` — `FileCap`, `EndpointScope K V`, `ValueMeet V`, `BashPolicy`, `Surface`, `Avail`.
- `ToolPolicy/Meet.lean` — every `meet` + `permits` denotation.
- `ToolPolicy/Theorems.lean` — subset/lower-bound + meet algebra.
- `ToolPolicy/Instances.lean` — per-category `ValueMeet` instances + narrowing corollaries.
- `ToolPolicy/Cases.lean` — witness inputs + expected outputs + JSON serializer.
- `Conformance/Contracts/Json/ToolPolicy.lean` — `toolPolicyCasesJson` (per-domain emitter, mirrors `Json/CommandPolicy.lean`).

**Modified Lean files:**
- `Proofs.lean` — add `import Proofs.ToolPolicy`.
- `Conformance/Contracts/Json/Snapshot.lean` — import + `"tool_policy_cases"` line.
- `Conformance/CoverageLedger.lean` — `caseCoverage` entry.

**New Rust files:**
- `crates/defra-agent/src/lean_vocab_test/tool_policy.rs` — `LeanToolPolicy*` case + input structs.
- `crates/defra-agent/tests/conformance/tool_policy_mirror.rs` — pure Rust meet re-derivation.
- `crates/defra-agent/tests/conformance/tool_policy.rs` — the fenced test.

**Modified Rust files:**
- `crates/defra-agent/src/lean_vocab_test.rs` — snapshot field + `mod tool_policy;` + accessors.
- `crates/defra-agent/tests/conformance.rs` — `mod tool_policy;` + `#[test]` wrapper.
- `crates/defra-agent/tests/conformance/structure.rs` — `model_homes()` entry.
- `crates/defra-agent/tests/conformance/coverage.rs` — emission check + `valid_categories`.
- `crates/defra-agent/tests/support/conformance_consumers.rs` — consumer id.

---

## Task 1: Barrel + atom types (`FileCap`, `EndpointScope`, `ValueMeet`)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy.lean`
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`

**Interfaces:**
- Produces: `ToolPolicy.ToolId := String`; `FileCap` (`.off|.readOnly|.readWrite`); `EndpointScope (K V : Type)` (`.none|.only (keys : Finset K) (val : K → V)|.all` — a finite map, single-valued by construction); `ValueMeet V` record (`vmeet`, `vle`, `vmeet_le_left`, `vmeet_le_right`). Consumed by all later tasks.

- [ ] **Step 1: Create `Types.lean`**

```lean
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Lattice.Basic

/-! # Tool Policy — Types (atoms + aggregate). -/

namespace ToolPolicy

abbrev ToolId := String

inductive FileCap where
  | off | readOnly | readWrite
  deriving DecidableEq, Repr

/-- A value-narrowing meet: `vmeet` is a lower bound under order `vle`.
    Endpoint categories supply one of these; `Unit`-valued categories use
    the trivial instance. -/
structure ValueMeet (V : Type) where
  vmeet : V → V → V
  vle : V → V → Prop
  vmeet_le_left : ∀ a b, vle (vmeet a b) a
  vmeet_le_right : ∀ a b, vle (vmeet a b) b

/-- Keyed endpoint scope. `only` carries a finite key SET plus a TOTAL value
    function — a finite map with structural single-value-per-key (no uniqueness
    convention to maintain). `none` ≤ `only keys val` ≤ `all`. The value
    function is only meaningful on `keys`; off-key values are irrelevant and
    never observed (`permits`/`lookup` gate on `keys`). -/
inductive EndpointScope (K V : Type) where
  | none
  | only (keys : Finset K) (val : K → V)
  | all

end ToolPolicy
```

- [ ] **Step 2: Create the barrel `ToolPolicy.lean`**

```lean
import Proofs.ToolPolicy.Types
import Proofs.ToolPolicy.Meet
import Proofs.ToolPolicy.Theorems
import Proofs.ToolPolicy.Instances
import Proofs.ToolPolicy.Cases
```

> The barrel will not build until later files exist. Add the imports incrementally: for THIS task include only `import Proofs.ToolPolicy.Types`, and append the others in their tasks.

- [ ] **Step 3: Import the barrel from the root**

Edit `Proofs.lean` — add after `import Proofs.CommandPolicy`:

```lean
import Proofs.ToolPolicy
```

- [ ] **Step 4: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy.lean crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean crates/defra-agent/proofs/Proofs.lean
git commit -m "feat(proofs): ToolPolicy barrel + atom types (FileCap, EndpointScope K V, ValueMeet)"
```

---

## Task 2: `BashPolicy`, `Surface`, `Avail`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean`

**Interfaces:**
- Produces: `BashPolicy` (mode/network/forbidden/allowed-scope/readonly-scope/sandbox), `Surface` (full per-category aggregate), `Avail := Surface`. **`Surface` field names are the contract for all later tasks.**

- [ ] **Step 1: Add the modeled bash policy + aggregate**

Append to `Types.lean` before `end ToolPolicy`:

```lean
/-- Execution + network permissiveness ranks (lower = stricter). -/
inductive ExecMode where | readOnly | workspaceWrite | unrestricted
  deriving DecidableEq, Repr
inductive NetMode where | disabled | inherit | enabled
  deriving DecidableEq, Repr

/-- Bash as a product. Allowed/read-only gates are `EndpointScope`s so the
    empty-list=allow-all wire convention is `all`, and `Only(∅)` (deny-all
    after a meet) stays distinct. `sandbox` is availability (fail-closed). -/
structure BashPolicy where
  mode      : ExecMode
  network   : NetMode
  forbidden : Finset (List String)
  allowed   : EndpointScope (List String) Unit
  readOnly  : EndpointScope String Unit
  sandbox   : Bool

/-- Full per-category tool policy. Used at behavior, ceiling, and (the
    availability-shaped) runtime levels. -/
structure Surface where
  file             : FileCap
  bash             : BashPolicy
  meta             : Bool
  defraQuery       : Bool
  memory           : Bool
  sessionHistory   : Bool
  contextBudget    : Bool
  spawn            : Bool
  steering         : Bool
  background       : Bool
  orchestration    : Bool
  crossDeployment  : Bool
  skills           : Bool
  cliTools         : EndpointScope ToolId (Finset String)         -- value: allowed roots
  mcpServices      : EndpointScope ToolId Unit
  defraCollections : EndpointScope ToolId Unit
  subagentTargets  : EndpointScope (String × String) Unit          -- key: (did, behavior)
  backgroundTools  : EndpointScope ToolId Unit
  writeTools       : EndpointScope (String × String) (Finset String) -- key:(tool,collection) val:fields

abbrev Avail := Surface
```

- [ ] **Step 2: Build + commit**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean
git commit -m "feat(proofs): BashPolicy product + Surface aggregate + Avail"
```

---

## Task 3: `FileCap` rank + meet + order lemmas

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy.lean` (append `import Proofs.ToolPolicy.Meet`)

**Interfaces:**
- Produces: `FileCap.rank`, `FileCap.meet`, `FileCap.meet_rank_le_left/right`, `FileCap.rank_inj`.

- [ ] **Step 1: Write `Meet.lean` head**

```lean
import Proofs.ToolPolicy.Types

namespace ToolPolicy

def FileCap.rank : FileCap → Nat
  | .off => 0 | .readOnly => 1 | .readWrite => 2

def FileCap.meet (a b : FileCap) : FileCap :=
  if a.rank ≤ b.rank then a else b

theorem FileCap.rank_inj {a b : FileCap} (h : a.rank = b.rank) : a = b := by
  cases a <;> cases b <;> simp_all [FileCap.rank]

theorem FileCap.meet_rank_le_left (a b : FileCap) : (a.meet b).rank ≤ a.rank := by
  unfold FileCap.meet; by_cases h : a.rank ≤ b.rank <;> simp [h]

theorem FileCap.meet_rank_le_right (a b : FileCap) : (a.meet b).rank ≤ b.rank := by
  unfold FileCap.meet; by_cases h : a.rank ≤ b.rank <;> simp [h]
  · exact h
  · exact Nat.le_of_lt (Nat.lt_of_not_le h)

end ToolPolicy
```

- [ ] **Step 2: Append barrel import, build, commit**

Append `import Proofs.ToolPolicy.Meet` to `ToolPolicy.lean`.
Run: `cd crates/defra-agent/proofs && lake build` → clean (if a `Nat` branch stalls, use `omega`).

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean crates/defra-agent/proofs/Proofs/ToolPolicy.lean
git commit -m "feat(proofs): FileCap rank + meet + order lemmas"
```

---

## Task 4: Generic `EndpointScope` meet + `permits` + keyed subset & value-narrowing lemmas

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`

**Interfaces:**
- Produces: `EndpointScope.permits : EndpointScope K V → K → Prop` (key-level); `EndpointScope.lookup : EndpointScope K V → K → Option V`; `EndpointScope.meet (vm : ValueMeet V) : EndpointScope K V → EndpointScope K V → EndpointScope K V`; lemmas `meet_permits_left/right` (key subset) and `meet_lookup_vle_left/right` (value narrowing under `vm.vle`).

- [ ] **Step 1: Add the generic meet + denotations**

Insert into `Meet.lean` before `end ToolPolicy`:

```lean
variable {K V : Type} [DecidableEq K] [DecidableEq V]

/-- The keys carried by a scope. -/
/-- Key-level permission. `all` permits every key; `none` permits none;
    `only keys _` permits exactly `keys`. Decidable (Finset membership). -/
def EndpointScope.permits : EndpointScope K V → K → Prop
  | .none, _ => False
  | .all, _ => True
  | .only keys _, k => k ∈ keys

instance (sc : EndpointScope K V) (k : K) [DecidableEq K] :
    Decidable (sc.permits k) := by
  cases sc <;> simp [EndpointScope.permits] <;> infer_instance

/-- Lookup the value at a key: total, single-valued by construction. -/
def EndpointScope.lookup : EndpointScope K V → K → Option V
  | .none, _ => Option.none
  | .all, _ => Option.none
  | .only keys val, k => if k ∈ keys then some (val k) else Option.none

/-- Keyed meet under a value meet: keys intersect, values meet pointwise. -/
def EndpointScope.meet (vm : ValueMeet V) :
    EndpointScope K V → EndpointScope K V → EndpointScope K V
  | .none, _ => .none
  | _, .none => .none
  | .all, b => b
  | a, .all => a
  | .only ka va, .only kb vb =>
      .only (ka ∩ kb) (fun k => vm.vmeet (va k) (vb k))
```

- [ ] **Step 2: Add the key-subset + value-narrowing lemmas (the failing target)**

```lean
variable [DecidableEq K]

theorem EndpointScope.meet_permits_left (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) :
    (a.meet vm b).permits k → a.permits k := by
  cases a <;> cases b <;>
    simp [EndpointScope.meet, EndpointScope.permits, Finset.mem_inter] <;>
    intro h <;> tauto

theorem EndpointScope.meet_permits_right (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) :
    (a.meet vm b).permits k → b.permits k := by
  cases a <;> cases b <;>
    simp [EndpointScope.meet, EndpointScope.permits, Finset.mem_inter] <;>
    intro h <;> tauto

/-- Value narrowing: where a key survives the meet, the meet's value is `vle`
    the right operand's value. Now PROVABLE (single-valued by construction). -/
theorem EndpointScope.meet_lookup_vle_right (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) (w w' : V)
    (hm : (a.meet vm b).lookup k = some w) (hb : b.lookup k = some w') :
    vm.vle w w' := by
  cases a <;> cases b <;>
    simp_all [EndpointScope.meet, EndpointScope.lookup, Finset.mem_inter]
  -- only/only: hm gives w = vm.vmeet (va k) (vb k), hb gives w' = vb k.
  -- subst both; close by vm.vmeet_le_right.
  rename_i ka va kb vb
  split at hm <;> split at hb <;> simp_all
  subst hm; subst hb; exact vm.vmeet_le_right _ _

/-- Symmetric left version. -/
theorem EndpointScope.meet_lookup_vle_left (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) (w w' : V)
    (hm : (a.meet vm b).lookup k = some w) (ha : a.lookup k = some w') :
    vm.vle w w' := by
  cases a <;> cases b <;>
    simp_all [EndpointScope.meet, EndpointScope.lookup, Finset.mem_inter]
  rename_i ka va kb vb
  split at hm <;> split at ha <;> simp_all
  subst hm; subst ha; exact vm.vmeet_le_left _ _
```

- [ ] **Step 3: Build (iterate)**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean. With the finite-map representation the `only/only` case of `meet_permits_*` reduces to `k ∈ ka ∩ kb → k ∈ ka` (`Finset.mem_inter.mp …`); if `tauto` stalls use `exact fun h => (Finset.mem_inter.mp h).1` / `.2`. For the `vle` lemmas the `split`/`subst` may need the `if`-condition hypotheses named — adjust with `by_cases hk : k ∈ ka ∩ kb` if `split at` is awkward.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): generic EndpointScope keyed meet + permits + subset lemmas"
```

---

## Task 5: `BashPolicy.permits` + `BashPolicy.meet` (all fields, gates as scopes)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`

**Interfaces:**
- Produces: `unitVM : ValueMeet Unit`; `ExecMode.rank`, `NetMode.rank`; `CmdReq` = `{ argv : List String, cmdHead : String, wantsNetwork : Bool, wantsWrite : Bool }`; `BashPolicy.allowedPrefixMatched`; `BashPolicy.permits : BashPolicy → CmdReq → Prop` (covers **all six fields**); `BashPolicy.meet`.

- [ ] **Step 1: Add the trivial value meet, ranks, request type, permits (full denotation), and meet**

```lean
def unitVM : ValueMeet Unit :=
  { vmeet := fun _ _ => (), vle := fun _ _ => True,
    vmeet_le_left := by intro _ _; trivial, vmeet_le_right := by intro _ _; trivial }

def ExecMode.rank : ExecMode → Nat
  | .readOnly => 0 | .workspaceWrite => 1 | .unrestricted => 2
def NetMode.rank : NetMode → Nat
  | .disabled => 0 | .inherit => 1 | .enabled => 2

/-- A command request: its argv, the command head (for the read-only
    allowlist), whether it needs network, and whether it writes. -/
structure CmdReq where
  argv : List String
  cmdHead : String
  wantsNetwork : Bool
  wantsWrite : Bool

/-- Did an operator-configured allowed prefix match (authorizes a read-only
    diagnostic command alongside the allowlist)? Mirrors
    `CommandPolicy/Validation.lean`'s `allowedPrefixMatched`. -/
def BashPolicy.allowedPrefixMatched (p : BashPolicy) (req : CmdReq) : Prop :=
  match p.allowed with
  | .only keys _ => ∃ pre ∈ keys, pre.isPrefixOf req.argv
  | _ => False

/-- A bash policy permits a request iff ALL SIX gates pass, in
    `CommandPolicy/Validation.lean` order:
    1. sandbox available (fail-closed);
    2. no forbidden prefix matches;
    3. the allowed gate admits argv (empty gate = `all`, `none` = deny);
    4. network demand within `network`;
    5. mode gate: a write request needs a write-capable mode; a read-only mode
       additionally requires the command head to be in `readOnly` OR an allowed
       prefix to have matched. -/
def BashPolicy.permits (p : BashPolicy) (req : CmdReq) : Prop :=
  p.sandbox = true
  ∧ (∀ f ∈ p.forbidden, ¬ f.isPrefixOf req.argv)
  ∧ (match p.allowed with
      | .all => True
      | .none => False
      | .only keys _ => ∃ pre ∈ keys, pre.isPrefixOf req.argv)
  ∧ (req.wantsNetwork → p.network.rank ≥ NetMode.rank .inherit)
  ∧ (match p.mode with
      | .readOnly =>
          ¬ req.wantsWrite ∧ (p.readOnly.permits req.cmdHead ∨ p.allowedPrefixMatched req)
      | .workspaceWrite => True
      | .unrestricted => True)

def BashPolicy.meet (a b : BashPolicy) : BashPolicy :=
  { mode := if a.mode.rank ≤ b.mode.rank then a.mode else b.mode
  , network := if a.network.rank ≤ b.network.rank then a.network else b.network
  , forbidden := a.forbidden ∪ b.forbidden
  , allowed := a.allowed.meet unitVM b.allowed
  , readOnly := a.readOnly.meet unitVM b.readOnly
  , sandbox := a.sandbox && b.sandbox }
```

> `List.isPrefixOf` exists in Lean core for `[BEq α]`; `String` lists qualify. `readOnly`/`allowed` are `EndpointScope … Unit`; `permits`/`allowedPrefixMatched` read their `keys` only (`.only keys _`). Confirm `Finset.union` is `∪`. **Mode-gate soundness intuition** (proved in Task 6): `mode` meet = `min` rank, so a request admitted by the *stricter* meet mode is admitted by each looser operand mode; the read-only allowlist meets by key-intersection, so a head admitted by the meet is admitted by both.

- [ ] **Step 2: Build + commit**

Run: `cd crates/defra-agent/proofs && lake build` → clean.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): BashPolicy permits + full-field meet (gates as EndpointScope)"
```

---

## Task 6: Bash safety — `meet.permits → both permit`, per field

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy.lean` (append `import Proofs.ToolPolicy.Theorems`)

**Interfaces:**
- Produces: `BashPolicy.meet_permits_left/right`, and field lemmas `bash_meet_mode_le`, `bash_meet_network_le`, `bash_meet_forbidden_superset`, `bash_meet_sandbox`, `bash_meet_allowed_left/right`, `bash_meet_readonly_left/right`, `bash_meet_mode_gate_left/right`.

- [ ] **Step 1: Write the per-field lemmas + the headline bash safety theorem**

Create `Theorems.lean`:

```lean
import Proofs.ToolPolicy.Meet

namespace ToolPolicy

theorem bash_meet_mode_le (a b : BashPolicy) :
    (a.meet b).mode.rank ≤ a.mode.rank ∧ (a.meet b).mode.rank ≤ b.mode.rank := by
  unfold BashPolicy.meet; constructor <;> by_cases h : a.mode.rank ≤ b.mode.rank <;>
    simp [h] <;> omega

theorem bash_meet_network_le (a b : BashPolicy) :
    (a.meet b).network.rank ≤ a.network.rank ∧ (a.meet b).network.rank ≤ b.network.rank := by
  unfold BashPolicy.meet; constructor <;> by_cases h : a.network.rank ≤ b.network.rank <;>
    simp [h] <;> omega

theorem bash_meet_forbidden_superset (a b : BashPolicy) :
    a.forbidden ⊆ (a.meet b).forbidden ∧ b.forbidden ⊆ (a.meet b).forbidden := by
  unfold BashPolicy.meet
  exact ⟨Finset.subset_union_left, Finset.subset_union_right⟩

theorem bash_meet_sandbox (a b : BashPolicy) :
    (a.meet b).sandbox = true → a.sandbox = true ∧ b.sandbox = true := by
  unfold BashPolicy.meet; intro h; simpa using Bool.and_eq_true.mp h

/-- Allowed-gate lower bound: an argv admitted by the meet's allowed gate is
    admitted by each operand's gate. Handles the `Only(∅)` trap: the meet of
    two `only`s is `only (ka ∩ kb)` (still a gate, never `all`). -/
theorem bash_meet_allowed_left (a b : BashPolicy) (req : CmdReq)
    (h : match (a.meet b).allowed with
         | .all => True | .none => False
         | .only keys _ => ∃ pre ∈ keys, pre.isPrefixOf req.argv) :
    (match a.allowed with
     | .all => True | .none => False
     | .only keys _ => ∃ pre ∈ keys, pre.isPrefixOf req.argv) := by
  unfold BashPolicy.meet at h
  cases ha : a.allowed <;> cases hb : b.allowed <;>
    simp_all [EndpointScope.meet, Finset.mem_inter]
  -- only/only: the witness prefix is in ka ∩ kb, hence in ka.
  obtain ⟨pre, hpre, hp⟩ := h; exact ⟨pre, (Finset.mem_inter.mp hpre).1, hp⟩

/-- Read-only allowlist lower bound: a head admitted by the meet's readOnly is
    admitted by each operand's readOnly (key intersection). -/
theorem bash_meet_readonly_left (a b : BashPolicy) (k : String) :
    (a.meet b).readOnly.permits k → a.readOnly.permits k := by
  unfold BashPolicy.meet
  exact EndpointScope.meet_permits_left unitVM a.readOnly b.readOnly k

/-- Allowed-prefix-match lower bound (same key-intersection argument as the
    allowed gate, but the predicate is `False` for `all`/`none`). -/
theorem bash_meet_allowedPrefixMatched_left (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).allowedPrefixMatched req → a.allowedPrefixMatched req := by
  unfold BashPolicy.meet BashPolicy.allowedPrefixMatched
  cases ha : a.allowed <;> cases hb : b.allowed <;>
    simp_all [EndpointScope.meet, Finset.mem_inter]
  obtain ⟨pre, hpre, hp⟩ := ‹_›; exact ⟨pre, (Finset.mem_inter.mp hpre).1, hp⟩

/-- Mode-gate lower bound: if the (stricter) meet mode admits the request, so
    does each looser operand mode. Read-only is rank 0 (strictest), so when the
    meet mode is read-only both the allowlist and write checks transfer; when
    the meet mode is looser, both operands are at least as loose (their gate is
    `True`). -/
theorem bash_meet_mode_gate_left (a b : BashPolicy) (req : CmdReq)
    (hmode : match (a.meet b).mode with
             | .readOnly => ¬ req.wantsWrite ∧
                 ((a.meet b).readOnly.permits req.cmdHead ∨ (a.meet b).allowedPrefixMatched req)
             | _ => True) :
    (match a.mode with
     | .readOnly => ¬ req.wantsWrite ∧ (a.readOnly.permits req.cmdHead ∨ a.allowedPrefixMatched req)
     | _ => True) := by
  -- If a.mode ≠ readOnly the goal is `True`. If a.mode = readOnly then since
  -- (a.meet b).mode = min-rank and readOnly is rank 0, the meet mode is also
  -- readOnly, so hmode gives the conjuncts; transfer via bash_meet_readonly_left
  -- and the allowed-prefix key-subset (allowedPrefixMatched uses a.allowed's
  -- keys ⊇ meet keys).
  cases hA : a.mode <;> simp_all
  · -- a.mode = readOnly ⇒ meet mode = readOnly
    have : (a.meet b).mode = ExecMode.readOnly := by
      unfold BashPolicy.meet; simp [hA, ExecMode.rank]
      by_cases hb : b.mode.rank ≤ (ExecMode.readOnly).rank <;> simp_all [ExecMode.rank]
      · interval_cases b.mode <;> simp_all [ExecMode.rank]
    rw [this] at hmode
    obtain ⟨hw, hro⟩ := hmode
    refine ⟨hw, ?_⟩
    rcases hro with hk | hpm
    · exact Or.inl (bash_meet_readonly_left a b req.cmdHead hk)
    · exact Or.inr (bash_meet_allowedPrefixMatched_left a b req hpm)
```

> The `min`-mode step (`(a.meet b).mode = readOnly` when `a.mode = readOnly`) follows because `readOnly` has rank 0, the minimum; if `interval_cases b.mode`/`simp` is awkward, prove it via `omega` on the ranks plus `ExecMode` injectivity (`cases (a.meet b).mode` and discharge the non-`readOnly` cases by rank contradiction). No `sorry` survives.

```lean
/-- The meet never permits a request beyond either operand: forbidden grows
    (union), allowed/readOnly narrow (key intersection), network/mode drop,
    sandbox fails-closed. All six gates. -/
theorem BashPolicy.meet_permits_left (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).permits req → a.permits req := by
  intro ⟨hs, hf, hal, hn, hmode⟩
  refine ⟨(bash_meet_sandbox a b hs).1, ?_, bash_meet_allowed_left a b req hal, ?_,
          bash_meet_mode_gate_left a b req hmode⟩
  · intro f hf'; exact hf f ((bash_meet_forbidden_superset a b).1 hf')
  · intro hw; have := hn hw; have hle := (bash_meet_network_le a b).1; omega
```

> Write `bash_meet_allowed_right` / `bash_meet_readonly_right` / `bash_meet_mode_gate_right` / `BashPolicy.meet_permits_right` symmetrically (use `_right` field lemmas and `(Finset.mem_inter.mp _).2`). The mode-gate proof is the hardest in the plan; its fallback is the inline `Finset.mem_inter` step noted above.

- [ ] **Step 2: Append barrel import, build (this is a hard proof — iterate)**

Append `import Proofs.ToolPolicy.Theorems` to `ToolPolicy.lean`.
Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero `sorry`. If the allowed-gate conjunct resists, factor a standalone lemma `bash_allowed_meet_permits_left : (a.allowed.meet unitVM b.allowed) admits argv → a.allowed admits argv` proved directly from `EndpointScope.meet_permits_left`, and apply it.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean crates/defra-agent/proofs/Proofs/ToolPolicy.lean
git commit -m "feat(proofs): bash meet safety (meet.permits => both permit) across all fields"
```

---

## Task 7: `Surface.meet` + `effective`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`

**Interfaces:**
- Produces: `bool_and_left` / `bool_and_right` helper lemmas only. `Surface.meet` and `effective` are intentionally deferred to **Task 8** (they need the per-category `ValueMeet` instances `fieldsVM`/`rootVM` from `Instances.lean`); defining them here would force a forward dependency on a not-yet-existing file. This task just lands the boolean meet helpers the aggregate theorems reuse.

- [ ] **Step 1: Define `Surface.meet` referencing instances that will exist by build time**

Because `Surface.meet` needs `fieldsVM`/`rootVM` (defined in `Instances.lean`, Task 8), `Surface.meet` + `effective` are defined in **Task 8**, not here. This task instead adds the **boolean** helper lemmas used by the aggregate theorems:

```lean
/-- Boolean meet helpers (so the aggregate reads uniformly). -/
@[simp] theorem bool_and_left {a b : Bool} (h : (a && b) = true) : a = true :=
  (Bool.and_eq_true.mp h).1
@[simp] theorem bool_and_right {a b : Bool} (h : (a && b) = true) : b = true :=
  (Bool.and_eq_true.mp h).2
```

- [ ] **Step 2: Build + commit**

Run: `cd crates/defra-agent/proofs && lake build` → clean.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): boolean meet helper lemmas for the aggregate"
```

---

## Task 8: Per-category `ValueMeet` instances + `Surface.meet` + `effective`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Instances.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy.lean` (append `import Proofs.ToolPolicy.Instances`)

**Interfaces:**
- Produces: `fieldsVM : ValueMeet (Finset String)` (write-tool fields; `vmeet = ∩`, `vle = ⊆`), `rootVM : ValueMeet (Finset String)` (cli roots; same shape), `Surface.meet`, `effective`.

- [ ] **Step 1: Define the structured value meets**

```lean
import Proofs.ToolPolicy.Theorems

namespace ToolPolicy

/-- Write-tool field narrowing: effective fields = intersection. -/
def fieldsVM : ValueMeet (Finset String) :=
  { vmeet := fun a b => a ∩ b
  , vle := fun a b => a ⊆ b
  , vmeet_le_left := by intro a b; exact Finset.inter_subset_left
  , vmeet_le_right := by intro a b; exact Finset.inter_subset_right }

/-- CLI root narrowing: effective allowed-roots = intersection (a stand-in
    for path-prefix containment; the precise root semantics is refined in
    SP1-Rust). -/
def rootVM : ValueMeet (Finset String) := fieldsVM
```

- [ ] **Step 2: Define `Surface.meet` + `effective`**

```lean
def Surface.meet (a b : Surface) : Surface :=
  { file             := a.file.meet b.file
  , bash             := a.bash.meet b.bash
  , meta             := a.meta && b.meta
  , defraQuery       := a.defraQuery && b.defraQuery
  , memory           := a.memory && b.memory
  , sessionHistory   := a.sessionHistory && b.sessionHistory
  , contextBudget    := a.contextBudget && b.contextBudget
  , spawn            := a.spawn && b.spawn
  , steering         := a.steering && b.steering
  , background       := a.background && b.background
  , orchestration    := a.orchestration && b.orchestration
  , crossDeployment  := a.crossDeployment && b.crossDeployment
  , skills           := a.skills && b.skills
  , cliTools         := a.cliTools.meet rootVM b.cliTools
  , mcpServices      := a.mcpServices.meet unitVM b.mcpServices
  , defraCollections := a.defraCollections.meet unitVM b.defraCollections
  , subagentTargets  := a.subagentTargets.meet unitVM b.subagentTargets
  , backgroundTools  := a.backgroundTools.meet unitVM b.backgroundTools
  , writeTools       := a.writeTools.meet fieldsVM b.writeTools }

def effective (behavior ceiling : Surface) (runtime : Avail) : Surface :=
  (behavior.meet ceiling).meet runtime
```

- [ ] **Step 3: Append barrel import, build, commit**

Append `import Proofs.ToolPolicy.Instances` to `ToolPolicy.lean`.
Run: `cd crates/defra-agent/proofs && lake build` → clean.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Instances.lean crates/defra-agent/proofs/Proofs/ToolPolicy.lean
git commit -m "feat(proofs): per-category ValueMeet instances + Surface.meet + effective"
```

---

## Task 9: Headline subset theorems — `effective ⊆ ceiling` & `⊆ behavior`, all categories

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean` (add an `Instances`-dependent section — import `Instances` at top, or place these in `Instances.lean`)

> Place this section in `Instances.lean` (it depends on `Surface.meet`). Keep the file order: `Meet → Theorems → Instances`, and put aggregate theorems in `Instances.lean`.

**Interfaces:**
- Produces: per-category `effective_<cat>_le_ceiling` / `_le_behavior`, plus a bundled doc theorem `effective_within_ceiling`.

- [ ] **Step 1: Write the representative theorem per KIND (then enumerate the mechanical repeats)**

Append to `Instances.lean`:

```lean
variable (behavior ceiling : Surface) (runtime : Avail)

-- KIND A: file (rank). ceiling is the inner-meet's right operand; runtime outer.
theorem effective_file_le_ceiling :
    (effective behavior ceiling runtime).file.rank ≤ ceiling.file.rank := by
  unfold effective Surface.meet
  exact le_trans (FileCap.meet_rank_le_left _ _) (FileCap.meet_rank_le_right _ _)

-- KIND B: boolean (meta shown; repeat verbatim for defraQuery, memory,
-- sessionHistory, contextBudget, spawn, steering, background, orchestration,
-- crossDeployment, skills).
theorem effective_meta_le_ceiling :
    (effective behavior ceiling runtime).meta = true → ceiling.meta = true := by
  unfold effective Surface.meet; intro h; exact bool_and_right (bool_and_left h)

-- KIND C: Unit-endpoint (mcpServices shown; repeat for defraCollections,
-- subagentTargets, backgroundTools).
theorem effective_mcp_subset_ceiling (k : ToolId) :
    (effective behavior ceiling runtime).mcpServices.permits k →
      ceiling.mcpServices.permits k := by
  unfold effective Surface.meet; intro h
  exact EndpointScope.meet_permits_left unitVM _ _ _
    (EndpointScope.meet_permits_right unitVM _ _ _ h)

-- KIND D: structured-endpoint key subset (writeTools shown; cliTools same with
-- rootVM). The VALUE-narrowing corollary follows from vm.vmeet_le_* (Task 10).
theorem effective_write_keys_subset_ceiling (k : String × String) :
    (effective behavior ceiling runtime).writeTools.permits k →
      ceiling.writeTools.permits k := by
  unfold effective Surface.meet; intro h
  exact EndpointScope.meet_permits_left fieldsVM _ _ _
    (EndpointScope.meet_permits_right fieldsVM _ _ _ h)

-- KIND E: bash (request-level). 
theorem effective_bash_permits_subset_ceiling (req : CmdReq) :
    (effective behavior ceiling runtime).bash.permits req →
      ceiling.bash.permits req := by
  unfold effective Surface.meet; intro h
  exact BashPolicy.meet_permits_left _ _ req
    (BashPolicy.meet_permits_right _ _ req h)
```

Write the full enumerated set: 1×KIND A, 11×KIND B (one per boolean field), 4×KIND C, 2×KIND D, 1×KIND E. Then the `_le_behavior` mirror for each (swap inner `_right`→`_left`; for the outer runtime meet drop to the `behavior.meet ceiling` operand via `_left`).

- [ ] **Step 2: Bundle**

Add a doc-comment conjunction theorem `effective_within_ceiling` whose statement `&&`s/`∧`s the representative per-category lemmas (so reviewers have one referenceable name).

- [ ] **Step 3: Build (proof gate) + commit**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero `sorry`. Verify the inner/outer meet nesting in `effective` matches the `_left`/`_right` choices (ceiling is the *second* arg of `behavior.meet ceiling`; runtime is the *second* arg of the outer meet).

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Instances.lean
git commit -m "feat(proofs): effective ⊆ ceiling & ⊆ behavior for every tool category"
```

---

## Task 10: Value-narrowing corollary + meet algebra (idempotent/commutative)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Instances.lean`

**Interfaces:**
- Produces: `effective_write_fields_narrow` (the K/V narrowing the design promised, built on `EndpointScope.meet_lookup_vle_right` from **Task 4**), `FileCap.meet_idem/comm`, `EndpointScope.meet_idem`, `BashPolicy.meet_idem`, `Surface.meet_idem`.

**Scope note (review #3):** the load-bearing property is **lower-bound safety** (Task 9). We additionally prove **idempotence** (atom + aggregate) and **commutativity at the atom level**, which give the practical guarantees ("secure ⊓ wide-open = secure"; order-independence of behavior/ceiling). Full **associativity is intentionally omitted** — `effective` fixes the composition order, so it is not load-bearing; the Goal/Architecture state this explicitly so the claim matches the proofs.

- [ ] **Step 1: Write the write-tool value-narrowing corollary**

Uses `EndpointScope.meet_lookup_vle_right` (proved generically in Task 4). The outer meet against `runtime` is the identity on `writeTools` (runtime is `.all` there), so a single application after reducing through `effective`:

```lean
theorem effective_write_fields_narrow
    (behavior ceiling : Surface) (runtime : Avail)
    (hrt : runtime.writeTools = .all)
    (k : String × String) (vc ve : Finset String)
    (hck : ceiling.writeTools.lookup k = some vc)
    (hek : (effective behavior ceiling runtime).writeTools.lookup k = some ve) :
    ve ⊆ vc := by
  -- effective.writeTools = (behavior.meet ceiling).meet fieldsVM runtime.writeTools
  --                      = (behavior.meet ceiling).meet fieldsVM .all   [hrt]
  --                      = (behavior ⊓ ceiling).writeTools              [meet right-id on .all]
  -- so the meet against ceiling gives the narrowing via vmeet_le_right.
  unfold effective Surface.meet at hek
  rw [hrt] at hek
  simp [EndpointScope.meet] at hek          -- collapses `_.meet fieldsVM .all`
  exact EndpointScope.meet_lookup_vle_right fieldsVM
    behavior.writeTools ceiling.writeTools k ve vc hek hck
```

> `fieldsVM.vle = (· ⊆ ·)`, so the conclusion `vle ve vc` IS `ve ⊆ vc`. If `simp [EndpointScope.meet]` does not fully collapse the `.all` right-identity, add the rewrite lemma `EndpointScope.meet_all_right : x.meet vm .all = x` (one-line `cases x <;> rfl`) in Meet.lean and use it. No `sorry`.

- [ ] **Step 2: Meet algebra (idempotence + atom commutativity)**

```lean
@[simp] theorem FileCap.meet_idem (a : FileCap) : a.meet a = a := by simp [FileCap.meet]
theorem FileCap.meet_comm (a b : FileCap) : a.meet b = b.meet a := by
  unfold FileCap.meet
  by_cases h : a.rank ≤ b.rank <;> by_cases h2 : b.rank ≤ a.rank
  · exact FileCap.rank_inj (Nat.le_antisymm h h2)
  all_goals simp_all <;> omega

/-- Endpoint idempotence (needs value idempotence + funext). -/
theorem EndpointScope.meet_idem (vm : ValueMeet V)
    (hidem : ∀ v, vm.vmeet v v = v) (a : EndpointScope K V) :
    a.meet vm a = a := by
  cases a with
  | none => rfl
  | all => rfl
  | only keys val =>
      simp [EndpointScope.meet, Finset.inter_self]
      funext k; exact hidem (val k)

/-- Bash idempotence (pointwise; uses unit-value idempotence for the gates). -/
theorem BashPolicy.meet_idem (p : BashPolicy) : p.meet p = p := by
  unfold BashPolicy.meet
  have hu : ∀ v : Unit, unitVM.vmeet v v = v := by intro v; rfl
  simp [le_refl, Finset.union_self,
        EndpointScope.meet_idem unitVM hu, Bool.and_self]

/-- Aggregate idempotence: `secure ⊓ secure = secure`, etc. -/
theorem Surface.meet_idem (s : Surface) : s.meet s = s := by
  unfold Surface.meet
  have hf : ∀ v : Finset String, fieldsVM.vmeet v v = v := by
    intro v; simp [fieldsVM, Finset.inter_self]
  have hr : ∀ v : Finset String, rootVM.vmeet v v = v := hf
  have hu : ∀ v : Unit, unitVM.vmeet v v = v := by intro v; rfl
  simp [FileCap.meet_idem, BashPolicy.meet_idem, Bool.and_self,
        EndpointScope.meet_idem unitVM hu, EndpointScope.meet_idem fieldsVM hf,
        EndpointScope.meet_idem rootVM hr]
```

> `Surface.meet_idem` reduces each of the 19 fields by the relevant idempotence simp lemma; if `simp` leaves a residual record-eta goal, finish with `rfl` or `cases s`.

- [ ] **Step 3: Build + commit**

Run: `cd crates/defra-agent/proofs && lake build` → clean, no `sorry`.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Instances.lean crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): write-tool value narrowing + meet idempotence (atom + aggregate)"
```

---

## Task 11: `Cases.lean` — emit INPUTS + expected OUTPUT (serializable)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Cases.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy.lean` (append import)

**Interfaces:**
- Produces: `ToolPolicy.ContractCases` namespace with `Case` (carrying serialized `behavior`/`ceiling`/`runtime` inputs AND the expected `effective` projection probes), `cases : List Case`. **Field names = the JSON/Rust contract (Tasks 14, 16).**

- [ ] **Step 1: Define a Case that carries inputs + expected output**

The Case serializes a compact, fully-probeable view: the three input Surfaces reduced to the fields the Rust mirror needs, plus the expected effective probes. Use the same reduced shape for input and output so the Rust mirror is a pure function `reduce(behavior,ceiling,runtime) -> output`.

```lean
namespace ToolPolicy.ContractCases
open ToolPolicy

/-- A reduced, JSON-friendly projection of a Surface: the scalar ranks/bools
    plus single-probe endpoint answers. Inputs are three of these; the
    expected effective output is a fourth, computed by the Lean `effective`. -/
structure SurfaceView where
  fileRank      : Nat
  meta          : Bool
  defraQuery    : Bool
  spawn         : Bool
  bashMode      : Nat
  bashNet       : Nat
  bashSandbox   : Bool
  bashAllowedKind : String          -- "all" | "only" | "none"
  mcpProbe      : String
  mcpPermits    : Bool
  writeProbe    : String × String
  writeFields   : List String       -- effective field set at writeProbe (sorted)
  deriving Repr

structure Case where
  name       : String
  behavior   : SurfaceView
  ceiling    : SurfaceView
  runtime    : SurfaceView
  expected   : SurfaceView          -- = view (effective behavior ceiling runtime)
  deriving Repr
```

> Provide `view : Surface → ... → SurfaceView` that fills the probes (`mcpPermits := decide (s.mcpServices.permits mcpProbe)`, `writeFields := (s.writeTools.lookup writeProbe).getD ∅ |>.sort (· ≤ ·)`, `bashAllowedKind` by matching `s.bash.allowed`). `decide` needs `Decidable (permits …)`; add the instance in `Meet.lean` (`permits` on `none`/`all`/`only` is decidable: `False`/`True`/`Finset` membership).

- [ ] **Step 2: Build the witness cases (full Surfaces) and compute expected via `effective`**

Define `secureMinimal`, `wideOpen`, `ceilingWriteFieldsNarrowed`, `runtimeNoMcp` as full `Surface`s, then:

```lean
def mkCase (name : String) (b c : Surface) (r : Avail) (mcpProbe : String)
    (writeProbe : String × String) : Case :=
  { name, behavior := view b mcpProbe writeProbe
  , ceiling := view c mcpProbe writeProbe
  , runtime := view r mcpProbe writeProbe
  , expected := view (effective b c r) mcpProbe writeProbe }

def cases : List Case :=
  [ mkCase "wide_open_clamped_by_secure_ceiling" wideOpen secureMinimal wideOpen "svc-a" ("wt","coll")
  , mkCase "ceiling_mcp_only_clamps_behavior" wideOpen ceilingMcpOnly wideOpen "svc-a" ("wt","coll")
  , mkCase "runtime_offline_drops_permitted_mcp" wideOpen wideOpen runtimeNoMcp "svc-a" ("wt","coll")
  , mkCase "write_fields_narrowed_by_ceiling" wideOpen ceilingWriteFieldsNarrowed wideOpen "svc-a" ("wt","coll")
  , mkCase "bash_empty_allowed_is_all" wideOpen wideOpen wideOpen "svc-a" ("wt","coll") ]
```

- [ ] **Step 3: Append barrel import, build, commit**

Append `import Proofs.ToolPolicy.Cases` to `ToolPolicy.lean`.
Run: `cd crates/defra-agent/proofs && lake build` → clean.

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Cases.lean crates/defra-agent/proofs/Proofs/ToolPolicy.lean
git commit -m "feat(proofs): ToolPolicy conformance cases carrying inputs + expected output"
```

---

## Task 12: JSON serializer (`Contracts/Json/ToolPolicy.lean`)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ToolPolicy.lean`

**Interfaces:**
- Produces: `Conformance.Contracts.toolPolicyCasesJson : String`.

- [ ] **Step 1: Mirror `Json/CommandPolicy.lean`'s shape**

```lean
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.ToolPolicy.Cases

namespace Conformance.Contracts
open ToolPolicy.ContractCases

def surfaceViewJson (v : SurfaceView) : String :=
  "{" ++
  "\"file_rank\":" ++ toString v.fileRank ++ "," ++
  "\"meta\":" ++ (if v.meta then "true" else "false") ++ "," ++
  "\"defra_query\":" ++ (if v.defraQuery then "true" else "false") ++ "," ++
  "\"spawn\":" ++ (if v.spawn then "true" else "false") ++ "," ++
  "\"bash_mode\":" ++ toString v.bashMode ++ "," ++
  "\"bash_net\":" ++ toString v.bashNet ++ "," ++
  "\"bash_sandbox\":" ++ (if v.bashSandbox then "true" else "false") ++ "," ++
  "\"bash_allowed_kind\":" ++ jsonString v.bashAllowedKind ++ "," ++
  "\"mcp_probe\":" ++ jsonString v.mcpProbe ++ "," ++
  "\"mcp_permits\":" ++ (if v.mcpPermits then "true" else "false") ++ "," ++
  "\"write_fields\":" ++ jsonArray (v.writeFields.map jsonString) ++
  "}"

def toolPolicyCaseJson (c : ToolPolicy.ContractCases.Case) : String :=
  "{" ++
  "\"name\":" ++ jsonString c.name ++ "," ++
  "\"behavior\":" ++ surfaceViewJson c.behavior ++ "," ++
  "\"ceiling\":" ++ surfaceViewJson c.ceiling ++ "," ++
  "\"runtime\":" ++ surfaceViewJson c.runtime ++ "," ++
  "\"expected\":" ++ surfaceViewJson c.expected ++
  "}"

def toolPolicyCasesJson : String :=
  jsonArray (ToolPolicy.ContractCases.cases.map toolPolicyCaseJson)

end Conformance.Contracts
```

> Confirm `jsonString`/`jsonArray` signatures against `Json/Helpers.lean` (they exist; `Json/CommandPolicy.lean` uses them). `write_probe` (the key) is implicit/constant across cases ("wt","coll"); if a test needs it, add `"write_probe_tool"`/`"write_probe_collection"` keys to `surfaceViewJson`.

- [ ] **Step 2: Build + commit**

Run: `cd crates/defra-agent/proofs && lake build` → clean (the file is unused until Task 13 imports it; build still type-checks it once a later barrel pulls it — to force it now, temporarily `import` it from `Snapshot.lean` in Task 13).

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ToolPolicy.lean
git commit -m "feat(proofs): tool-policy conformance JSON serializer"
```

---

## Task 13: Wire into `snapshotJson`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean`

**Interfaces:**
- Produces: a `"tool_policy_cases"` key in the emitted snapshot.

- [ ] **Step 1: Import + emit**

Add to the import block: `import Proofs.Conformance.Contracts.Json.ToolPolicy`
Add after the `apply_reconcile_cases` line (Snapshot.lean ~:56):

```lean
    ++ "\"tool_policy_cases\":" ++ toolPolicyCasesJson ++ ","
```

- [ ] **Step 2: Build + run the emitter to verify the JSON**

Run: `cd crates/defra-agent/proofs && lake build`
Then: `lake env lean --run Proofs/Conformance/Contracts.lean | sed -n '/BEGIN DEFRA LEAN CONTRACT JSON/,/END/p' | grep -o '"tool_policy_cases":\[.\{0,120\}' | head`
Expected: the array with the five named cases.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean
git commit -m "feat(proofs): emit tool_policy_cases into the conformance snapshot"
```

---

## Task 14: Rust ingest — snapshot field + case/input structs

> **Ordering (review #4):** this task adds the `LeanContractSnapshot.tool_policy_cases` field, which **must exist before** Task 15's `coverage.rs` edit references `snapshot.tool_policy_cases`. Ingest precedes coverage.

**Files:**
- Create: `crates/defra-agent/src/lean_vocab_test/tool_policy.rs`
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`

**Interfaces:**
- Produces: `LeanToolPolicyCase { name, behavior, ceiling, runtime, expected }`, `LeanToolPolicySurfaceView { … }` (snake_case, mirroring Task 12 JSON), `lean_tool_policy_cases()` / `lean_tool_policy_case(name)`.

- [ ] **Step 1: Create the case structs**

`src/lean_vocab_test/tool_policy.rs`:

```rust
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicySurfaceView {
    pub(crate) file_rank: u8,
    pub(crate) meta: bool,
    pub(crate) defra_query: bool,
    pub(crate) spawn: bool,
    pub(crate) bash_mode: u8,
    pub(crate) bash_net: u8,
    pub(crate) bash_sandbox: bool,
    pub(crate) bash_allowed_kind: String,
    pub(crate) mcp_probe: String,
    pub(crate) mcp_permits: bool,
    pub(crate) write_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicyCase {
    pub(crate) name: String,
    pub(crate) behavior: LeanToolPolicySurfaceView,
    pub(crate) ceiling: LeanToolPolicySurfaceView,
    pub(crate) runtime: LeanToolPolicySurfaceView,
    pub(crate) expected: LeanToolPolicySurfaceView,
}
```

- [ ] **Step 2: Wire into the snapshot + module + accessors**

In `lean_vocab_test.rs`: add `#[path = "lean_vocab_test/tool_policy.rs"] mod tool_policy;` with the other submodules (~:234); re-export the types (`pub(crate) use tool_policy::{LeanToolPolicyCase, LeanToolPolicySurfaceView};`); add to `LeanContractSnapshot` (~:41):

```rust
    #[serde(default)]
    pub(crate) tool_policy_cases: Vec<LeanToolPolicyCase>,
```

Add accessors near `lean_apply_reconcile_case` (~:327):

```rust
pub(crate) fn lean_tool_policy_cases() -> &'static [LeanToolPolicyCase] {
    &lean_contract_snapshot().tool_policy_cases
}
pub(crate) fn lean_tool_policy_case(name: &str) -> &'static LeanToolPolicyCase {
    lean_tool_policy_cases().iter().find(|c| c.name == name)
        .unwrap_or_else(|| panic!("missing lean tool-policy case: {name}"))
}
```

- [ ] **Step 3: Compile + commit**

Run: `cargo test -p defra-agent --no-run`
Expected: compiles.

```bash
git add crates/defra-agent/src/lean_vocab_test.rs crates/defra-agent/src/lean_vocab_test/tool_policy.rs
git commit -m "test(conformance): ingest tool_policy_cases (inputs + expected) into lean_vocab_test"
```

---

## Task 15: Coverage ledger + coverage.rs registration

> **Depends on Task 14** (the `snapshot.tool_policy_cases` field must already exist before the `coverage.rs` emission check below references it — review #4).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/conformance/coverage.rs`

**Interfaces:**
- Produces: the `(tool_policy_cases, ToolPolicyCases)` domain accounted on both sides. **This is the fence the reviewer flagged — the snapshot domain MUST equal the ledger domain or `lean_contract_coverage_ledger_accounts_for_every_emitted_domain` fails.**

- [ ] **Step 1: Ledger entry (Lean)**

In `CoverageLedger.lean`, in the `caseCoverage` list (~:406, near the ApplyReconcile entry ~:437), add:

```lean
  , tagged (consumerCoverage
      "tool_policy_cases"
      "ToolPolicyCases"
      "conformance::generated_tool_policy_cases_match_lean_composition")
      "tool-policy" [Surface.operatorUi, Surface.agentFacing]
```

> Confirm `Surface.operatorUi`/`Surface.agentFacing` exist (they are used by the CommandPolicy entry). The consumer string must equal the registered id in Task 18.

- [ ] **Step 2: coverage.rs emission check**

In `coverage.rs`, after the `apply_reconcile_cases` emission block (~:516):

```rust
    if !snapshot.tool_policy_cases.is_empty() {
        emitted.insert((
            "tool_policy_cases".to_string(),
            "ToolPolicyCases".to_string(),
        ));
    }
```

- [ ] **Step 3: coverage.rs `valid_categories`**

Add `"tool_policy_cases",` to the `valid_categories` array (~:764-805), in sorted position.

- [ ] **Step 4: Build proofs + compile the test target**

Run: `cd crates/defra-agent/proofs && lake build` (clean)
Then: `cargo test -p defra-agent --no-run` — this now compiles, since Task 14 added the `snapshot.tool_policy_cases` field the emission check references. (The coverage fence itself runs at Task 17/19's gate.)

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean crates/defra-agent/tests/conformance/coverage.rs
git commit -m "test(conformance): account tool_policy_cases in coverage ledger + coverage.rs"
```

---

## Task 16: The Rust meet mirror (re-derivation)

**Files:**
- Create: `crates/defra-agent/tests/conformance/tool_policy_mirror.rs`

**Interfaces:**
- Consumes: `LeanToolPolicySurfaceView`.
- Produces: `pub(super) fn rederive(behavior, ceiling, runtime) -> LeanToolPolicySurfaceView` — a pure Rust re-implementation of the meet, projected to the same `SurfaceView` probes. **This is the artifact SP1-Rust deletes and replaces with the real resolver.**

- [ ] **Step 1: Implement the meet on the reduced view**

```rust
//! Pure Rust mirror of the Lean ToolPolicy meet, on the reduced SurfaceView.
//! SP1-Rust replaces this with the production resolver pointed at the same
//! cases; until then it is the independent re-derivation that catches drift.

use crate::lean_vocab_test::LeanToolPolicySurfaceView as View;

fn meet_min(a: u8, b: u8) -> u8 { a.min(b) }

/// allowed-gate kind meet: all ⊓ x = x; none ⊓ _ = none; only ⊓ only = only
/// (key intersection may be empty -> stays "only", i.e. deny-all, NOT "all").
fn meet_allowed_kind(a: &str, b: &str) -> String {
    match (a, b) {
        ("none", _) | (_, "none") => "none",
        ("all", x) => x,
        (x, "all") => x,
        _ => "only",
    }
    .to_string()
}

fn meet2(a: &View, b: &View) -> View {
    View {
        file_rank: meet_min(a.file_rank, b.file_rank),
        meta: a.meta && b.meta,
        defra_query: a.defra_query && b.defra_query,
        spawn: a.spawn && b.spawn,
        bash_mode: meet_min(a.bash_mode, b.bash_mode),
        bash_net: meet_min(a.bash_net, b.bash_net),
        bash_sandbox: a.bash_sandbox && b.bash_sandbox,
        bash_allowed_kind: meet_allowed_kind(&a.bash_allowed_kind, &b.bash_allowed_kind),
        mcp_probe: a.mcp_probe.clone(),
        mcp_permits: a.mcp_permits && b.mcp_permits,
        write_fields: {
            let bf: std::collections::BTreeSet<_> = b.write_fields.iter().cloned().collect();
            let mut v: Vec<String> =
                a.write_fields.iter().filter(|f| bf.contains(*f)).cloned().collect();
            v.sort();
            v
        },
    }
}

pub(super) fn rederive(behavior: &View, ceiling: &View, runtime: &View) -> View {
    meet2(&meet2(behavior, ceiling), runtime)
}
```

> `mcp_permits` mirror: the reduced view already encodes whether each operand permits the probe, so the meet is `&&`. This is faithful because the Lean `view` computed `permits` per operand; SP1-Rust will instead carry the scope sets and re-derive membership against the real resolver.

- [ ] **Step 2: Compile (registered in Task 17) + commit**

```bash
git add crates/defra-agent/tests/conformance/tool_policy_mirror.rs
git commit -m "test(conformance): pure Rust meet mirror for tool-policy re-derivation"
```

---

## Task 17: The fenced conformance test + registration

**Files:**
- Create: `crates/defra-agent/tests/conformance/tool_policy.rs`
- Modify: `crates/defra-agent/tests/conformance.rs`
- Modify: `crates/defra-agent/tests/conformance/structure.rs`

**Interfaces:**
- Consumes: `lean_tool_policy_cases`, `tool_policy_mirror::rederive`.
- Produces: `pub(super) fn generated_tool_policy_cases_match_lean_composition()`.

- [ ] **Step 1: Write the re-derivation test (mirror `tool_execution.rs` visibility)**

`tests/conformance/tool_policy.rs`:

```rust
//! ToolPolicy conformance home: re-derive the effective surface from the
//! Lean-emitted (behavior, ceiling, runtime) inputs and assert it equals the
//! Lean-emitted expected output — catching any resolver-side drift.

use super::*;
use crate::lean_vocab_test::lean_tool_policy_cases;

#[path = "tool_policy_mirror.rs"]
mod tool_policy_mirror;

pub(super) fn generated_tool_policy_cases_match_lean_composition() {
    let cases = lean_tool_policy_cases();
    assert!(!cases.is_empty(), "no tool-policy cases emitted by Lean");
    for c in cases {
        let got = tool_policy_mirror::rederive(&c.behavior, &c.ceiling, &c.runtime);
        assert_eq!(
            got, c.expected,
            "case {}: Rust re-derivation diverged from Lean effective surface",
            c.name
        );
        // Spot-check the headline safety law directly on the expected output.
        assert!(
            c.expected.file_rank <= c.ceiling.file_rank,
            "case {}: effective file rank exceeds ceiling", c.name
        );
        if c.expected.mcp_permits {
            assert!(c.ceiling.mcp_permits,
                "case {}: effective permits an MCP service the ceiling forbids", c.name);
        }
        // empty-allowed=all stays all only when no narrowing occurred.
        if c.name == "bash_empty_allowed_is_all" {
            assert_eq!(c.expected.bash_allowed_kind, "all");
        }
    }
}
```

- [ ] **Step 2: Register module + wrapper in `conformance.rs`**

```rust
#[path = "conformance/tool_policy.rs"]
mod tool_policy;
```
and the wrapper `#[test]`:
```rust
#[test]
fn generated_tool_policy_cases_match_lean_composition() {
    tool_policy::generated_tool_policy_cases_match_lean_composition();
}
```

- [ ] **Step 3: Structure fence home**

In `structure.rs` `model_homes()`, add: `("ToolPolicy", Module("conformance/tool_policy.rs")),`

- [ ] **Step 4: Run the targeted tests**

Run: `cargo test -p defra-agent --test conformance generated_tool_policy_cases_match_lean_composition -- --nocapture`
Then: `cargo test -p defra-agent --test conformance every_lean_model_has_a_declared_conformance_home`
Then: `cargo test -p defra-agent --test conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/conformance/tool_policy.rs crates/defra-agent/tests/conformance.rs crates/defra-agent/tests/conformance/structure.rs
git commit -m "test(conformance): ToolPolicy re-derivation fenced + structure home"
```

---

## Task 18: Register the consumer + note the SP1-Rust handoff

**Files:**
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`

**Interfaces:**
- Produces: the resolved consumer id matching the ledger string from Task 15.

- [ ] **Step 1: Register**

In `registered_conformance_consumers()`:

```rust
ConformanceConsumer::RustTest {
    id: "conformance::generated_tool_policy_cases_match_lean_composition",
    package: "defra-agent",
    source_path: "crates/defra-agent/tests/conformance.rs",
    module_path: "conformance",
    function: "generated_tool_policy_cases_match_lean_composition",
},
```

- [ ] **Step 2: Run the consumer-resolution test**

Run: `cargo test -p defra-agent --test conformance -- --list | grep -i consumer` (find the test name), then run it.
Expected: PASS (id resolves to the real fn).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/support/conformance_consumers.rs
git commit -m "test(conformance): register ToolPolicy conformance consumer id"
```

---

## Task 19: Full-suite gate + SP1-Rust handoff notes

**Files:**
- Modify: `docs/superpowers/specs/2026-06-26-unified-tool-policy-design.md` (append a short "SP1-Rust carries" note — non-code)

**Interfaces:** none (acceptance).

- [ ] **Step 1: Append the handoff note to the spec**

Add a short subsection under §5 listing what SP1-Rust must do that this model assumes/defers: (a) decide MCP health strictness and build the `RuntimeAvailability` snapshot feeding `effective`; (b) represent `Only(∅)` (deny-all allowed-gate) on the wire — it is NOT an empty list; (c) replace `tool_policy_mirror.rs` with the production resolver pointed at the same cases; (d) the structured `lookup`/root-narrowing precise semantics (cli path-prefix containment) that this model approximates with `Finset` intersection.

- [ ] **Step 2: The real acceptance check — full package suite**

Run: `cargo test -p defra-agent`
Expected: ENTIRE suite passes — the new re-derivation test, structure fence, coverage ledger, and consumer ledger included. Capture the summary line.

- [ ] **Step 3: Lean clean-build confirmation**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | tail -3` and confirm no `sorry`/error.
Run: `grep -rn "sorry\|admit" crates/defra-agent/proofs/Proofs/ToolPolicy*` → expect no matches.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-26-unified-tool-policy-design.md
git commit -m "docs: SP1-Rust handoff notes (availability, Only(empty) wire, resolver mirror)"
```

---

## Self-Review (completed during authoring)

**Spec coverage (`2026-06-26-unified-tool-policy-design.md`):**
- §3.1 `Capability` rank → Tasks 2, 3 (file), boolean fields (Tasks 2, 9). ✔
- §3.1 `BashPolicy` product, **all fields**, empty-allowed=all + `Only(∅)`≠`All` → Tasks 2, 5, 6. ✔ (review #2)
- §3.1 **structured `EndpointScope K V`** with value narrowing (write fields/collections via key=(tool,collection)+fields∩; cli roots; subagent targets) → Tasks 1, 2, 4, 8, 10. ✔ (review #1)
- §3.3 `effective = behavior ⊓ ceiling ⊓ runtime`, `⊆ ceiling`/`⊆ behavior`, all categories → Tasks 8, 9. ✔
- §3.3 runtime availability as precomputed input → Tasks 2, 11; strictness deferred to SP1-Rust (Task 19). ✔ (pre-flight #3)
- §4 meet algebra → Task 10. conformance mirror that **re-derives** (not tautological) → Tasks 11, 16, 17. ✔ (review #3)
- §5 context_budget gate (Task 2 field + Task 9 lemma); skills/load_skill category (Task 2 + Task 9). ✔ (pre-flight #4)

**Repo-integration correctness (review #4–#6):**
- Root barrel `Proofs.lean` + new `Proofs/ToolPolicy.lean` (no `DefraAgent.lean`) → Tasks 1, 3, 6, 8, 11. ✔ (#4)
- `Contracts/Json/Snapshot.lean` (not the `Json.lean` barrel) + per-domain `Json/ToolPolicy.lean` + snake_case keys → Tasks 12, 13. ✔ (#5a)
- coverage.rs emission check + `valid_categories` pinned list + ledger domain match → Task 15 (after the Task 14 ingest field exists — review #4 ordering). No new Lean boundary added (availability is a pure input), so the `expected_boundary_ids` pinned list is untouched. ✔ (#5b, #4)
- `pub(super) fn` child + `#[test]` wrapper in parent → Task 17. ✔ (#6)

**Type consistency:** `Surface` fields (Task 2) used verbatim in Tasks 8, 9, 11. `SurfaceView` fields (Task 11) ↔ `LeanToolPolicySurfaceView` (Task 14 ingest) ↔ JSON keys (Task 12), all snake_case. Test name `generated_tool_policy_cases_match_lean_composition` identical in Tasks 15 (ledger), 17 (test), 18 (consumer). Ledger consumer string (Task 15) == registered id (Task 18).

**Review #1 + #2 closure:** endpoint uniqueness is now structural — `EndpointScope.only` carries `(keys : Finset K) (val : K → V)` (a finite map), so `lookup` is single-valued and `meet_lookup_vle_*` is provable, not assumed (Tasks 1, 4). Bash `permits` now denotes all six fields incl. `mode` + `readOnly` allowlist, with per-field lower-bound lemmas and the mode-gate transfer (Tasks 5, 6). The "meet-semilattice" claim is narrowed to lower-bound-safety + idempotence + atom-commutativity, with associativity explicitly out of scope (Goal + Task 10, review #3).

**Known iteration points (not placeholders — flagged proof risk):** the **bash mode-gate lower bound** (Task 6 `bash_meet_mode_gate_left`, the `min`-mode reasoning) is the hardest proof; the `EndpointScope` `only/only` meet cases (Tasks 4, 10) are next. Each has an explicit fallback tactic inline. The `effective_write_fields_narrow` corollary (Task 10) assumes `runtime.writeTools = .all` (true for every modeled case) to keep the outer-meet handling trivial.

**Intentional SP1-Lean/SP1-Rust seam (NOT a gap):** the production resolver, retyped `ToolSelection`, category-complete Rust `ToolCeiling`, decode/version/backfill, `ToolSurfaceExplanation` rework, parity, presets, and the precise cli-root/`Only(∅)`-wire semantics live in SP1-Rust, which mirrors THIS model and replaces `tool_policy_mirror.rs`.
