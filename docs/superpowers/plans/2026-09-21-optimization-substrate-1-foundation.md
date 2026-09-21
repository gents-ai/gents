# Optimization Substrate, Plan 1 of 3: Foundation Implementation Plan

> **Status after the 2026-09-21 redesign: execute this plan as "Track 0" only.** `publishIf` and the
> Rust compare-and-set are unchanged by the redesign and can start now. Everything about
> `Proofs/Optimization.lean` and the `optimization_cases` conformance family is superseded by
> `2026-09-21-optimization-policy-and-lean.md`. Follow the Track 0 execution guide below; where it
> conflicts with a task's text, the guide wins.

## Track 0 execution guide

Three PRs, same branch names and bases as written below.

**PR 1 (`optimization/01-lean`): `publishIf` only.**
- Execute **Task 1** in full.
- **Skip Task 2** entirely. Do not create `Proofs/Optimization.lean` and do not add its import.
- Execute **Task 3** with these changes: skip Step 1 (the `Proofs/Optimization.lean` row). Execute
  Steps 2, 3 and 4. The commit message becomes `docs(proofs): map publishIf`.

**PR 2 (`optimization/02-conformance`): `publishIf` cases only.**
- **Task 4:** execute Step 1. **Skip Step 2** (do not create `Proofs/Conformance/Optimization.lean`).
  In Step 3, do not add the `import Proofs.Conformance.Optimization` line, and insert only these two
  lines after the `apply_reconcile_cases` splice:
  ```lean
      ++ "\"publish_if_cases\":"
        ++ ApplyReconcile.ContractCases.publishIfCasesJson ++ ","
  ```
  In Step 4, add only the `publish_if_cases` ledger entry, and do not add the `"optimization"`
  `featureSurfaceRequirements` entry. Execute Step 5. The Step 5 contingency does not apply. The
  commit message becomes `proofs(conformance): emit publishIf cases`.
- **Task 5:** in Step 1, create `crates/gents/src/lean_vocab_test/publish_if.rs` instead of
  `optimization.rs`, containing only the `use` lines, `LeanPublishIfExpectation` and
  `LeanPublishIfCase`. In Step 2, the module is `publish_if` (`#[path = "publish_if.rs"] mod
  publish_if;` and `pub(crate) use publish_if::*;`), add only the `publish_if_cases` snapshot field,
  and add only the `lean_publish_if_cases` accessor. In Step 3, add only the
  `publish_if_cases.len() == 7` assertion and only the `publish_if_cases` emitted-domain block.
  Execute Steps 4 and 5. The commit message becomes `test(conformance): deserialize publishIf cases`.

**PR 3 (`optimization/03-cas`): unchanged.** Execute **Task 6** and **Task 7** in full.

The PR descriptions drop every mention of the optimization model. PR 1's owners line becomes
`ApplyReconcile/Publication` only.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Lean model, the generated conformance cases, and the Rust digest-precondition (compare-and-set) that the optimization substrate is built on.

**Architecture:** `publishIf` is a precondition on the one existing `publish` in `Proofs/ApplyReconcile/Publication.lean`. A new `Proofs/Optimization.lean` models outcome imputation, the three-gate promotion decision, and the append-only job journal. Lean emits cases into the existing contract snapshot; Rust unit tests consume them. `DesiredStateApplyPlan` gains an `expected` digest set that `apply_desired_state_plan` checks inside the caller's transaction before any write.

**Tech Stack:** Lean 4 (v4.18.0) with Mathlib, Rust 1.97.1, DefraDB embedded node, `gents-lean-contract` bridge, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-substrate-design.md`

**Plans in this series:** 1 Foundation (this file, PRs 1 to 3) · 2 Core and driver (PRs 4 and 5) · 3 Promotion and live demo (PRs 6 and 7).

## Global Constraints

- Order is Lean, then conformance, then Rust. Each PR targets its parent branch.
- Lean proofs contain no `sorry`. CI greps for it. If a tactic fails, fix the tactic; never weaken a theorem statement without recording why in the PR description.
- Use `tracing`, never `println!`, in `crates/gents/src`.
- Escape every interpolated GraphQL string with `graphql::escape_graphql_string()`.
- Never emit `[]` in a DefraDB mutation; use `null` for an empty nillable list.
- Typed errors in `config_client` are marker structs wrapped with `anyhow::Error::new` and recovered with `downcast_ref` (see `config_client/retry.rs`). Do not add `thiserror` enums there.
- Do not touch `self_config` `cleanup`. Its combined plan digest is already a compare-and-set inside one transaction, and its one-digest contract does not fit per-document expectations.
- Before each push: `cargo test -p gents` and `cargo check --workspace --all-targets`. For Lean changes also `lake build` in `crates/gents/proofs`.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`. Git has no global identity on this machine.
- Create each PR branch with `make worktree BRANCH=<branch> BASE=<parent>`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/gents/proofs/Proofs/ApplyReconcile/Publication.lean` | modify | add `expectationsHold`, `publishIf`, six theorems |
| `crates/gents/proofs/Proofs/Optimization.lean` | create | imputation, tallies, gates, decision, journal |
| `crates/gents/proofs/Proofs.lean` | modify | register `Proofs.Optimization` |
| `crates/gents/proofs/README.md` | modify | proof-map rows; fix the stale `ApplyReconcile` submodule row |
| `crates/gents/proofs/Proofs/ApplyReconcile/ContractCases.lean` | modify | emit `publishIfCasesJson` |
| `crates/gents/proofs/Proofs/Conformance/Optimization.lean` | create | emit `optimizationCasesJson` |
| `crates/gents/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` | modify | splice two new snapshot keys |
| `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean` | modify | two ledger entries |
| `crates/gents/src/lean_vocab_test/optimization.rs` | create | serde structs for the two new case families |
| `crates/gents/src/lean_vocab_test/support.rs` | modify | snapshot fields and accessors |
| `crates/gents/tests/conformance/coverage.rs` | modify | count pins and emitted-domain ledger |
| `crates/gents/src/config_client/desired_state.rs` | modify | `expected`, `StaleExpectation`, check in apply |
| `crates/gents/src/config_client/desired_state/tests.rs` | modify | compare-and-set tests and the `publishIf` consumer |
| `crates/gents/src/config_client/mod.rs` | modify | re-export the new public items |

---

## PR 1: Lean model

Branch: `optimization/01-lean`, base `main`.

### Task 1: `publishIf` in `Publication.lean`

**Files:**
- Modify: `crates/gents/proofs/Proofs/ApplyReconcile/Publication.lean` (append before the final `end ApplyReconcile`)

**Interfaces:**
- Consumes: `publish`, `LiveState`, `Manifest`, `DocRef`, `DesiredFields`, and the theorems `publication_preserves_observations`, `publication_all_or_nothing`, `publication_idempotent`, all in this file or `Manifest.lean`.
- Produces: `ApplyReconcile.expectationsHold : LiveState → List DocRef → (DocRef → Option DesiredFields) → Bool` and `ApplyReconcile.publishIf : LiveState → List DocRef → (DocRef → Option DesiredFields) → Manifest → LiveState`.

- [ ] **Step 1: Add the definitions and theorems**

Insert immediately before `end ApplyReconcile`:

```lean
/-- Digest-guarded publication. `scope` lists the documents whose desired fields
must still equal `expected`; a digest stands in for field equality at the Rust
refinement boundary. This is a precondition on the one publication model, not a
second one: an empty scope is exactly `publish`. -/
def expectationsHold (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) : Bool :=
  decide (∀ d ∈ scope, old.desired d = expected d)

def publishIf (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) : LiveState :=
  if expectationsHold old scope expected then publish old candidate else old

theorem publishIf_stale_unchanged (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest)
    (h : expectationsHold old scope expected = false) :
    publishIf old scope expected candidate = old := by
  simp [publishIf, h]

theorem publishIf_match_eq_publish (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest)
    (h : expectationsHold old scope expected = true) :
    publishIf old scope expected candidate = publish old candidate := by
  simp [publishIf, h]

/-- Existing callers carry no expectations and are unaffected. -/
theorem publishIf_empty_scope (old : LiveState)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    publishIf old [] expected candidate = publish old candidate := by
  simp [publishIf, expectationsHold]

theorem publishIf_preserves_observations (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    (publishIf old scope expected candidate).live = old.live := by
  unfold publishIf
  split
  · exact publication_preserves_observations old candidate
  · rfl

theorem publishIf_all_or_nothing (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    (publishIf old scope expected candidate).desired = candidate.docs
      ∨ publishIf old scope expected candidate = old := by
  unfold publishIf
  split
  · exact publication_all_or_nothing old candidate
  · exact Or.inr rfl

/-- A replayed guarded publication converges: after success the expectation
either still holds (then `publish` is idempotent) or is stale (then nothing moves). -/
theorem publishIf_idempotent (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    publishIf (publishIf old scope expected candidate) scope expected candidate
      = publishIf old scope expected candidate := by
  by_cases h : expectationsHold old scope expected = true
  · rw [publishIf_match_eq_publish old scope expected candidate h]
    unfold publishIf
    split
    · exact publication_idempotent old candidate
    · rfl
  · have hf : expectationsHold old scope expected = false := by simpa using h
    rw [publishIf_stale_unchanged old scope expected candidate hf]
    exact publishIf_stale_unchanged old scope expected candidate hf
```

- [ ] **Step 2: Build**

Run: `cd crates/gents/proofs && lake build Proofs.ApplyReconcile.Publication`
Expected: success with no errors and no `sorry` warnings. The first build downloads and compiles Mathlib and can take a long time; use `lake exe cache get` first if the project supports it.
If a tactic fails, repair the tactic (for example replace `simp [publishIf, h]` with `unfold publishIf; rw [h]; rfl`). Do not change a statement.

- [ ] **Step 3: Commit**

```bash
git add crates/gents/proofs/Proofs/ApplyReconcile/Publication.lean
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(apply-reconcile): digest-guarded publishIf precondition (#1455)"
```

### Task 2: `Proofs/Optimization.lean`

**Files:**
- Create: `crates/gents/proofs/Proofs/Optimization.lean`
- Modify: `crates/gents/proofs/Proofs.lean` (append one import)

**Interfaces:**
- Produces, all in `namespace Optimization`: `OutcomeClass`, `Arm`, `impute`, `Tally`, `tally`, `Params`, `Evidence`, `sufficient`, `noCaseRegression`, `Mode`, `RejectReason`, `Decision`, `decideGates`, `Entry`, `Journal`, `appendIf`, `roundsUsed`, `propose`. Plan 1 Task 4 and Plan 2 rely on these exact names.

Units: every fraction in `Params` is an integer number of basis points (1 bp = 0.01%). Rust uses the same units so the arithmetic is identical on both sides.

- [ ] **Step 1: Write the file**

```lean
import Mathlib.Data.List.Basic
import Mathlib.Tactic

/-! Optimization substrate (#1455).

Models the parts of the promotion decision that must stay outside model control:
worst-case imputation of unclassified trials, the two integer-arithmetic gates,
the ordered three-gate decision, and the append-only job journal.

Refinement boundaries: the improvement test (a one-sided Fisher exact test in
Rust) is an abstract `Bool` here; candidate construction, evaluation and the
proposer are outside the fence. Numeric antitonicity of the concrete gates is
checked by Rust property tests, not proved here. -/
namespace Optimization

inductive OutcomeClass where
  | pass | fail | notEvidence | unknown
  deriving DecidableEq, Repr

inductive Arm where
  | baseline | candidate
  deriving DecidableEq, Repr

/-- Evidence value of one trial. `none` means excluded. An unknown can never
help a candidate: it fails the candidate and passes the baseline. -/
def impute : Arm → OutcomeClass → Option Bool
  | _, .pass => some true
  | _, .fail => some false
  | _, .notEvidence => none
  | .candidate, .unknown => some false
  | .baseline, .unknown => some true

theorem candidate_unknown_never_passes : impute .candidate .unknown ≠ some true := by
  decide

theorem candidate_unknown_is_fail : impute .candidate .unknown = impute .candidate .fail :=
  rfl

theorem baseline_unknown_is_pass : impute .baseline .unknown = impute .baseline .pass :=
  rfl

theorem not_evidence_excluded (a : Arm) : impute a .notEvidence = none := by
  cases a <;> rfl

structure Tally where
  pass : Nat
  fail : Nat
  excluded : Nat
  deriving DecidableEq, Repr

namespace Tally
def evidence (t : Tally) : Nat := t.pass + t.fail
def total (t : Tally) : Nat := t.pass + t.fail + t.excluded
end Tally

def tallyStep (arm : Arm) (t : Tally) (o : OutcomeClass) : Tally :=
  match impute arm o with
  | some true => { t with pass := t.pass + 1 }
  | some false => { t with fail := t.fail + 1 }
  | none => { t with excluded := t.excluded + 1 }

def tally (arm : Arm) (outcomes : List OutcomeClass) : Tally :=
  outcomes.foldl (tallyStep arm) ⟨0, 0, 0⟩

/-- Replacing a candidate `pass` with any other class never raises the pass count. -/
theorem candidate_pass_is_best (t : Tally) (o : OutcomeClass) :
    (tallyStep .candidate t o).pass ≤ (tallyStep .candidate t .pass).pass := by
  cases o <;> simp [tallyStep, impute]

structure Params where
  minTrials : Nat
  maxNotEvidenceBp : Nat
  maxAsymmetryBp : Nat
  caseToleranceBp : Nat
  deriving DecidableEq, Repr

/-- Per-case `(baseline, candidate)` tallies. `casesMatch` is false when the two
arms did not report the same case ids. -/
structure Evidence where
  casesMatch : Bool
  cases : List (Tally × Tally)
  deriving Repr

def pool (ts : List Tally) : Tally :=
  ts.foldl (fun a t => ⟨a.pass + t.pass, a.fail + t.fail, a.excluded + t.excluded⟩) ⟨0, 0, 0⟩

def absDiff (a b : Nat) : Nat := (a - b) + (b - a)

/-- Gate 1. Same cases, the same number of trials per case in both arms, enough
evidence per case, bounded exclusions per arm, bounded exclusion asymmetry. -/
def sufficient (p : Params) (e : Evidence) : Bool :=
  let b := pool (e.cases.map Prod.fst)
  let c := pool (e.cases.map Prod.snd)
  e.casesMatch && !e.cases.isEmpty
    && e.cases.all (fun bc =>
        decide (bc.1.total = bc.2.total)
          && decide (p.minTrials ≤ bc.1.evidence)
          && decide (p.minTrials ≤ bc.2.evidence))
    && decide (b.excluded * 10000 ≤ p.maxNotEvidenceBp * b.total)
    && decide (c.excluded * 10000 ≤ p.maxNotEvidenceBp * c.total)
    && decide (absDiff (b.excluded * c.total) (c.excluded * b.total) * 10000
        ≤ p.maxAsymmetryBp * (b.total * c.total))

/-- Gate 2. Cross-multiplied `cand.pass/cand.evidence ≥ base.pass/base.evidence − tol`. -/
def noCaseRegression (p : Params) (e : Evidence) : Bool :=
  e.cases.all fun bc =>
    decide (bc.1.pass * bc.2.evidence * 10000
      ≤ bc.2.pass * bc.1.evidence * 10000 + p.caseToleranceBp * (bc.1.evidence * bc.2.evidence))

inductive Mode where
  | improve | confirm
  deriving DecidableEq, Repr

inductive RejectReason where
  | caseRegression | noImprovement
  deriving DecidableEq, Repr

inductive Decision where
  | accept
  | reject (reason : RejectReason)
  | inconclusive
  deriving DecidableEq, Repr

/-- The first failing gate decides. `confirm` never consults the improvement test. -/
def decideGates (mode : Mode) (sufficient noCaseRegression improves : Bool) : Decision :=
  if !sufficient then .inconclusive
  else if !noCaseRegression then .reject .caseRegression
  else match mode with
    | .confirm => .accept
    | .improve => if improves then .accept else .reject .noImprovement

theorem improve_accept_iff (s r i : Bool) :
    decideGates .improve s r i = .accept ↔ s = true ∧ r = true ∧ i = true := by
  cases s <;> cases r <;> cases i <;> decide

theorem confirm_accept_iff (s r i : Bool) :
    decideGates .confirm s r i = .accept ↔ s = true ∧ r = true := by
  cases s <;> cases r <;> cases i <;> decide

theorem inconclusive_iff (mode : Mode) (s r i : Bool) :
    decideGates mode s r i = .inconclusive ↔ s = false := by
  cases mode <;> cases s <;> cases r <;> cases i <;> decide

/-- Weakening any gate never turns a non-accept into an accept. -/
theorem accept_antitone (mode : Mode) (s r i s' r' i' : Bool)
    (hs : s' = true → s = true) (hr : r' = true → r = true) (hi : i' = true → i = true)
    (h : decideGates mode s' r' i' = .accept) : decideGates mode s r i = .accept := by
  cases mode <;> cases s <;> cases r <;> cases i <;> cases s' <;> cases r' <;> cases i' <;>
    simp_all [decideGates]

/-! ## Job journal -/

inductive Entry where
  | frozen | evaluationStarted | evaluated | interrupted
  | proposed | structuralReject
  | decided (d : Decision)
  | budgetExhausted | finalized
  | promoted | promotionRefused
  deriving DecidableEq, Repr

abbrev Journal := List Entry

/-- Append guarded by the expected journal length (the Rust transaction check). -/
def appendIf (expectedLen : Nat) (j : Journal) (e : Entry) : Journal :=
  if j.length = expectedLen then j ++ [e] else j

theorem appendIf_prefix (n : Nat) (j : Journal) (e : Entry) : j <+: appendIf n j e := by
  unfold appendIf
  split
  · exact List.prefix_append j [e]
  · exact List.prefix_refl j

theorem appendIf_stale_unchanged (n : Nat) (j : Journal) (e : Entry) (h : j.length ≠ n) :
    appendIf n j e = j := by
  simp [appendIf, h]

theorem appendIf_match (j : Journal) (e : Entry) : appendIf j.length j e = j ++ [e] := by
  simp [appendIf]

def roundsUsed (j : Journal) : Nat := j.count .proposed

/-- A round may start only while rounds remain. -/
def propose (maxRounds : Nat) (j : Journal) : Journal :=
  if roundsUsed j < maxRounds then j ++ [.proposed] else j

theorem propose_bounded (maxRounds : Nat) (j : Journal) (h : roundsUsed j ≤ maxRounds) :
    roundsUsed (propose maxRounds j) ≤ maxRounds := by
  unfold propose
  split
  · rename_i hlt
    simp only [roundsUsed, List.count_append] at *
    simp
    omega
  · exact h

end Optimization
```

- [ ] **Step 2: Register the module**

Append to `crates/gents/proofs/Proofs.lean`:

```lean

import Proofs.Optimization
```

- [ ] **Step 3: Build**

Run: `cd crates/gents/proofs && lake build`
Expected: success. Likely repair points if a tactic fails: `propose_bounded` (try `simp [roundsUsed, List.count_append, List.count_cons] at *; omega`), and `accept_antitone` (try `revert hs hr hi h; cases mode <;> ... <;> decide`). Statements stay as written.

- [ ] **Step 4: Check for `sorry`**

Run: `grep -rn "sorry" crates/gents/proofs/Proofs/Optimization.lean crates/gents/proofs/Proofs/ApplyReconcile/Publication.lean`
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/proofs/Proofs/Optimization.lean crates/gents/proofs/Proofs.lean
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(optimization): imputation, promotion gates and job journal (#1455)"
```

### Task 3: Proof map

**Files:**
- Modify: `crates/gents/proofs/README.md`

- [ ] **Step 1: Add the file row**

In the `| File | Contents |` table, after the `Proofs/ApplyReconcile.lean` row, add:

```
| `Proofs/Optimization.lean` | Optimization substrate (#1455): worst-case outcome imputation, integer promotion gates, ordered three-gate decision, append-only length-guarded job journal with bounded rounds. The improvement test and candidate construction are refinement boundaries |
```

- [ ] **Step 2: Fix the stale submodule row**

In the `| Barrel | Submodules |` table, replace the `Proofs.ApplyReconcile` row with:

```
| `Proofs.ApplyReconcile` | `Collections`, `Manifest`, `Publication` (includes digest-guarded `publishIf`), `Convergence`, `ContractCases`, `RuntimeBridge` |
```

- [ ] **Step 3: Extend the Installation surface row**

In the `| Surface | Contract and retained owner |` table, append this sentence to the end of the Installation row's second cell:

```
 `publishIf` adds an expected-field precondition on the same publication; a stale expectation leaves state unchanged and an empty scope is `publish`.
```

- [ ] **Step 4: Commit and open PR 1**

```bash
git add crates/gents/proofs/README.md
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "docs(proofs): map publishIf and the optimization model"
```

PR description must state: baseline `main`, owners touched (`ApplyReconcile/Publication`, new `Optimization`), no deletions, validation `lake build`. Note that the `lean-proofs` CI job is skipped on non-tip stacked PRs, so paste the local `lake build` result.

---

## PR 2: Conformance emission

Branch: `optimization/02-conformance`, base `optimization/01-lean`.

`LeanContractSnapshot` is `#[serde(deny_unknown_fields)]`. Adding a snapshot key in Lean without the matching Rust field breaks `cargo test -p gents --test conformance`, so the Lean emission and the Rust structs land in the same PR. Consumers arrive with the Rust implementations (Task 7 here, and Plan 2), so both ledger entries start as `followUpCoverage`.

### Task 4: Emit the two case families from Lean

**Files:**
- Modify: `crates/gents/proofs/Proofs/ApplyReconcile/ContractCases.lean`
- Create: `crates/gents/proofs/Proofs/Conformance/Optimization.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean`

**Interfaces:**
- Consumes: private helpers `manifestOf`, `row`, `docRefJson`, `desiredJson` in `ContractCases.lean`; `jsonString`, `jsonArray` from `Proofs/Conformance/ContractTypes.lean`; everything from Task 1 and Task 2.
- Produces: snapshot keys `publish_if_cases` and `optimization_cases` with the JSON shapes below.

- [ ] **Step 1: `publishIf` cases**

In `ContractCases.lean`, insert before `end ApplyReconcile.ContractCases`:

```lean
private def boolJson (b : Bool) : String := if b then "true" else "false"

/-- `expected` rows: `content` is `null` when the document must be absent. -/
private def expectedJson (rows : List (DocRef × Option DesiredFields)) : String :=
  jsonArray (rows.map fun (d, f) =>
    "{\"ref\":" ++ docRefJson d ++ ",\"content\":"
      ++ (match f with | some x => jsonString x.content | none => "null") ++ "}")

private def expectedFn (rows : List (DocRef × Option DesiredFields)) :
    DocRef → Option DesiredFields :=
  fun d => (rows.find? fun r => r.1 = d).bind Prod.snd

private def ctx : DocRef := ⟨.agentContext, "context", "did:owner"⟩
private def tools : DocRef := ⟨.tools, "tools", "did:owner"⟩
private def absent : DocRef := ⟨.agentContext, "absent", "did:owner"⟩

private def priorRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v1", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def candidateRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v2", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def publishIfScenarios : List (String × List (DocRef × Option DesiredFields)) :=
  [("all_expectations_match",
      [(ctx, some { content := "prompt-v1", refs := [tools] }),
       (tools, some { content := "tools-v1", refs := [] })]),
   ("target_drifted", [(ctx, some { content := "prompt-v0", refs := [tools] })]),
   ("closure_document_drifted",
      [(ctx, some { content := "prompt-v1", refs := [tools] }),
       (tools, some { content := "tools-v0", refs := [] })]),
   ("expected_absent_but_present", [(ctx, none)]),
   ("expected_absent_and_absent", [(absent, none)]),
   ("expected_present_but_absent", [(absent, some { content := "x", refs := [] })]),
   ("empty_scope_is_publish", [])]

private def publishIfScenarioJson (name : String)
    (rows : List (DocRef × Option DesiredFields)) : String :=
  let old : LiveState := { desired := (manifestOf priorRows).docs, live := fun _ => none }
  let candidate := manifestOf candidateRows
  let scope := rows.map Prod.fst
  let after := publishIf old scope (expectedFn rows) candidate
  let keys : List DocRef := [ctx, tools, absent]
  "{\"name\":" ++ jsonString name
    ++ ",\"expected\":" ++ expectedJson rows
    ++ ",\"pre_desired\":" ++ desiredJson keys old.desired
    ++ ",\"candidate\":" ++ desiredJson keys candidate.docs
    ++ ",\"applied\":" ++ boolJson (expectationsHold old scope (expectedFn rows))
    ++ ",\"expected_after_desired\":" ++ desiredJson keys after.desired ++ "}"

def publishIfCasesJson : String :=
  jsonArray (publishIfScenarios.map fun (name, rows) => publishIfScenarioJson name rows)
```

- [ ] **Step 2: Optimization cases**

Create `crates/gents/proofs/Proofs/Conformance/Optimization.lean`:

```lean
import Proofs.Optimization
import Proofs.Conformance.ContractTypes

/-! Generated witnesses for the optimization promotion policy. Rust must
reproduce imputation, both integer gates and the ordered decision exactly. -/
namespace Conformance.Optimization

open Conformance.Contracts
open _root_.Optimization

private def boolJson (b : Bool) : String := if b then "true" else "false"

private def classJson : OutcomeClass → String
  | .pass => "\"pass\"" | .fail => "\"fail\""
  | .notEvidence => "\"not_evidence\"" | .unknown => "\"unknown\""

private def armJson : Arm → String
  | .baseline => "\"baseline\"" | .candidate => "\"candidate\""

private def modeJson : Mode → String
  | .improve => "\"improve\"" | .confirm => "\"confirm\""

private def decisionJson : Decision → String
  | .accept => "\"accept\""
  | .inconclusive => "\"inconclusive\""
  | .reject .caseRegression => "\"reject_case_regression\""
  | .reject .noImprovement => "\"reject_no_improvement\""

private def imputeJson (a : Arm) (o : OutcomeClass) : String :=
  "{\"arm\":" ++ armJson a ++ ",\"class\":" ++ classJson o ++ ",\"counts_as\":"
    ++ (match impute a o with
        | some true => "\"pass\"" | some false => "\"fail\"" | none => "\"excluded\"") ++ "}"

private def imputationRows : List String :=
  [Arm.baseline, Arm.candidate].flatMap fun a =>
    [OutcomeClass.pass, .fail, .notEvidence, .unknown].map (imputeJson a)

private def decisionRows : List String :=
  [Mode.improve, Mode.confirm].flatMap fun m =>
    [true, false].flatMap fun s => [true, false].flatMap fun r => [true, false].map fun i =>
      "{\"mode\":" ++ modeJson m ++ ",\"sufficient\":" ++ boolJson s
        ++ ",\"no_case_regression\":" ++ boolJson r ++ ",\"improves\":" ++ boolJson i
        ++ ",\"decision\":" ++ decisionJson (decideGates m s r i) ++ "}"

private def tallyJson (t : Tally) : String :=
  "{\"pass\":" ++ toString t.pass ++ ",\"fail\":" ++ toString t.fail
    ++ ",\"excluded\":" ++ toString t.excluded ++ "}"

private def params : Params :=
  { minTrials := 4, maxNotEvidenceBp := 2000, maxAsymmetryBp := 1000, caseToleranceBp := 1000 }

private def t (p f x : Nat) : Tally := ⟨p, f, x⟩

private def gateScenarios : List (String × Evidence) :=
  [("clean_improvement", ⟨true, [(t 5 5 0, t 8 2 0), (t 6 4 0, t 7 3 0)]⟩),
   ("cases_do_not_match", ⟨false, [(t 5 5 0, t 8 2 0)]⟩),
   ("no_cases", ⟨true, []⟩),
   ("unequal_trial_counts", ⟨true, [(t 5 5 0, t 8 1 0)]⟩),
   ("too_few_evidence_trials", ⟨true, [(t 2 1 7, t 8 2 0)]⟩),
   ("too_much_not_evidence", ⟨true, [(t 4 3 3, t 4 3 3)]⟩),
   ("asymmetric_exclusion", ⟨true, [(t 5 5 0, t 7 1 2), (t 5 5 0, t 7 1 2)]⟩),
   ("one_case_regresses", ⟨true, [(t 5 5 0, t 9 1 0), (t 8 2 0, t 5 5 0)]⟩),
   ("regression_within_tolerance", ⟨true, [(t 6 4 0, t 5 5 0)]⟩),
   ("regression_just_outside_tolerance", ⟨true, [(t 7 3 0, t 5 5 0)]⟩)]

private def gateRow (name : String) (e : Evidence) : String :=
  "{\"name\":" ++ jsonString name
    ++ ",\"cases_match\":" ++ boolJson e.casesMatch
    ++ ",\"cases\":" ++ jsonArray (e.cases.map fun bc =>
        "{\"baseline\":" ++ tallyJson bc.1 ++ ",\"candidate\":" ++ tallyJson bc.2 ++ "}")
    ++ ",\"sufficient\":" ++ boolJson (sufficient params e)
    ++ ",\"no_case_regression\":" ++ boolJson (noCaseRegression params e) ++ "}"

def optimizationCasesJson : String :=
  "{\"params\":{\"min_trials\":" ++ toString params.minTrials
    ++ ",\"max_not_evidence_bp\":" ++ toString params.maxNotEvidenceBp
    ++ ",\"max_asymmetry_bp\":" ++ toString params.maxAsymmetryBp
    ++ ",\"case_tolerance_bp\":" ++ toString params.caseToleranceBp ++ "}"
    ++ ",\"imputation\":" ++ jsonArray imputationRows
    ++ ",\"decisions\":" ++ jsonArray decisionRows
    ++ ",\"gates\":" ++ jsonArray (gateScenarios.map fun (n, e) => gateRow n e) ++ "}"

end Conformance.Optimization
```

- [ ] **Step 3: Splice both keys into the snapshot**

In `Snapshot.lean`, add `import Proofs.Conformance.Optimization` after the last existing `import`. Then, directly after the two lines

```lean
    ++ "\"apply_reconcile_cases\":"
      ++ ApplyReconcile.ContractCases.applyReconcileCasesJson ++ ","
```

insert:

```lean
    ++ "\"publish_if_cases\":"
      ++ ApplyReconcile.ContractCases.publishIfCasesJson ++ ","
    ++ "\"optimization_cases\":"
      ++ Conformance.Optimization.optimizationCasesJson ++ ","
```

- [ ] **Step 4: Ledger entries**

In `CoverageLedger.lean`, inside `caseCoverage`, directly after the `apply_reconcile_cases` entry, add:

```lean
  , tagged (followUpCoverage
      "publish_if_cases"
      "PublishIfCases"
      "Consumed by config_client::desired_state::tests once the Rust expected-digest precondition lands in the next stacked PR.")
      "apply-reconcile" [Surface.operatorCli]
  , tagged (followUpCoverage
      "optimization_cases"
      "OptimizationCases"
      "Consumed by optimization::policy::tests once the pure policy core lands (optimization substrate plan 2).")
      "optimization" [Surface.operatorCli]
```

Register the new feature name. In the same file, inside `featureSurfaceRequirements`, directly after the `"apply-reconcile"` entry, add:

```lean
  , { feature := "optimization"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#1455")]
    }
```

- [ ] **Step 5: Build**

Run: `cd crates/gents/proofs && lake build`
Expected: success.

Contingency for Task 5 Step 4: if the Rust feature-matrix test in `tests/conformance/coverage.rs` fails because the `optimization` feature has no consumer on its required surface yet, change the `optimization_cases` ledger entry's feature tag from `"optimization"` to `"apply-reconcile"`, delete the `featureSurfaceRequirements` entry above, and note in the PR description that Plan 2 Task 3 restores both when the policy consumer lands.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/proofs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(conformance): emit publishIf and optimization policy cases"
```

### Task 5: Rust snapshot structs

**Files:**
- Create: `crates/gents/src/lean_vocab_test/optimization.rs`
- Modify: `crates/gents/src/lean_vocab_test/support.rs`
- Modify: `crates/gents/tests/conformance/coverage.rs`

**Interfaces:**
- Consumes: `LeanApplyDocRef`, `LeanApplyDesiredRow` from `lean_vocab_test/triggers_runtime_apply.rs`.
- Produces: `lean_publish_if_cases() -> &'static [LeanPublishIfCase]` and `lean_optimization_cases() -> &'static LeanOptimizationCases`, with the field names below. Task 7 and Plan 2 use them.

- [ ] **Step 1: Write the structs**

Create `crates/gents/src/lean_vocab_test/optimization.rs`:

```rust
use serde::Deserialize;

use super::{LeanApplyDesiredRow, LeanApplyDocRef};

/// One expectation row of a guarded publication. `content: None` means the
/// document must be absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfExpectation {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: Option<String>,
}

/// `Publication.lean` `publishIf`: applied iff every expectation equals the
/// prior desired fields; otherwise the prior state is returned unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfCase {
    pub(crate) name: String,
    pub(crate) expected: Vec<LeanPublishIfExpectation>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    pub(crate) applied: bool,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationParams {
    pub(crate) min_trials: u64,
    pub(crate) max_not_evidence_bp: u64,
    pub(crate) max_asymmetry_bp: u64,
    pub(crate) case_tolerance_bp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanImputationRow {
    pub(crate) arm: String,
    pub(crate) class: String,
    pub(crate) counts_as: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanDecisionRow {
    pub(crate) mode: String,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
    pub(crate) improves: bool,
    pub(crate) decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTally {
    pub(crate) pass: u64,
    pub(crate) fail: u64,
    pub(crate) excluded: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCaseTallies {
    pub(crate) baseline: LeanTally,
    pub(crate) candidate: LeanTally,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanGateRow {
    pub(crate) name: String,
    pub(crate) cases_match: bool,
    pub(crate) cases: Vec<LeanCaseTallies>,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationCases {
    pub(crate) params: LeanOptimizationParams,
    pub(crate) imputation: Vec<LeanImputationRow>,
    pub(crate) decisions: Vec<LeanDecisionRow>,
    pub(crate) gates: Vec<LeanGateRow>,
}
```

- [ ] **Step 2: Wire into the snapshot**

`support.rs` declares each sibling file with a `#[path]` attribute, because `tests/conformance.rs` also includes `support.rs` by path. Directly after these two lines:

```rust
#[path = "triggers_runtime_apply.rs"]
mod triggers_runtime_apply;
```

add:

```rust
#[path = "optimization.rs"]
mod optimization;
```

and directly after `pub(crate) use triggers_runtime_apply::*;` add:

```rust
pub(crate) use optimization::*;
```

`optimization.rs` is a child module of `support`, so its `use super::{LeanApplyDesiredRow, LeanApplyDocRef};` resolves through the `triggers_runtime_apply::*` re-export.

In the `LeanContractSnapshot` struct, directly after `pub(crate) apply_reconcile_cases: Vec<LeanApplyReconcileCase>,` add:

```rust
    pub(crate) publish_if_cases: Vec<LeanPublishIfCase>,
    pub(crate) optimization_cases: LeanOptimizationCases,
```

After `lean_apply_reconcile_case` add:

```rust
pub(crate) fn lean_publish_if_cases() -> &'static [LeanPublishIfCase] {
    &lean_contract_snapshot().publish_if_cases
}

pub(crate) fn lean_optimization_cases() -> &'static LeanOptimizationCases {
    &lean_contract_snapshot().optimization_cases
}
```

- [ ] **Step 3: Pin counts and register emitted domains**

In `crates/gents/tests/conformance/coverage.rs`, after `assert_eq!(lean_contract_snapshot().apply_reconcile_cases.len(), 8);` add:

```rust
    assert_eq!(lean_contract_snapshot().publish_if_cases.len(), 7);
    assert_eq!(lean_contract_snapshot().optimization_cases.imputation.len(), 8);
    assert_eq!(lean_contract_snapshot().optimization_cases.decisions.len(), 16);
    assert_eq!(lean_contract_snapshot().optimization_cases.gates.len(), 10);
```

After the `if !snapshot.apply_reconcile_cases.is_empty() { ... }` block add:

```rust
    if !snapshot.publish_if_cases.is_empty() {
        emitted.insert(("publish_if_cases".to_string(), "PublishIfCases".to_string()));
    }
    if !snapshot.optimization_cases.gates.is_empty() {
        emitted.insert((
            "optimization_cases".to_string(),
            "OptimizationCases".to_string(),
        ));
    }
```

- [ ] **Step 4: Run conformance**

Run: `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
Expected: PASS. A failure naming an unknown field means a Lean key and a Rust field disagree; fix the spelling on the Rust side to match the JSON keys in Task 4.

- [ ] **Step 5: Commit and open PR 2**

```bash
git add crates/gents/src/lean_vocab_test crates/gents/tests/conformance/coverage.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(conformance): deserialize publishIf and optimization cases"
```

PR description: baseline `optimization/01-lean`; owners `Conformance` snapshot and `lean_vocab_test`; both new domains are `followUpCoverage` until their consumers land; validation `lake build` and `cargo test -p gents --test conformance`.

---

## PR 3: Rust compare-and-set

Branch: `optimization/03-cas`, base `optimization/02-conformance`.

### Task 6: `expected` on the plan and the check in apply

**Files:**
- Modify: `crates/gents/src/config_client/desired_state.rs`
- Modify: `crates/gents/src/config_client/mod.rs`
- Test: `crates/gents/src/config_client/desired_state/tests.rs`

**Interfaces:**
- Consumes: `read_desired_state_document_in_txn`, `desired_state_document_digest`, `reference_filter`, all in `desired_state.rs`.
- Produces:
  - `pub struct DesiredStateExpectation { pub collection: Collection, pub owner: String, pub id: String, pub digest: Option<String> }`
  - `DesiredStateApplyPlan::with_expected(self, Vec<DesiredStateExpectation>) -> Result<Self>` and `expected(&self) -> &[DesiredStateExpectation]`
  - `pub struct StaleExpectation { pub drifted: Vec<DriftedDocument> }` and `pub struct DriftedDocument { pub collection: Collection, pub owner: String, pub id: String, pub expected: Option<String>, pub found: Option<String> }`
  - `pub fn stale_expectation(error: &anyhow::Error) -> Option<&StaleExpectation>`
  - `pub fn desired_state_document_digest` becomes `pub` (was `pub(crate)`) so the CLI in Plan 3 can show digests.

- [ ] **Step 1: Write the failing tests**

Append to `crates/gents/src/config_client/desired_state/tests.rs`:

```rust
fn expectation(owner: &str, id: &str, digest: Option<String>) -> DesiredStateExpectation {
    DesiredStateExpectation {
        collection: Collection::InferenceBackend,
        owner: owner.to_owned(),
        id: id.to_owned(),
        digest,
    }
}

async fn cas_node() -> Result<(ConfigAccess, &'static str)> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    Ok((ConfigAccess::Local(node), "did:key:owner"))
}

async fn live_digest(access: &ConfigAccess, owner: &str, id: &str) -> Result<Option<String>> {
    let (owner, id) = (owner.to_owned(), id.to_owned());
    access
        .transact("test.cas.read", |txn| {
            let (owner, id) = (owner.clone(), id.clone());
            Box::pin(async move {
                read_desired_state_document_in_txn(txn, Collection::InferenceBackend, &owner, &id)
                    .await?
                    .map(|live| desired_state_document_digest(&live))
                    .transpose()
            })
        })
        .await
}

fn renamed(owner: &str, id: &str, name: &str) -> Value {
    let mut value = backend(owner, id);
    value["name"] = name.into();
    value
}

#[tokio::test]
async fn matching_expectation_applies_the_plan() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(&access, vec![document(backend(owner, "target"))]).await?;
    let digest = live_digest(&access, owner, "target").await?;
    let plan = DesiredStateApplyPlan::new(vec![document(renamed(owner, "target", "Promoted"))])?
        .with_expected(vec![expectation(owner, "target", digest)])?;
    access
        .transact("test.cas.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    assert_ne!(live_digest(&access, owner, "target").await?, None);
    Ok(())
}

#[tokio::test]
async fn drifted_expectation_refuses_and_writes_nothing() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(&access, vec![document(backend(owner, "target")), document(backend(owner, "other"))])
        .await?;
    let frozen_other = live_digest(&access, owner, "other").await?;
    let frozen_target = live_digest(&access, owner, "target").await?;
    // An operator edits a closure document after the freeze.
    apply(&access, vec![document(renamed(owner, "other", "Edited"))]).await?;
    let before = live_digest(&access, owner, "target").await?;

    let plan = DesiredStateApplyPlan::new(vec![document(renamed(owner, "target", "Promoted"))])?
        .with_expected(vec![
            expectation(owner, "target", frozen_target),
            expectation(owner, "other", frozen_other.clone()),
        ])?;
    let error = access
        .transact("test.cas.stale", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();

    let stale = stale_expectation(&error).expect("typed StaleExpectation");
    assert_eq!(stale.drifted.len(), 1);
    assert_eq!(stale.drifted[0].id, "other");
    assert_eq!(stale.drifted[0].expected, frozen_other);
    assert_eq!(live_digest(&access, owner, "target").await?, before, "target must be untouched");
    Ok(())
}

#[tokio::test]
async fn absent_expectations_are_checked_in_both_directions() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(&access, vec![document(backend(owner, "present"))]).await?;

    let must_be_absent = DesiredStateApplyPlan::new(vec![document(backend(owner, "fresh"))])?
        .with_expected(vec![expectation(owner, "fresh", None)])?;
    access
        .transact("test.cas.absent_ok", |txn| {
            let plan = &must_be_absent;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;

    let wrongly_absent = DesiredStateApplyPlan::new(vec![document(backend(owner, "again"))])?
        .with_expected(vec![expectation(owner, "present", None)])?;
    let error = access
        .transact("test.cas.absent_stale", |txn| {
            let plan = &wrongly_absent;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    assert!(stale_expectation(&error).is_some());

    let wrongly_present = DesiredStateApplyPlan::new(vec![document(backend(owner, "third"))])?
        .with_expected(vec![expectation(owner, "missing", Some("sha256:00".into()))])?;
    let error = access
        .transact("test.cas.present_stale", |txn| {
            let plan = &wrongly_present;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    let stale = stale_expectation(&error).expect("typed StaleExpectation");
    assert_eq!(stale.drifted[0].found, None);
    Ok(())
}

#[test]
fn duplicate_expectations_are_rejected() {
    let plan = DesiredStateApplyPlan::new(Vec::new()).unwrap();
    let error = plan
        .with_expected(vec![
            expectation("did:key:owner", "a", None),
            expectation("did:key:owner", "a", None),
        ])
        .unwrap_err();
    assert!(format!("{error:#}").contains("duplicate expectation"));
}
```

Add `DesiredStateExpectation, stale_expectation` to the `use super::...` import at the top of the tests file.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gents --lib config_client::desired_state::tests::drifted_expectation`
Expected: compile error, `cannot find type DesiredStateExpectation`.

- [ ] **Step 3: Implement**

In `desired_state.rs`, change `pub(crate) fn desired_state_document_digest` to `pub fn desired_state_document_digest`.

Add below the `DesiredStateApplyDocument` struct:

```rust
/// A digest precondition checked inside the publishing transaction. `digest:
/// None` requires the document to be absent. Digests come from
/// [`desired_state_document_digest`] over the normalized live projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredStateExpectation {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftedDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub expected: Option<String>,
    pub found: Option<String>,
}

/// Refinement of `ApplyReconcile.publishIf`: a stale expectation leaves desired
/// and live state unchanged. Recover it with [`stale_expectation`].
#[derive(Debug)]
pub struct StaleExpectation {
    pub drifted: Vec<DriftedDocument>,
}

impl std::fmt::Display for StaleExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "desired configuration changed since it was read:")?;
        for document in &self.drifted {
            write!(
                formatter,
                " {} {:?}/{:?}",
                document.collection.graphql_type(),
                document.owner,
                document.id
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for StaleExpectation {}

pub fn stale_expectation(error: &anyhow::Error) -> Option<&StaleExpectation> {
    error.downcast_ref::<StaleExpectation>()
}
```

Change the plan struct and add the builder and accessor:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyPlan {
    documents: Vec<DesiredStateApplyDocument>,
    removals: Vec<(Collection, String, String)>,
    expected: Vec<DesiredStateExpectation>,
}
```

In `DesiredStateApplyPlan::new`, change the final constructor to:

```rust
        Ok(Self {
            documents,
            removals: Vec::new(),
            expected: Vec::new(),
        })
```

Add inside `impl DesiredStateApplyPlan`, after `with_removals`:

```rust
    /// Guard this publication with digest preconditions. Expectations may name
    /// documents the plan does not write: promotion freezes a whole closure
    /// and writes only its targets.
    pub fn with_expected(mut self, expected: Vec<DesiredStateExpectation>) -> Result<Self> {
        let mut identities = BTreeSet::new();
        for expectation in &expected {
            reference_filter(expectation.collection, &expectation.owner, &expectation.id)?;
            anyhow::ensure!(
                identities.insert((
                    expectation.collection,
                    expectation.owner.clone(),
                    expectation.id.clone()
                )),
                "duplicate expectation {} {:?}/{:?}",
                expectation.collection.graphql_type(),
                expectation.owner,
                expectation.id
            );
        }
        self.expected = expected;
        Ok(self)
    }

    pub fn expected(&self) -> &[DesiredStateExpectation] {
        &self.expected
    }
```

Add the check function above `apply_desired_state_plan`:

```rust
async fn ensure_expectations_hold(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut drifted = Vec::new();
    for expectation in plan.expected() {
        let found = read_desired_state_document_in_txn(
            txn,
            expectation.collection,
            &expectation.owner,
            &expectation.id,
        )
        .await?
        .map(|live| desired_state_document_digest(&live))
        .transpose()?;
        if found != expectation.digest {
            drifted.push(DriftedDocument {
                collection: expectation.collection,
                owner: expectation.owner.clone(),
                id: expectation.id.clone(),
                expected: expectation.digest.clone(),
                found,
            });
        }
    }
    if drifted.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(StaleExpectation { drifted }))
    }
}
```

In `apply_desired_state_plan`, make this the first statement of the body, before `let mut counts = ...`:

```rust
    ensure_expectations_hold(txn, plan).await?;
```

Update the doc comment on `apply_desired_state_plan` by appending: `Digest expectations are checked first, in the same transaction; a mismatch returns [`StaleExpectation`] before any write.`

In `crates/gents/src/config_client/mod.rs`, extend the existing `pub use desired_state::{...}` re-export with `desired_state_document_digest, stale_expectation, DesiredStateExpectation, DriftedDocument, StaleExpectation`. If `desired_state_document_digest` is already re-exported as `pub(crate) use`, move it into the `pub use` list.

- [ ] **Step 4: Generalize the test-only verifier**

Replace the whole `#[cfg(test)] pub(crate) async fn verify_existing_desired_state_plan` with a non-test helper that builds expectations from a plan's own documents, so there is one digest-comparison path:

```rust
/// Expect every document this plan writes that already exists to still match
/// the plan's authored create form. Absent documents carry no expectation.
pub(crate) async fn expect_existing_documents_unchanged(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut expected = Vec::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        if read_desired_state_document_in_txn(txn, document.collection, owner, id)
            .await?
            .is_some()
        {
            expected.push(DesiredStateExpectation {
                collection: document.collection,
                owner: owner.to_owned(),
                id: id.to_owned(),
                digest: Some(desired_state_document_digest(&document.add)?),
            });
        }
    }
    let guarded = DesiredStateApplyPlan::new(Vec::new())?.with_expected(expected)?;
    ensure_expectations_hold(txn, &guarded).await
}
```

In `tests.rs`, rename the two call sites of `verify_existing_desired_state_plan` inside `verify_existing_desired_state_plan_rejects_drifted_live_rows` to `expect_existing_documents_unchanged`, rename that test to `expect_existing_documents_unchanged_rejects_drifted_live_rows`, and change its final assertion to:

```rust
    assert!(
        stale_expectation(&error).is_some(),
        "drifted live row must be rejected: {error:#}"
    );
```

Update the tests file import accordingly.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p gents --lib config_client::desired_state`
Expected: PASS, including the four new tests and the renamed one.

- [ ] **Step 6: Workspace check**

Run: `cargo check --workspace --all-targets`
Expected: success. `DesiredStateApplyPlan` has private fields and is only built through `new`, `from_pack_config` and `with_removals`, so the roughly 90 call sites of `apply_desired_state_plan` need no change. If any crate constructs the struct literally, add `expected: Vec::new()` there.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/config_client
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(config-client): digest expectations on desired-state publication (#1455)"
```

### Task 7: Consume the `publishIf` cases

**Files:**
- Test: `crates/gents/src/config_client/desired_state/tests.rs`
- Modify: `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean`

**Interfaces:**
- Consumes: `crate::lean_vocab_test::lean_publish_if_cases()` (Task 5) and everything from Task 6.

The Lean model compares desired fields; Rust compares digests. The consumer maps each Lean `content` string onto the `name` field of an `InferenceBackend` document, so two rows have equal digests exactly when their Lean contents are equal. References in the Lean rows are not materialized: reference closure is a separate, already-tested gate, and these cases exercise only the precondition.

- [ ] **Step 1: Write the consumer test**

Append to `tests.rs`:

```rust
#[tokio::test]
async fn guarded_publication_matches_lean_publish_if_cases() -> Result<()> {
    use crate::lean_vocab_test::lean_publish_if_cases;

    fn doc_for(owner: &str, id: &str, content: &str) -> Value {
        renamed(owner, id, content)
    }

    for case in lean_publish_if_cases() {
        let (access, _) = cas_node().await?;
        let prior = case
            .pre_desired
            .iter()
            .map(|row| document(doc_for(&row.target.agent_did, &row.target.id, &row.content)))
            .collect::<Vec<_>>();
        apply(&access, prior).await?;

        let expected = case
            .expected
            .iter()
            .map(|row| {
                let digest = row
                    .content
                    .as_deref()
                    .map(|content| {
                        let (_, projected) = config_projection(
                            Collection::InferenceBackend,
                            Some(&doc_for(&row.target.agent_did, &row.target.id, content)),
                        )?;
                        desired_state_document_digest(&projected.context("projection")?)
                    })
                    .transpose()?;
                Ok(expectation(&row.target.agent_did, &row.target.id, digest))
            })
            .collect::<Result<Vec<_>>>()?;
        let plan = DesiredStateApplyPlan::new(
            case.candidate
                .iter()
                .map(|row| document(doc_for(&row.target.agent_did, &row.target.id, &row.content)))
                .collect(),
        )?
        .with_expected(expected)?;

        let outcome = access
            .transact("test.cas.lean", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await;
        assert_eq!(outcome.is_ok(), case.applied, "case {}", case.name);
        if let Err(error) = &outcome {
            assert!(stale_expectation(error).is_some(), "case {}: {error:#}", case.name);
        }

        for row in &case.expected_after_desired {
            let live = live_digest(&access, &row.target.agent_did, &row.target.id).await?;
            let (_, projected) = config_projection(
                Collection::InferenceBackend,
                Some(&doc_for(&row.target.agent_did, &row.target.id, &row.content)),
            )?;
            let want = desired_state_document_digest(&projected.context("projection")?)?;
            assert_eq!(live, Some(want), "case {} document {}", case.name, row.target.id);
        }
    }
    Ok(())
}
```

If `config_projection` is not already imported in the tests file, add it to the `use super::...` list.

- [ ] **Step 2: Run it**

Run: `RUST_MIN_STACK=16777216 cargo test -p gents --lib config_client::desired_state::tests::guarded_publication_matches_lean_publish_if_cases`
Expected: PASS for all 7 cases. If a digest differs because the live projection normalizes defaults that the authored value lacks, compute `live_digest` and `want` through the same `config_projection` call on both sides rather than loosening the comparison.

- [ ] **Step 3: Flip the ledger entry to a consumer**

In `CoverageLedger.lean`, replace the `publish_if_cases` entry from Task 4 with:

```lean
  , tagged (consumerCoverage
      "publish_if_cases"
      "PublishIfCases"
      "config_client::desired_state::tests::guarded_publication_matches_lean_publish_if_cases")
      "apply-reconcile" [Surface.operatorCli]
```

- [ ] **Step 4: Full validation**

Run, in order:
- `cd crates/gents/proofs && lake build`
- `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
- `cargo test -p gents`
- `cargo check --workspace --all-targets`

Expected: all succeed.

- [ ] **Step 5: Commit and open PR 3**

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(config-client): guarded publication matches Lean publishIf cases"
```

PR description: baseline `optimization/02-conformance`; owner `config_client::desired_state`; deletion of the test-only `verify_existing_desired_state_plan` in favor of `expect_existing_documents_unchanged` on the shared path; `cleanup` deliberately untouched, with the reason from Global Constraints; validation as in Step 4. Record one observation for maintainers without acting on it: `pack install --digest` compares its artifact digest outside the write transaction, so live drift between that check and the install transaction is not detected.
