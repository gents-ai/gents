# Unified Tool Policy — SP1-Lean Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Lean formal model of unified tool-policy composition — a per-category meet-semilattice with the theorem that the effective tool surface is a subset of the operator ceiling (and of the behavior policy) for every category — plus its Rust conformance mirror.

**Architecture:** A new `Proofs/ToolPolicy/` Lean module mirrors the existing `CommandPolicy/` layout (Types / Meet / Theorems / Cases). Categories are modeled with three atom families: a ranked `Capability` (file modes + boolean gates), a `BashPolicy` product (reusing `CommandPolicy.Types.Policy`), and an `EndpointScope` (`none`/`only(Finset)`/`all`). `meet` is defined per atom and lifted pointwise to an aggregate `ToolPolicy`. `effective = behavior ⊓ ceiling ⊓ runtime`. Conformance follows the established `Contracts.lean main` → JSON → `lean_vocab_test.rs` → fenced test path.

**Tech Stack:** Lean 4 `v4.18.0` + Mathlib `v4.18.0` (`Finset` only — concrete ops `∩`/`⊆`, NOT Mathlib lattice typeclasses, matching `Skills.lean`); Rust conformance via `cargo test -p defra-agent`.

## Global Constraints

- **Lean toolchain:** `leanprover/lean4:v4.18.0`; Mathlib pinned `v4.18.0` (`proofs/lakefile.lean`, `proofs/lean-toolchain`). Do not bump.
- **Zero `sorry`s.** Every theorem complete, or explicitly recorded as a boundary in `Proofs/Conformance/Boundaries.lean`. No `sorry`, no `admit`, no `native_decide` shortcuts on the safety theorems.
- **Build gate:** `cd crates/defra-agent/proofs && lake build` must be clean before any commit that touches `proofs/`.
- **Conformance gate:** `cargo test -p defra-agent` (the FULL package suite, never `--lib` — per CLAUDE.md, integration tests are separate compile units).
- **`autoImplicit false`** is set package-wide (`lakefile.lean`); declare all implicit binders explicitly.
- **Mirror the `CommandPolicy/` file split** (Types / Validation-or-Meet / Theorems / Cases) — follow existing patterns; do not invent a new layout.
- **Empty-list semantics for argv prefixes:** an empty `allowedArgvPrefixes` means *no allowed-prefix gate* (allow-all), matching `CommandPolicy/Validation.lean`'s `match … | [] => …` and Rust `command.rs:288`. The `BashPolicy` meet must preserve this (empty ≡ `all`, never `only ∅`).
- **Four pre-flight constraints carried from the spec** (`docs/superpowers/specs/2026-06-26-unified-tool-policy-design.md`): (1) agent_did-scoped selection load is SP1-Rust, not modeled here; (2) bash empty-allowed=All — Task 4; (3) MCP health strictness (Healthy-only vs not-Unreachable) — modeled as an availability input + a `stale` conformance case, Tasks 5 & 9, decision recorded in Boundaries; (4) `load_skill`/generated tools — the aggregate carries a `skills` category so the model is category-complete, Task 6.
- **This plan is Lean + conformance ONLY.** No `src/` runtime changes, no schema/protocol/desktop edits — those are SP1-Rust.

---

## File Structure

**New Lean files (all under `crates/defra-agent/proofs/Proofs/ToolPolicy/`):**
- `Types.lean` — atom + aggregate type definitions (`Capability`, `EndpointScope`, `BashPolicy`, `CategoryKind`, `ToolPolicy`, `RuntimeAvailability`).
- `Meet.lean` — `meet` for each atom + the pointwise aggregate `meet`, and the `permits`/`rankLe` denotations the theorems are stated against.
- `Theorems.lean` — meet algebra (idempotent/commutative/lower-bound) + headline `effective_subset_ceiling` / `effective_subset_behavior`.
- `Cases.lean` — finite witness rows for conformance (`ToolPolicyCase` list), incl. the bash-empty-allowed case and the runtime-stale case.

**Modified Lean files:**
- `crates/defra-agent/proofs/Proofs/DefraAgent.lean` — add `import Proofs.ToolPolicy.Theorems` (and `Cases`) to the root so `lake build` compiles them.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` — fold `ToolPolicy` cases into `snapshotJson`.
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean` (or a new `ContractCases/ToolPolicy.lean` imported by it) — register the case list for emission.
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` — add the `ToolPolicy` consumer entry.

**Modified Rust files:**
- `crates/defra-agent/src/lean_vocab_test.rs` — add `LeanToolPolicyCase` struct + `lean_tool_policy_case(name)` accessor; ingest the new JSON section.
- `crates/defra-agent/tests/conformance/tool_policy.rs` — NEW: the fenced conformance test.
- `crates/defra-agent/tests/conformance.rs` — declare `mod tool_policy;` + wrapper `#[test]`.
- `crates/defra-agent/tests/conformance/structure.rs` — add `("ToolPolicy", Module("conformance/tool_policy.rs"))` to `model_homes()`.
- `crates/defra-agent/tests/support/conformance_consumers.rs` — register the consumer id.

---

## Task 1: Scaffold `ToolPolicy/Types.lean` with the atom types

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/DefraAgent.lean` (add import)

**Interfaces:**
- Produces: `ToolPolicy.FileCap` (`.off|.readOnly|.readWrite`), `ToolPolicy.EndpointScope (α)` (`.none|.only (Finset α)|.all`), `ToolPolicy.ToolId := String`. Consumed by every later ToolPolicy task.

- [ ] **Step 1: Write the type definitions (this is the "failing test" — it must compile)**

Create `Proofs/ToolPolicy/Types.lean`:

```lean
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Lattice.Basic
import Proofs.CommandPolicy.Types

/-!
# Tool Policy — Types

Per-category vocabulary for unified tool-policy composition. Three atom
families: a ranked `FileCap`, boolean capabilities (`Bool`), and
`EndpointScope` for endpoint-style categories. Bash reuses
`CommandPolicy.Types.Policy` (see `BashPolicy` in this module).
-/

namespace ToolPolicy

abbrev ToolId := String

/-- Ranked file capability (mirrors Rust `FileToolMode`). -/
inductive FileCap where
  | off
  | readOnly
  | readWrite
  deriving DecidableEq, Repr

/-- A bounded scope over endpoint-style categories (MCP services, defra
    collections, subagent targets, cli tools, backgroundable tools,
    write-tool names). `none` ≤ `only s` ≤ `all`. -/
inductive EndpointScope (α : Type) [DecidableEq α] where
  | none
  | only (s : Finset α)
  | all
  deriving Repr

end ToolPolicy
```

- [ ] **Step 2: Wire the import into the root and build**

Edit `Proofs/DefraAgent.lean` — add near the other imports:

```lean
import Proofs.ToolPolicy.Types
```

- [ ] **Step 3: Run the build to verify it compiles**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build (no errors, no `sorry`).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean crates/defra-agent/proofs/Proofs/DefraAgent.lean
git commit -m "feat(proofs): scaffold ToolPolicy atom types (FileCap, EndpointScope)"
```

---

## Task 2: Add the aggregate `ToolPolicy`, `RuntimeAvailability`, and `BashPolicy` alias

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean`

**Interfaces:**
- Consumes: `FileCap`, `EndpointScope`, `CommandPolicy.Types.Policy`.
- Produces: `ToolPolicy.BashPolicy := CommandPolicy.Policy`; structures `ToolPolicy.Surface` (the aggregate policy, one field per category) and `ToolPolicy.Avail` (runtime availability). Field names below are the contract for all later tasks.

- [ ] **Step 1: Add the aggregate + availability types**

Append to `Proofs/ToolPolicy/Types.lean` (before the final `end ToolPolicy`):

```lean
/-- Bash carries a full command-execution policy, not a single rank. -/
abbrev BashPolicy := CommandPolicy.Policy

/-- The full per-category tool policy. Used at three levels: behavior
    request, operator ceiling, and (the availability-shaped subset)
    runtime. -/
structure Surface where
  file              : FileCap
  bash              : BashPolicy
  meta              : Bool
  defraQuery        : Bool
  memory            : Bool
  sessionHistory    : Bool
  contextBudget     : Bool
  spawn             : Bool
  steering          : Bool
  background        : Bool
  orchestration     : Bool
  crossDeployment   : Bool
  skills            : Bool
  cliTools          : EndpointScope ToolId
  mcpServices       : EndpointScope ToolId
  defraCollections  : EndpointScope ToolId
  subagentTargets   : EndpointScope ToolId
  backgroundTools   : EndpointScope ToolId
  writeTools        : EndpointScope ToolId

/-- Runtime availability, expressed as a `Surface` so it composes by the
    same meet. MCP availability is the online+healthy service set; feature
    gates (memory) are booleans; everything else is `all`/`true` (runtime
    does not restrict it). -/
abbrev Avail := Surface
```

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Types.lean
git commit -m "feat(proofs): add ToolPolicy.Surface aggregate + Avail + BashPolicy alias"
```

---

## Task 3: `FileCap` rank + meet + the rank-order lemma

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`
- Modify: `crates/defra-agent/proofs/Proofs/DefraAgent.lean` (import)

**Interfaces:**
- Produces: `FileCap.rank : FileCap → Nat`, `FileCap.meet : FileCap → FileCap → FileCap`, lemma `FileCap.meet_rank_le_left`/`_right`.

- [ ] **Step 1: Write the meet + the lemma statements (proofs first as the failing target)**

Create `Proofs/ToolPolicy/Meet.lean`:

```lean
import Proofs.ToolPolicy.Types

namespace ToolPolicy

def FileCap.rank : FileCap → Nat
  | .off => 0
  | .readOnly => 1
  | .readWrite => 2

/-- Meet = the lower-ranked mode (no more permissive than either side). -/
def FileCap.meet (a b : FileCap) : FileCap :=
  if a.rank ≤ b.rank then a else b

theorem FileCap.meet_rank_le_left (a b : FileCap) :
    (a.meet b).rank ≤ a.rank := by
  unfold FileCap.meet
  by_cases h : a.rank ≤ b.rank <;> simp [h]

theorem FileCap.meet_rank_le_right (a b : FileCap) :
    (a.meet b).rank ≤ b.rank := by
  unfold FileCap.meet
  by_cases h : a.rank ≤ b.rank <;> simp [h]
  · exact h
  · exact Nat.le_of_lt (Nat.lt_of_not_le h)

end ToolPolicy
```

- [ ] **Step 2: Import into root + build (verify the lemmas close)**

Edit `Proofs/DefraAgent.lean`: `import Proofs.ToolPolicy.Meet`

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean. If a `by_cases`/`simp` branch fails, adjust with `omega` for the `Nat` arithmetic (the `rank` values are concrete) — e.g. replace the failing branch with `· omega` after `simp [FileCap.rank] at *`.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean crates/defra-agent/proofs/Proofs/DefraAgent.lean
git commit -m "feat(proofs): FileCap rank + meet + rank-order lemmas"
```

---

## Task 4: `EndpointScope` meet + `BashPolicy` meet (empty-allowed = all) + denotations

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`

**Interfaces:**
- Produces: `EndpointScope.permits : EndpointScope α → α → Prop`, `EndpointScope.meet`, `BashPolicy.meet : BashPolicy → BashPolicy → BashPolicy`, `Bool.meet := (· && ·)` (use `&&` directly). The `permits` predicate is the denotation all subset theorems are stated against.

- [ ] **Step 1: Add EndpointScope meet + permits + Bash meet**

Append to `Proofs/ToolPolicy/Meet.lean` (before `end ToolPolicy`):

```lean
variable {α : Type} [DecidableEq α]

/-- Does this scope permit element `x`? `none` permits nothing; `all`
    permits everything; `only s` permits exactly `s`. -/
def EndpointScope.permits : EndpointScope α → α → Prop
  | .none, _ => False
  | .only s, x => x ∈ s
  | .all, _ => True

def EndpointScope.meet : EndpointScope α → EndpointScope α → EndpointScope α
  | .none, _ => .none
  | _, .none => .none
  | .all, b => b
  | a, .all => a
  | .only s, .only t => .only (s ∩ t)

/-- Network permissiveness rank: disabled is strictest. -/
def networkRank : CommandPolicy.NetworkMode → Nat
  | .disabled => 0
  | .inherit => 1
  | .enabled => 2

def execRank : CommandPolicy.ExecutionMode → Nat
  | .readOnly => 0
  | .workspaceWrite => 1
  | .unrestricted => 2

/-- Interpret an `allowedArgvPrefixes` list as a scope: empty list means
    "no allowed-prefix gate" = `all`; non-empty means `only` that set.
    (Mirrors `CommandPolicy/Validation.lean` and Rust `command.rs:288`.) -/
def allowedScope (prefixes : List (List String)) : EndpointScope (List String) :=
  match prefixes with
  | [] => .all
  | _ => .only prefixes.toFinset

/-- Bash meet: per-field, never more permissive than either operand.
    mode/network: lower rank; forbidden: union; allowed: scope-meet
    (empty = all); read-only allowlist: intersection. -/
def BashPolicy.meet (a b : BashPolicy) : BashPolicy :=
  { mode := if execRank a.mode ≤ execRank b.mode then a.mode else b.mode
  , networkMode :=
      if networkRank a.networkMode ≤ networkRank b.networkMode then a.networkMode
      else b.networkMode
  , forbiddenArgvPrefixes := (a.forbiddenArgvPrefixes ++ b.forbiddenArgvPrefixes).dedup
  , allowedArgvPrefixes :=
      match allowedScope a.allowedArgvPrefixes, allowedScope b.allowedArgvPrefixes with
      | .all, _ => b.allowedArgvPrefixes
      | _, .all => a.allowedArgvPrefixes
      | _, _ => (a.allowedArgvPrefixes.toFinset ∩ b.allowedArgvPrefixes.toFinset).toList
  , readOnlyAllowlist := (a.readOnlyAllowlist.toFinset ∩ b.readOnlyAllowlist.toFinset).toList }
```

> Note on `Policy` field names: confirm against `CommandPolicy/Types.lean` (`mode`, `allowedArgvPrefixes`, `forbiddenArgvPrefixes`, `networkMode`, `readOnlyAllowlist`). If `Policy` has no anonymous-constructor support due to a custom `deriving`, build the record with `{ a with mode := … }` instead.

- [ ] **Step 2: Add the EndpointScope subset lemmas (the failing test)**

Append:

```lean
theorem EndpointScope.meet_permits_left
    (a b : EndpointScope α) (x : α) :
    (a.meet b).permits x → a.permits x := by
  cases a <;> cases b <;>
    simp [EndpointScope.meet, EndpointScope.permits, Finset.mem_inter] <;>
    intro h <;> tauto

theorem EndpointScope.meet_permits_right
    (a b : EndpointScope α) (x : α) :
    (a.meet b).permits x → b.permits x := by
  cases a <;> cases b <;>
    simp [EndpointScope.meet, EndpointScope.permits, Finset.mem_inter] <;>
    intro h <;> tauto
```

- [ ] **Step 3: Build (iterate proofs against `lake build`)**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean. The `only s, only t` case reduces to `x ∈ s ∩ t → x ∈ s` (`Finset.mem_inter.mp`); if `tauto` stalls, replace with explicit `exact fun h => (Finset.mem_inter.mp h).1` (left) / `.2` (right). The `none`/`all` cases are `False.elim`/`trivial`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): EndpointScope + BashPolicy meet with empty-allowed=all + subset lemmas"
```

---

## Task 5: Aggregate `Surface.meet` (pointwise) + `effective`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean`

**Interfaces:**
- Produces: `Surface.meet : Surface → Surface → Surface`, `effective : Surface → Surface → Avail → Surface` (= `behavior.meet ceiling |>.meet runtime`).

- [ ] **Step 1: Write `Surface.meet` and `effective`**

Append to `Meet.lean`:

```lean
/-- Pointwise meet of two full policies. -/
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
  , cliTools         := a.cliTools.meet b.cliTools
  , mcpServices      := a.mcpServices.meet b.mcpServices
  , defraCollections := a.defraCollections.meet b.defraCollections
  , subagentTargets  := a.subagentTargets.meet b.subagentTargets
  , backgroundTools  := a.backgroundTools.meet b.backgroundTools
  , writeTools       := a.writeTools.meet b.writeTools }

/-- Effective surface = behavior ⊓ ceiling ⊓ runtime. -/
def effective (behavior ceiling : Surface) (runtime : Avail) : Surface :=
  (behavior.meet ceiling).meet runtime
```

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Meet.lean
git commit -m "feat(proofs): aggregate Surface.meet + effective composition"
```

---

## Task 6: Headline theorems — `effective ⊆ ceiling` and `⊆ behavior`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean`
- Modify: `crates/defra-agent/proofs/Proofs/DefraAgent.lean` (import)

**Interfaces:**
- Produces: theorems `effective_file_le_ceiling`, `effective_meta_le_ceiling`, `effective_mcp_subset_ceiling`, … and the two bundled statements `effective_subset_ceiling` / `effective_subset_behavior`. These are the properties the conformance cases (Task 9) witness.

- [ ] **Step 1: Write the per-category subset theorems (the failing test)**

Create `Proofs/ToolPolicy/Theorems.lean`. Representative bundle (write the full set, one per category — boolean categories use `Bool` `&&` monotonicity, endpoint categories use Task 4 lemmas, file/bash use rank lemmas):

```lean
import Proofs.ToolPolicy.Meet

namespace ToolPolicy

/-- Booleans: `(a && b) = true → a = true`. -/
theorem and_le_left {a b : Bool} (h : (a && b) = true) : a = true := by
  simpa using (Bool.and_eq_true.mp h).1

theorem and_le_right {a b : Bool} (h : (a && b) = true) : b = true := by
  simpa using (Bool.and_eq_true.mp h).2

variable (behavior ceiling : Surface) (runtime : Avail)

/-- File capability of the effective surface never exceeds the ceiling. -/
theorem effective_file_le_ceiling :
    (effective behavior ceiling runtime).file.rank ≤ ceiling.file.rank := by
  unfold effective Surface.meet
  exact le_trans (FileCap.meet_rank_le_left _ _) (FileCap.meet_rank_le_right _ _)

/-- MCP service scope of the effective surface is within the ceiling. -/
theorem effective_mcp_subset_ceiling (x : ToolId) :
    (effective behavior ceiling runtime).mcpServices.permits x →
      ceiling.mcpServices.permits x := by
  unfold effective Surface.meet
  intro h
  exact EndpointScope.meet_permits_left _ _ _
    (EndpointScope.meet_permits_right _ _ _ h)

/-- meta capability within ceiling. -/
theorem effective_meta_le_ceiling :
    (effective behavior ceiling runtime).meta = true → ceiling.meta = true := by
  unfold effective Surface.meet
  intro h
  exact and_le_right (and_le_left h)
```

Repeat the boolean pattern for `defraQuery, memory, sessionHistory, contextBudget, spawn, steering, background, orchestration, crossDeployment, skills`; the endpoint pattern for `cliTools, defraCollections, subagentTargets, backgroundTools, writeTools`; and a bash field lemma (e.g. `execRank (effective …).bash.mode ≤ execRank ceiling.bash.mode`).

- [ ] **Step 2: Add the `⊆ behavior` mirror + a structure-level bundle**

Append a parallel set proving `… → behavior.…` (use `meet_permits_left` where Task 6 used `_right`, and the symmetric rank lemma). Then a doc-comment bundle theorem name `effective_subset_ceiling` as a conjunction over the representative categories so the headline is one referenceable name. Keep each conjunct pointing at the per-category lemma.

- [ ] **Step 3: Import + build (this is the proof gate)**

Edit `Proofs/DefraAgent.lean`: `import Proofs.ToolPolicy.Theorems`

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero `sorry`. Iterate proofs here — the boolean lemmas may need `Bool.and_eq_true`; the rank `le_trans` may need the `_left`/`_right` order swapped depending on which operand is the ceiling (ceiling is the *second* arg of the inner `meet`, runtime the outer — verify the nesting matches `effective`).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean crates/defra-agent/proofs/Proofs/DefraAgent.lean
git commit -m "feat(proofs): effective ⊆ ceiling and ⊆ behavior for all tool categories"
```

---

## Task 7: meet algebra — idempotent, commutative, lower-bound completeness

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean`

**Interfaces:**
- Produces: `EndpointScope.meet_idem`, `EndpointScope.meet_comm`, `FileCap.meet_idem`, `FileCap.meet_comm`, `Surface.meet_idem`. These prove the meet is a genuine semilattice operation (so "secure-minimal ⊓ wide-open = secure-minimal" and order-independence hold).

- [ ] **Step 1: Write the algebra lemmas**

Append to `Theorems.lean`:

```lean
@[simp] theorem EndpointScope.meet_idem (a : EndpointScope α) : a.meet a = a := by
  cases a <;> simp [EndpointScope.meet]

theorem EndpointScope.meet_comm (a b : EndpointScope α) : a.meet b = b.meet a := by
  cases a <;> cases b <;> simp [EndpointScope.meet, Finset.inter_comm]

@[simp] theorem FileCap.meet_idem (a : FileCap) : a.meet a = a := by
  simp [FileCap.meet]

theorem FileCap.meet_comm (a b : FileCap) : a.meet b = b.meet a := by
  unfold FileCap.meet
  by_cases h : a.rank ≤ b.rank <;> by_cases h2 : b.rank ≤ a.rank <;>
    simp_all <;> omega_nat <;> cases a <;> cases b <;> simp_all [FileCap.rank]
```

> `FileCap.meet_comm` only holds up to rank-tie resolution; if ranks are equal the `if` picks `a` on one side and `b` on the other. Since distinct `FileCap` constructors have distinct ranks (0/1/2), equal ranks ⇒ equal constructors, so it holds — but the proof needs the injectivity of `rank`. If the one-liner stalls, prove a helper `FileCap.rank_inj : a.rank = b.rank → a = b` by `cases a <;> cases b <;> simp [FileCap.rank]` and use it.

- [ ] **Step 2: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Theorems.lean
git commit -m "feat(proofs): meet algebra (idempotent/commutative) for ToolPolicy atoms"
```

---

## Task 8: `Cases.lean` — finite witness rows + JSON-serializable shape

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolPolicy/Cases.lean`
- Modify: `crates/defra-agent/proofs/Proofs/DefraAgent.lean` (import)

**Interfaces:**
- Produces: `ToolPolicy.Case` structure (`name : String`, plus the evaluated witness fields a Rust test asserts) and `ToolPolicy.cases : List Case`. Consumed by Task 10 (JSON) and the Rust mirror (Task 11). **Field names here are the JSON contract — they must match the Rust struct in Task 11 exactly.**

- [ ] **Step 1: Define the case shape + witnesses**

Create `Proofs/ToolPolicy/Cases.lean`:

```lean
import Proofs.ToolPolicy.Meet

namespace ToolPolicy

/-- A finite conformance witness: a (behavior, ceiling, runtime) triple and
    the evaluated effective values a Rust test re-derives. Endpoint fields
    are rendered as the explicit allow/deny of a probe element. -/
structure Case where
  name                   : String
  /-- effective file rank (0/1/2) and the ceiling rank it must not exceed. -/
  effectiveFileRank      : Nat
  ceilingFileRank        : Nat
  /-- effective boolean for meta + the ceiling meta. -/
  effectiveMeta          : Bool
  ceilingMeta            : Bool
  /-- a probe MCP service id, and whether the effective surface permits it. -/
  mcpProbe               : String
  effectivePermitsMcp    : Bool
  ceilingPermitsMcp      : Bool
  /-- bash effective allowed-prefix scope is `all` (empty input) marker. -/
  bashAllowedIsAll       : Bool
  deriving Repr

private def probe : String := "svc-a"

private def secureMinimal : Surface :=
  { file := .off, bash := CommandPolicy.Policy.mk .readOnly [] [] .disabled []
  , meta := false, defraQuery := false, memory := false, sessionHistory := false
  , contextBudget := false, spawn := false, steering := false, background := false
  , orchestration := false, crossDeployment := false, skills := false
  , cliTools := .none, mcpServices := .none, defraCollections := .none
  , subagentTargets := .none, backgroundTools := .none, writeTools := .none }

private def wideOpen : Surface :=
  { file := .readWrite, bash := CommandPolicy.Policy.mk .unrestricted [] [] .enabled []
  , meta := true, defraQuery := true, memory := true, sessionHistory := true
  , contextBudget := true, spawn := true, steering := true, background := true
  , orchestration := true, crossDeployment := true, skills := true
  , cliTools := .all, mcpServices := .all, defraCollections := .all
  , subagentTargets := .all, backgroundTools := .all, writeTools := .all }

private def ceilingMcpOnly : Surface :=
  { wideOpen with mcpServices := .only {probe}, meta := true, file := .readOnly }

/-- Runtime where the probe service is offline (stale/unreachable -> not
    in the online set). -/
private def runtimeNoMcp : Avail :=
  { wideOpen with mcpServices := .none }

def mkCase (name : String) (behavior ceiling : Surface) (runtime : Avail) : Case :=
  let e := effective behavior ceiling runtime
  { name := name
  , effectiveFileRank := e.file.rank
  , ceilingFileRank := ceiling.file.rank
  , effectiveMeta := e.meta
  , ceilingMeta := ceiling.meta
  , mcpProbe := probe
  , effectivePermitsMcp := decide (e.mcpServices.permits probe)
  , ceilingPermitsMcp := decide (ceiling.mcpServices.permits probe)
  , bashAllowedIsAll :=
      match allowedScope e.bash.allowedArgvPrefixes with | .all => true | _ => false }

def cases : List Case :=
  [ mkCase "wide_open_behavior_clamped_by_secure_ceiling" wideOpen secureMinimal wideOpen
  , mkCase "ceiling_mcp_only_clamps_wide_open_behavior" wideOpen ceilingMcpOnly wideOpen
  , mkCase "runtime_offline_mcp_drops_permitted_service" wideOpen wideOpen runtimeNoMcp
  , mkCase "bash_empty_allowed_prefixes_is_all" wideOpen wideOpen wideOpen ]

end ToolPolicy
```

> `EndpointScope.permits` needs `Decidable` for `decide`; `x ∈ s` on `Finset` is decidable, `True`/`False` are decidable, so add `deriving DecidableEq`-free `instance : Decidable (EndpointScope.permits sc x)` if `decide` complains — implement by `cases sc` returning the matching `Decidable` instance. Confirm `CommandPolicy.Policy.mk` field order against `Types.lean` (mode, allowedArgvPrefixes, forbiddenArgvPrefixes, networkMode, readOnlyAllowlist).

- [ ] **Step 2: Import + build**

Edit `Proofs/DefraAgent.lean`: `import Proofs.ToolPolicy.Cases`

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Cases.lean crates/defra-agent/proofs/Proofs/DefraAgent.lean
git commit -m "feat(proofs): ToolPolicy conformance witness cases"
```

---

## Task 9: Emit `ToolPolicy` cases into the contract JSON

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Possibly modify: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean`

**Interfaces:**
- Consumes: `ToolPolicy.cases`.
- Produces: a `"toolPolicyCases"` array inside the contract JSON emitted by `Contracts.main` between the `---BEGIN DEFRA LEAN CONTRACT JSON---` markers.

- [ ] **Step 1: Add a `toJson` for `ToolPolicy.Case`**

In `Proofs/ToolPolicy/Cases.lean`, append a JSON serializer mirroring the existing idiom (see `ContractTypes.lean:131-166` `toJson` style — manual string building, booleans as `true`/`false`, ints bare, strings quoted via the existing `jsonString`/escape helper used there):

```lean
def Case.toJson (c : Case) : String :=
  "{" ++
  "\"name\":\"" ++ c.name ++ "\"," ++
  "\"effectiveFileRank\":" ++ toString c.effectiveFileRank ++ "," ++
  "\"ceilingFileRank\":" ++ toString c.ceilingFileRank ++ "," ++
  "\"effectiveMeta\":" ++ (if c.effectiveMeta then "true" else "false") ++ "," ++
  "\"ceilingMeta\":" ++ (if c.ceilingMeta then "true" else "false") ++ "," ++
  "\"mcpProbe\":\"" ++ c.mcpProbe ++ "\"," ++
  "\"effectivePermitsMcp\":" ++ (if c.effectivePermitsMcp then "true" else "false") ++ "," ++
  "\"ceilingPermitsMcp\":" ++ (if c.ceilingPermitsMcp then "true" else "false") ++ "," ++
  "\"bashAllowedIsAll\":" ++ (if c.bashAllowedIsAll then "true" else "false") ++
  "}"
```

> Reuse the exact array-join + escaping helper that `Contracts/Json.lean` already uses (it has a `jsonArray`/`jsonString` — match it rather than hand-rolling quoting, so escaping stays consistent). Case names here are ASCII-safe so quoting is trivial.

- [ ] **Step 2: Fold the array into `snapshotJson`**

In `Contracts/Json.lean`, locate where `snapshotJson` assembles the top-level object (the other `…Cases` arrays). Add a key:

```lean
  ++ ",\"toolPolicyCases\":" ++ jsonArray (ToolPolicy.cases.map ToolPolicy.Case.toJson)
```

Add `import Proofs.ToolPolicy.Cases` at the top of `Contracts/Json.lean` if not transitively imported.

- [ ] **Step 3: Build + run the emitter to eyeball the JSON**

Run: `cd crates/defra-agent/proofs && lake build`
Then: `lake env lean --run Proofs/Conformance/Contracts.lean | sed -n '/BEGIN DEFRA LEAN CONTRACT JSON/,/END DEFRA LEAN CONTRACT JSON/p' | grep -o '"toolPolicyCases":\[.*\]' | head -c 400`
Expected: a JSON array with the four named cases.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolPolicy/Cases.lean crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean
git commit -m "feat(proofs): emit ToolPolicy cases into the conformance contract JSON"
```

---

## Task 10: Rust mirror — `LeanToolPolicyCase` + accessor

**Files:**
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`

**Interfaces:**
- Consumes: the `toolPolicyCases` JSON array.
- Produces: `pub(crate) struct LeanToolPolicyCase { name, effective_file_rank: u8, ceiling_file_rank: u8, effective_meta: bool, ceiling_meta: bool, mcp_probe: String, effective_permits_mcp: bool, ceiling_permits_mcp: bool, bash_allowed_is_all: bool }` and `pub(crate) fn lean_tool_policy_case(name: &str) -> &'static LeanToolPolicyCase`.

- [ ] **Step 1: Add the struct + snapshot field**

In `lean_vocab_test.rs`, find the `LeanContractSnapshot` struct (the `serde`-derived type populated by `load_lean_contract_snapshot`) and add a field matching the JSON key:

```rust
#[serde(default)]
pub(crate) tool_policy_cases: Vec<LeanToolPolicyCase>,
```

Add the case struct near the other `Lean*Case` types (match the existing `#[derive(... Deserialize)]` + `#[serde(rename_all = "camelCase")]` convention used by neighbors):

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeanToolPolicyCase {
    pub name: String,
    pub effective_file_rank: u8,
    pub ceiling_file_rank: u8,
    pub effective_meta: bool,
    pub ceiling_meta: bool,
    pub mcp_probe: String,
    pub effective_permits_mcp: bool,
    pub ceiling_permits_mcp: bool,
    pub bash_allowed_is_all: bool,
}
```

- [ ] **Step 2: Add the accessor (mirror `lean_apply_reconcile_case` at line ~327)**

```rust
pub(crate) fn lean_tool_policy_cases() -> &'static [LeanToolPolicyCase] {
    &lean_contract_snapshot().tool_policy_cases
}

pub(crate) fn lean_tool_policy_case(name: &str) -> &'static LeanToolPolicyCase {
    lean_tool_policy_cases()
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("missing lean tool-policy case: {name}"))
}
```

- [ ] **Step 3: Build the test target to confirm it compiles + deserializes**

Run: `cargo test -p defra-agent --no-run`
Expected: compiles. (Deserialization is exercised in Task 11.)

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/lean_vocab_test.rs
git commit -m "test(conformance): mirror ToolPolicy lean cases into lean_vocab_test"
```

---

## Task 11: The fenced conformance test

**Files:**
- Create: `crates/defra-agent/tests/conformance/tool_policy.rs`
- Modify: `crates/defra-agent/tests/conformance.rs`

**Interfaces:**
- Consumes: `lean_tool_policy_case`.
- Produces: `#[test] fn generated_tool_policy_cases_match_lean_composition()`.

- [ ] **Step 1: Write the conformance test (mirror `command_policy.rs`)**

Create `tests/conformance/tool_policy.rs`:

```rust
//! ToolPolicy conformance home: generated composition cases asserting the
//! effective tool surface is within ceiling + behavior for every category.

use super::*;
use crate::lean_vocab_test::lean_tool_policy_case;

#[test]
fn generated_tool_policy_cases_match_lean_composition() {
    // wide-open behavior under a secure-minimal ceiling collapses to secure.
    let c = lean_tool_policy_case("wide_open_behavior_clamped_by_secure_ceiling");
    assert!(c.effective_file_rank <= c.ceiling_file_rank);
    assert!(!c.effective_meta, "secure ceiling must force meta off");
    assert_eq!(c.effective_meta, c.ceiling_meta && true);

    // ceiling that allows only one MCP service clamps an all-permitting behavior.
    let c = lean_tool_policy_case("ceiling_mcp_only_clamps_wide_open_behavior");
    assert_eq!(c.effective_permits_mcp, c.ceiling_permits_mcp);
    assert!(c.ceiling_permits_mcp, "probe is the single allowed service");

    // runtime offline drops a service both behavior and ceiling permit.
    let c = lean_tool_policy_case("runtime_offline_mcp_drops_permitted_service");
    assert!(!c.effective_permits_mcp, "offline service must be dropped");

    // empty allowed-argv-prefixes decodes as all (no gate), not deny-all.
    let c = lean_tool_policy_case("bash_empty_allowed_prefixes_is_all");
    assert!(c.bash_allowed_is_all);

    // the universal law across every emitted case.
    for case in crate::lean_vocab_test::lean_tool_policy_cases() {
        assert!(
            case.effective_file_rank <= case.ceiling_file_rank,
            "case {}: effective file rank exceeds ceiling",
            case.name
        );
        if case.effective_permits_mcp {
            assert!(
                case.ceiling_permits_mcp,
                "case {}: effective permits an MCP service the ceiling forbids",
                case.name
            );
        }
    }
}
```

- [ ] **Step 2: Register the module + wrapper in `conformance.rs`**

Add with the other `#[path = …] mod …;` lines:

```rust
#[path = "conformance/tool_policy.rs"]
mod tool_policy;
```

Add a wrapper `#[test]` alongside the others:

```rust
#[test]
fn generated_tool_policy_cases_match_lean_composition() {
    tool_policy::generated_tool_policy_cases_match_lean_composition();
}
```

- [ ] **Step 3: Run the test (it should now pass against the emitted JSON)**

Run: `cargo test -p defra-agent --test conformance generated_tool_policy_cases_match_lean_composition -- --nocapture`
Expected: PASS. If it fails on a missing snapshot field, confirm Task 9's JSON key is `toolPolicyCases` and Task 10's serde rename is `camelCase`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/conformance/tool_policy.rs crates/defra-agent/tests/conformance.rs
git commit -m "test(conformance): ToolPolicy composition fenced against lean cases"
```

---

## Task 12: Close the structure fence + coverage ledger

**Files:**
- Modify: `crates/defra-agent/tests/conformance/structure.rs`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`

**Interfaces:**
- Consumes: the test name from Task 11.
- Produces: green `every_lean_model_has_a_declared_conformance_home` + resolved consumer ledger.

- [ ] **Step 1: Declare the model home**

In `structure.rs` `model_homes()`, add (alphabetical with neighbors):

```rust
("ToolPolicy", Module("conformance/tool_policy.rs")),
```

- [ ] **Step 2: Register the coverage-ledger entry (Lean)**

In `CoverageLedger.lean`, add to the appropriate list (state-machine/vocabulary section, mirroring the `CommandPolicy` entry):

```lean
  , tagged (consumerCoverage
      "state_machine"
      "ToolPolicy"
      "conformance::generated_tool_policy_cases_match_lean_composition")
      "tool-policy" [Surface.agentFacing, Surface.operatorUi]
```

- [ ] **Step 3: Register the Rust consumer**

In `conformance_consumers.rs` `registered_conformance_consumers()`, add:

```rust
ConformanceConsumer::RustTest {
    id: "conformance::generated_tool_policy_cases_match_lean_composition",
    package: "defra-agent",
    source_path: "crates/defra-agent/tests/conformance.rs",
    module_path: "conformance",
    function: "generated_tool_policy_cases_match_lean_composition",
},
```

- [ ] **Step 4: Build proofs (ledger is Lean-checked) + run the fence tests**

Run: `cd crates/defra-agent/proofs && lake build`
Then: `cargo test -p defra-agent --test conformance every_lean_model_has_a_declared_conformance_home`
And: `cargo test -p defra-agent --test conformance -- consumer` (run the consumer-resolution test — find its exact name with `cargo test -p defra-agent --test conformance -- --list | grep -i consumer`)
Expected: all PASS. The structure fence fails loudly if `ToolPolicy` lacks a home; the consumer test fails if the id doesn't resolve to a real test fn.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/conformance/structure.rs crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean crates/defra-agent/tests/support/conformance_consumers.rs
git commit -m "test(conformance): register ToolPolicy in structure fence + coverage ledger"
```

---

## Task 13: Record the MCP-health-strictness boundary + full-suite gate

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean`

**Interfaces:**
- Produces: a documented boundary note that runtime availability (online+healthy MCP set) is an *input* to `effective`, and that the strict-`Healthy`-only-vs-`not-Unreachable` decision is deferred to SP1-Rust (where `enforce_health_gate` lives). The Lean model is sound for either: it composes whatever availability set Rust supplies.

- [ ] **Step 1: Add the boundary entry**

In `Boundaries.lean`, mirror the existing entry style to add a `ToolPolicy.runtimeAvailability` boundary: the model takes the availability `Surface` as given; the mapping from `ServiceHealthMap` status (Healthy/Stale/Unreachable) to the online set is a runtime concern conformance-checked in SP1-Rust, not proven here. Reference `meta_tools/shared.rs:366`.

- [ ] **Step 2: Build proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean.

- [ ] **Step 3: Full-suite gate (the real acceptance check)**

Run: `cargo test -p defra-agent`
Expected: the entire package suite passes, including the new conformance test, the structure fence, and the consumer ledger. Capture the summary line.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean
git commit -m "docs(proofs): record ToolPolicy runtime-availability boundary (health strictness -> SP1-Rust)"
```

---

## Self-Review (completed during authoring)

**Spec coverage (against `2026-06-26-unified-tool-policy-design.md`):**
- §3.1 `Capability` → Task 3 (`FileCap`) + boolean fields (Task 2/6). ✔
- §3.1 `BashPolicy` product meet (empty-allowed=all) → Task 4. ✔
- §3.1 `EndpointScope` none/only/all + keyed meet → Tasks 1, 4. ✔ (Keyed-*value* meet for `write_tools` collection/field narrowing is structured-value detail deferred to SP1-Rust decode; the scope skeleton + subset law is proven here. Noted as the one intentional SP1-Lean/SP1-Rust seam.)
- §3.3 `effective = behavior ⊓ ceiling ⊓ runtime` + `effective ⊆ ceiling`/`⊆ behavior` → Tasks 5, 6. ✔
- §3.3 runtime availability as a precomputed input → Tasks 2, 8, 13. ✔
- §4 meet algebra (commutative/idempotent/lower-bound) → Tasks 6, 7. ✔
- §4 conformance mirror → Tasks 8–12. ✔
- §5 context_budget *gate* category-complete → Task 2 (`contextBudget` field) + Task 6 lemma. ✔
- Pre-flight #2 (bash empty=all) → Task 4. #3 (health strictness) → Task 13. #4 (skills/load_skill category) → Task 2 (`skills` field) + Task 6. #1 (agent_did) → out of scope (SP1-Rust), stated in Global Constraints. ✔

**Deferred to SP1-Rust (NOT a gap — the Lean/Rust seam):** retyped `ToolSelection`, the Rust `ToolCeiling` expansion, decode/version/backfill, the `from_selection_*` rewrite, `ToolSurfaceExplanation` rework, `load_skill`/`context_budget` runtime wiring, protocol/desktop parity, presets. SP1-Rust mirrors *this* model.

**Type consistency:** `Surface` field names (Task 2) are used verbatim in Tasks 5, 6, 8. `Case` field names (Task 8) match `LeanToolPolicyCase` (Task 10) under `camelCase`↔`snake_case` serde. Test name `generated_tool_policy_cases_match_lean_composition` is identical in Tasks 11 and 12.

**Placeholder scan:** no TBD/TODO; every code step shows code; proof steps state the strategy + the fallback tactic when the one-liner may stall (the realistic Lean iteration loop).
