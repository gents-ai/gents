# Optimization Policy and Lean Model (M6a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the pure half of configuration optimization: the Lean model of the promotion decision and job journal, its generated conformance cases, and the deterministic `PolicyV2` in Rust with its paired sign-flip permutation test.

**Architecture:** `Proofs/Optimization.lean` models the two integer gates over per-case basis-point sums, the ordered decision with a cost sub-gate, and the length-guarded journal. Lean emits cases; a Rust unit test consumes them. `gents::optimization::policy` reads `gents::eval::PairedEvidence` and returns a decision with the numbers behind it. Nothing here runs an eval or touches a database.

**Tech Stack:** Lean 4 (v4.18.0) with Mathlib, Rust 1.97.1, `serde`, `proptest`.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-on-eval-design.md` sections 4 and 8.

**Depends on:** `2026-09-21-eval-core-contract.md` PR A (`Proofs/Eval.lean`) for the Lean work, and its PR E (`gents::eval::scoring`) for the Rust work. The driver, the job record, `promote` and `revert` are not in this plan: they need the runner API from eval spec 2.

## Global Constraints

- Order is Lean, then conformance, then Rust. Each PR targets its parent branch.
- **Narrow Lean imports only.** The Mathlib prebuilt cache is not available on the development machine, so only the Mathlib modules the repo already imports are compiled. Never import `Mathlib.Tactic` or any other broad module: it forces a from-source build of most of Mathlib. `simp`, `simp_all`, `decide`, `cases`, `rcases`, `split`, `omega`, `rename_i` and `rfl` are in Lean core. Before adding an import, confirm its `.olean` exists under `.lake/packages/mathlib/.lake/build/lib/`.
- Lean proofs contain no `sorry`. If a tactic fails, fix the tactic; never weaken a statement without recording why in the PR description.
- Every quantity is an integer: scores and tolerances in basis points, the significance level and p-values in parts per million. Lean and Rust compute the arithmetic gates identically.
- The policy is pure: no I/O, no clock, no unseeded randomness. The Monte Carlo seed is an argument.
- A rule change is a new policy version, never an edit to `PolicyV2`.
- `POLICY_VERSION` is `"v2"`. `"v1"` was the superseded pooled-Fisher design and never shipped.
- Parameter defaults are placeholders marked uncalibrated until the A/A calibration sets them.
- **rustfmt is a CI gate.** `cargo fmt --all --check` must exit 0 before every Rust commit. rustfmt orders `mod` and `use` lines alphabetically, so where a task says to add a module or re-export "directly after" another line in `lean_vocab_test/support.rs` or a `mod.rs`, place it in alphabetical position instead; running `cargo fmt -p gents` does this. New files under `lean_vocab_test/` open with a `//!` header naming the Lean owner they mirror and stating that decoding is strict.
- **Every top-level `Proofs/*.lean` file needs a conformance home.** `tests/conformance/structure.rs` test `every_lean_model_has_a_declared_conformance_home` enumerates them. A PR that adds one must add its entry there in the same PR: a `Gap` with an issue-prefixed rationale until a consumer exists, then `Module("conformance/<file>.rs")`.
- Use `tracing`, never `println!`.
- Before each push: `cargo test -p gents`, `cargo check --workspace --all-targets`, and `lake build` in `crates/gents/proofs` for Lean changes.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/gents/proofs/Proofs/Optimization.lean` | create | unknown imputation, integer gates, ordered decision, journal |
| `crates/gents/proofs/Proofs.lean`, `README.md` | modify | registration and proof map |
| `crates/gents/proofs/Proofs/Conformance/Optimization.lean` | create | emit `optimization_cases` |
| `Snapshot.lean`, `CoverageLedger.lean` | modify | snapshot key and ledger entry |
| `crates/gents/src/lean_vocab_test/optimization.rs`, `support.rs` | create/modify | snapshot structs and accessor |
| `crates/gents/tests/conformance/coverage.rs` | modify | count pins and emitted domain |
| `crates/gents/src/optimization/{mod,policy}.rs` | create | `PolicyV2`, gates, permutation test, `decide` |
| `crates/gents/src/lib.rs` | modify | `pub mod optimization;` |

---

## PR 1: Lean model

Branch: `optimization/10-lean`, base `eval/01-lean`.

### Task 1: `Proofs/Optimization.lean`

**Files:**
- Create: `crates/gents/proofs/Proofs/Optimization.lean`
- Modify: `crates/gents/proofs/Proofs.lean`, `crates/gents/proofs/README.md`

**Interfaces:**
- Consumes: `Eval.EvidenceClass` from `Proofs/Eval.lean`.
- Produces, in `namespace Optimization`: `Arm`, `scoreBpMax`, `unknownBp`, `CasePairs`, `Evidence`, `Params`, `alphaEffectivePpm`, `sufficient`, `noCaseRegression`, `costOk`, `Mode`, `RejectReason`, `Decision`, `decideGates`, `Entry`, `Journal`, `appendIf`, `roundsUsed`, `propose`.

- [ ] **Step 1: Write the file**

```lean
import Proofs.Eval
import Mathlib.Data.List.Basic

/-! Configuration optimization (#1455), on the eval core contract.

Models what must stay outside model control: worst-case imputation of an unknown
case-trial, the two integer-arithmetic gates over per-case paired sums, the cost
sub-gate, the ordered decision, and the append-only job journal.

Refinement boundaries: the improvement test (a paired sign-flip permutation test
in Rust) is an abstract `Bool`; eval runs, the proposer and candidate
construction are outside the fence. -/
namespace Optimization

inductive Arm where
  | baseline | candidate
  deriving DecidableEq, Repr

def scoreBpMax : Nat := 10000

/-- The score imputed to an unknown case-trial. It fails the candidate and passes
the baseline, so an unknown can never help a candidate. -/
def unknownBp : Arm → Nat
  | .baseline => scoreBpMax
  | .candidate => 0

theorem candidate_unknown_is_worst (bp : Nat) : unknownBp .candidate ≤ bp := by
  simp [unknownBp]

theorem baseline_unknown_is_best (bp : Nat) (h : bp ≤ scoreBpMax) :
    bp ≤ unknownBp .baseline := by
  simpa [unknownBp] using h

/-- One case: the number of evidence pairs and both arms' summed scores. -/
structure CasePairs where
  pairs : Nat
  sumBaseline : Nat
  sumCandidate : Nat
  deriving DecidableEq, Repr

structure Evidence where
  casesMatch : Bool
  cases : List CasePairs
  /-- Distinct (case, trial index) keys seen in either cell. -/
  keys : Nat
  droppedBaseline : Nat
  droppedCandidate : Nat
  deriving Repr

structure Params where
  minPairs : Nat
  maxNotEvidenceBp : Nat
  maxAsymmetryBp : Nat
  caseToleranceBp : Nat
  alphaPpm : Nat
  maxRounds : Nat
  maxTokenIncreaseBp : Nat
  deriving DecidableEq, Repr

/-- Bonferroni: one validation split, up to `maxRounds` candidates. -/
def alphaEffectivePpm (p : Params) : Nat := p.alphaPpm / (max p.maxRounds 1)

def absDiff (a b : Nat) : Nat := (a - b) + (b - a)

/-- Gate 1. The last conjunct says the permutation test can reach significance at
all: `2^-n ≤ alpha`, written without division. -/
def sufficient (p : Params) (e : Evidence) : Bool :=
  e.casesMatch && !e.cases.isEmpty
    && e.cases.all (fun c => decide (p.minPairs ≤ c.pairs))
    && decide (e.droppedBaseline * 10000 ≤ p.maxNotEvidenceBp * e.keys)
    && decide (e.droppedCandidate * 10000 ≤ p.maxNotEvidenceBp * e.keys)
    && decide (absDiff e.droppedBaseline e.droppedCandidate * 10000 ≤ p.maxAsymmetryBp * e.keys)
    && decide (1000000 ≤ alphaEffectivePpm p * 2 ^ e.cases.length)

/-- Gate 2a. `mean(cand) ≥ mean(base) − tol` per case, cross-multiplied by `pairs`. -/
def noCaseRegression (p : Params) (e : Evidence) : Bool :=
  e.cases.all fun c => decide (c.sumBaseline ≤ c.sumCandidate + p.caseToleranceBp * c.pairs)

/-- Gate 2b. Mean tokens per case-trial, cross-multiplied. -/
def costOk (p : Params) (baseTokens baseTrials candTokens candTrials : Nat) : Bool :=
  decide (candTokens * baseTrials * 10000
    ≤ baseTokens * candTrials * (10000 + p.maxTokenIncreaseBp))

inductive Mode where
  | improve | confirm
  deriving DecidableEq, Repr

inductive RejectReason where
  | caseRegression | costRegression | noImprovement
  deriving DecidableEq, Repr

inductive Decision where
  | accept
  | reject (reason : RejectReason)
  | inconclusive
  deriving DecidableEq, Repr

/-- The first failing gate decides. `confirm` never consults the improvement test. -/
def decideGates (mode : Mode) (sufficient noCaseRegression costOk improves : Bool) : Decision :=
  if !sufficient then .inconclusive
  else if !noCaseRegression then .reject .caseRegression
  else if !costOk then .reject .costRegression
  else match mode with
    | .confirm => .accept
    | .improve => if improves then .accept else .reject .noImprovement

theorem improve_accept_iff (s r c i : Bool) :
    decideGates .improve s r c i = .accept ↔ s = true ∧ r = true ∧ c = true ∧ i = true := by
  cases s <;> cases r <;> cases c <;> cases i <;> decide

theorem confirm_accept_iff (s r c i : Bool) :
    decideGates .confirm s r c i = .accept ↔ s = true ∧ r = true ∧ c = true := by
  cases s <;> cases r <;> cases c <;> cases i <;> decide

theorem inconclusive_iff (mode : Mode) (s r c i : Bool) :
    decideGates mode s r c i = .inconclusive ↔ s = false := by
  cases mode <;> cases s <;> cases r <;> cases c <;> cases i <;> decide

/-- Weakening any gate never turns a non-accept into an accept. -/
theorem accept_antitone (mode : Mode) (s r c i s' r' c' i' : Bool)
    (hs : s' = true → s = true) (hr : r' = true → r = true)
    (hc : c' = true → c = true) (hi : i' = true → i = true)
    (h : decideGates mode s' r' c' i' = .accept) : decideGates mode s r c i = .accept := by
  cases mode <;> cases s <;> cases r <;> cases c <;> cases i <;>
    cases s' <;> cases r' <;> cases c' <;> cases i' <;> simp_all [decideGates]

/-- Too few cases can never be accepted, whatever the scores. -/
theorem too_few_cases_never_accepts (mode : Mode) (p : Params) (e : Evidence) (r c i : Bool)
    (h : alphaEffectivePpm p * 2 ^ e.cases.length < 1000000) :
    decideGates mode (sufficient p e) r c i ≠ .accept := by
  have hs : sufficient p e = false := by
    simp only [sufficient, Bool.and_eq_false_iff, decide_eq_false_iff_not]
    right
    omega
  simp [decideGates, hs]

/-! ## Job journal -/

inductive Entry where
  | frozen | runStarted | proposed | structuralReject
  | decided (d : Decision)
  | budgetExhausted | finalized
  | promoted | promotionRefused | reverted
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

- [ ] **Step 2: Register and map**

In `crates/gents/tests/conformance/structure.rs`, add an `Optimization` entry next to the `Eval` entry, as a `Gap` whose rationale begins `#1455` and says the consumer is `optimization::policy::tests::gates_costs_and_decisions_match_lean`, landing in the policy PR of this stack. Copy the `Eval` entry's exact form. Validate it with `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance every_lean_model_has_a_declared_conformance_home`.

Append to `crates/gents/proofs/Proofs.lean`:

```lean

import Proofs.Optimization
```

In `README.md`, in the `| File | Contents |` table after the `Proofs/Eval.lean` row, add:

```
| `Proofs/Optimization.lean` | Configuration optimization (#1455) on the eval contract: worst-case unknown imputation, integer sufficiency and non-regression gates over per-case paired sums, the cost sub-gate, the ordered decision, the rule that too few cases can never be accepted, and the length-guarded job journal with bounded rounds. The permutation test, eval runs and the proposer are refinement boundaries |
```

- [ ] **Step 3: Build**

Run: `cd crates/gents/proofs && lake build`
Expected: success, no `sorry`.
Likely repair points. `too_few_cases_never_accepts`: if the `simp only` does not reduce the nested `&&`, replace the `have` with `have hs : sufficient p e = false := by unfold sufficient; simp only [Bool.and_eq_false_iff, decide_eq_false_iff_not, not_le]; right; exact h`. `propose_bounded`: try `simp [roundsUsed, List.count_append, List.count_cons] at *; omega`. `accept_antitone` expands to 512 cases; if it is slow, prove it as `revert hs hr hc hi h; cases mode <;> ... <;> decide`. Statements stay as written.

- [ ] **Step 4: Commit and open PR 1**

```bash
git add crates/gents/proofs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(optimization): paired integer gates, ordered decision and job journal (#1455)"
```

PR description: baseline `eval/01-lean`; new owner `Proofs/Optimization`; no deletions; validation `lake build`, result pasted because `lean-proofs` is skipped on non-tip stacked PRs.

---

## PR 2: Conformance emission

Branch: `optimization/11-conformance`, base `optimization/10-lean`, and it must sit above `eval/02-conformance` so the `"eval"` feature and the snapshot conventions exist. If the eval stack has not merged, rebase this branch onto `eval/02-conformance` and cherry-pick PR 1's commit beneath it.

### Task 2: Emit and deserialize `optimization_cases`

**Files:**
- Create: `crates/gents/proofs/Proofs/Conformance/Optimization.lean`, `crates/gents/src/lean_vocab_test/optimization.rs`
- Modify: `Snapshot.lean`, `CoverageLedger.lean`, `crates/gents/src/lean_vocab_test/support.rs`, `crates/gents/tests/conformance/coverage.rs`

**Interfaces:**
- Produces: snapshot key `optimization_cases` and `lean_optimization_cases() -> &'static LeanOptimizationCases` with the field names in Step 3.

- [ ] **Step 1: Lean cases**

```lean
import Proofs.Optimization
import Proofs.Conformance.ContractTypes

/-! Generated witnesses for the optimization promotion policy. Rust must reproduce
both integer gates, the cost gate and the ordered decision exactly. -/
namespace Conformance.Optimization

open Conformance.Contracts
open _root_.Optimization

private def boolJson (b : Bool) : String := if b then "true" else "false"

private def modeJson : Mode → String
  | .improve => "\"improve\"" | .confirm => "\"confirm\""

private def decisionJson : Decision → String
  | .accept => "\"accept\""
  | .inconclusive => "\"inconclusive\""
  | .reject .caseRegression => "\"reject_case_regression\""
  | .reject .costRegression => "\"reject_cost_regression\""
  | .reject .noImprovement => "\"reject_no_improvement\""

private def bools : List Bool := [true, false]

private def decisionRows : List String :=
  [Mode.improve, Mode.confirm].flatMap fun m =>
    bools.flatMap fun s => bools.flatMap fun r => bools.flatMap fun c => bools.map fun i =>
      "{\"mode\":" ++ modeJson m ++ ",\"sufficient\":" ++ boolJson s
        ++ ",\"no_case_regression\":" ++ boolJson r ++ ",\"cost_ok\":" ++ boolJson c
        ++ ",\"improves\":" ++ boolJson i
        ++ ",\"decision\":" ++ decisionJson (decideGates m s r c i) ++ "}"

private def params : Params :=
  { minPairs := 2, maxNotEvidenceBp := 2000, maxAsymmetryBp := 1000, caseToleranceBp := 3000,
    alphaPpm := 50000, maxRounds := 3, maxTokenIncreaseBp := 2500 }

private def c (n b k : Nat) : CasePairs := ⟨n, b, k⟩

private def six : List CasePairs :=
  [c 2 10000 16000, c 2 8000 14000, c 2 12000 18000, c 2 6000 12000, c 2 10000 15000, c 2 9000 16000]

private def gateScenarios : List (String × Evidence) :=
  [("six_clean_cases", ⟨true, six, 12, 0, 0⟩),
   ("cases_do_not_match", ⟨false, six, 12, 0, 0⟩),
   ("no_cases", ⟨true, [], 0, 0, 0⟩),
   ("five_cases_cannot_reach_alpha", ⟨true, six.take 5, 10, 0, 0⟩),
   ("too_few_pairs_in_one_case", ⟨true, c 1 5000 9000 :: six.drop 1, 12, 0, 0⟩),
   ("too_much_not_evidence", ⟨true, six, 12, 3, 3⟩),
   ("asymmetric_exclusion", ⟨true, six, 12, 0, 2⟩),
   ("one_case_regresses", ⟨true, c 2 18000 8000 :: six.drop 1, 12, 0, 0⟩),
   ("regression_at_the_tolerance", ⟨true, c 2 16000 10000 :: six.drop 1, 12, 0, 0⟩),
   ("regression_just_past_tolerance", ⟨true, c 2 16001 10000 :: six.drop 1, 12, 0, 0⟩)]

private def caseJson (x : CasePairs) : String :=
  "{\"pairs\":" ++ toString x.pairs ++ ",\"sum_baseline\":" ++ toString x.sumBaseline
    ++ ",\"sum_candidate\":" ++ toString x.sumCandidate ++ "}"

private def gateRow (name : String) (e : Evidence) : String :=
  "{\"name\":" ++ jsonString name
    ++ ",\"cases_match\":" ++ boolJson e.casesMatch
    ++ ",\"cases\":" ++ jsonArray (e.cases.map caseJson)
    ++ ",\"keys\":" ++ toString e.keys
    ++ ",\"dropped_baseline\":" ++ toString e.droppedBaseline
    ++ ",\"dropped_candidate\":" ++ toString e.droppedCandidate
    ++ ",\"sufficient\":" ++ boolJson (sufficient params e)
    ++ ",\"no_case_regression\":" ++ boolJson (noCaseRegression params e) ++ "}"

private def costScenarios : List (String × Nat × Nat × Nat × Nat) :=
  [("same_cost", 1000, 10, 1000, 10),
   ("at_the_limit", 1000, 10, 1250, 10),
   ("just_over_the_limit", 1000, 10, 1251, 10),
   ("cheaper", 1000, 10, 600, 10),
   ("unequal_trial_counts_within_limit", 1000, 10, 600, 5),
   ("unequal_trial_counts_over_limit", 1000, 10, 700, 5)]

private def costRow : String × Nat × Nat × Nat × Nat → String
  | (name, bt, bn, ct, cn) =>
    "{\"name\":" ++ jsonString name
      ++ ",\"baseline_tokens\":" ++ toString bt ++ ",\"baseline_trials\":" ++ toString bn
      ++ ",\"candidate_tokens\":" ++ toString ct ++ ",\"candidate_trials\":" ++ toString cn
      ++ ",\"cost_ok\":" ++ boolJson (costOk params bt bn ct cn) ++ "}"

def optimizationCasesJson : String :=
  "{\"params\":{\"min_pairs\":" ++ toString params.minPairs
    ++ ",\"max_not_evidence_bp\":" ++ toString params.maxNotEvidenceBp
    ++ ",\"max_asymmetry_bp\":" ++ toString params.maxAsymmetryBp
    ++ ",\"case_tolerance_bp\":" ++ toString params.caseToleranceBp
    ++ ",\"alpha_ppm\":" ++ toString params.alphaPpm
    ++ ",\"max_rounds\":" ++ toString params.maxRounds
    ++ ",\"max_token_increase_bp\":" ++ toString params.maxTokenIncreaseBp
    ++ ",\"alpha_effective_ppm\":" ++ toString (alphaEffectivePpm params) ++ "}"
    ++ ",\"decisions\":" ++ jsonArray decisionRows
    ++ ",\"gates\":" ++ jsonArray (gateScenarios.map fun (n, e) => gateRow n e)
    ++ ",\"costs\":" ++ jsonArray (costScenarios.map costRow) ++ "}"

end Conformance.Optimization
```

Save it as `crates/gents/proofs/Proofs/Conformance/Optimization.lean`. Counts: 32 decisions, 10 gate rows, 6 cost rows.

- [ ] **Step 2: Snapshot and ledger**

In `Snapshot.lean`, add `import Proofs.Conformance.Optimization` after the last `import`, and directly after the `eval_outcome_cases` splice insert:

```lean
    ++ "\"optimization_cases\":"
      ++ Conformance.Optimization.optimizationCasesJson ++ ","
```

In `CoverageLedger.lean`, inside `featureSurfaceRequirements` after the `"eval"` entry:

```lean
  , { feature := "optimization"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#1455")]
    }
```

and inside `caseCoverage` directly after the `eval_outcome_cases` entry:

```lean
  , tagged (followUpCoverage
      "optimization_cases"
      "OptimizationCases"
      "Consumed by optimization::policy::tests once PolicyV2 lands in the next stacked PR.")
      "optimization" [Surface.operatorCli]
```

- [ ] **Step 3: Rust structs**

Create `crates/gents/src/lean_vocab_test/optimization.rs`:

```rust
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationParams {
    pub(crate) min_pairs: u64,
    pub(crate) max_not_evidence_bp: u64,
    pub(crate) max_asymmetry_bp: u64,
    pub(crate) case_tolerance_bp: u64,
    pub(crate) alpha_ppm: u64,
    pub(crate) max_rounds: u32,
    pub(crate) max_token_increase_bp: u64,
    pub(crate) alpha_effective_ppm: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanDecisionRow {
    pub(crate) mode: String,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
    pub(crate) cost_ok: bool,
    pub(crate) improves: bool,
    pub(crate) decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCasePairs {
    pub(crate) pairs: u64,
    pub(crate) sum_baseline: u64,
    pub(crate) sum_candidate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanGateRow {
    pub(crate) name: String,
    pub(crate) cases_match: bool,
    pub(crate) cases: Vec<LeanCasePairs>,
    pub(crate) keys: u64,
    pub(crate) dropped_baseline: u64,
    pub(crate) dropped_candidate: u64,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCostRow {
    pub(crate) name: String,
    pub(crate) baseline_tokens: u64,
    pub(crate) baseline_trials: u64,
    pub(crate) candidate_tokens: u64,
    pub(crate) candidate_trials: u64,
    pub(crate) cost_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationCases {
    pub(crate) params: LeanOptimizationParams,
    pub(crate) decisions: Vec<LeanDecisionRow>,
    pub(crate) gates: Vec<LeanGateRow>,
    pub(crate) costs: Vec<LeanCostRow>,
}
```

In `support.rs`, directly after the `#[path = "eval.rs"] mod eval;` lines add

```rust
#[path = "optimization.rs"]
mod optimization;
```

and after `pub(crate) use eval::*;` add `pub(crate) use optimization::*;`. In `LeanContractSnapshot`, after `eval_outcome_cases`, add `pub(crate) optimization_cases: LeanOptimizationCases,`. After `lean_eval_outcome_cases` add:

```rust
pub(crate) fn lean_optimization_cases() -> &'static LeanOptimizationCases {
    &lean_contract_snapshot().optimization_cases
}
```

- [ ] **Step 4: Coverage pins**

In `coverage.rs`, after the `eval_outcome_cases.len()` assertion add:

```rust
    assert_eq!(lean_contract_snapshot().optimization_cases.decisions.len(), 32);
    assert_eq!(lean_contract_snapshot().optimization_cases.gates.len(), 10);
    assert_eq!(lean_contract_snapshot().optimization_cases.costs.len(), 6);
```

and after the `eval_outcome_cases` emitted-domain block add:

```rust
    if !snapshot.optimization_cases.gates.is_empty() {
        emitted.insert((
            "optimization_cases".to_string(),
            "OptimizationCases".to_string(),
        ));
    }
```

- [ ] **Step 5: Build, run, commit**

Run: `cd crates/gents/proofs && lake build`
Run: `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
Expected: both succeed. The same contingency as the eval plan applies if the feature-matrix test rejects a feature with no consumer: tag the entry `"apply-reconcile"`, drop the `"optimization"` requirement entry, and let Task 3 restore both.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(conformance): emit optimization policy cases"
```

---

## PR 3: `PolicyV2`

Branch: `optimization/12-policy`, base `optimization/11-conformance`, above `eval/05-contract`.

### Task 3: The policy

**Files:**
- Create: `crates/gents/src/optimization/mod.rs`, `crates/gents/src/optimization/policy.rs`
- Modify: `crates/gents/src/lib.rs`, `CoverageLedger.lean`

**Interfaces:**
- Consumes: `crate::eval::{PairedEvidence, Pair}`.
- Produces: `POLICY_VERSION`, `PolicyV2`, `Mode`, `RejectReason`, `InconclusiveReason`, `Decision`, `CaseEvidence`, `TokenTotals`, `Evidence`, `evidence_from_pairs`, `alpha_effective_ppm`, `sufficient`, `no_case_regression`, `cost_ok`, `permutation_p_ppm`, `Gates`, `decide_gates`, `DecisionReport`, `decide`.

- [ ] **Step 1: Failing tests**

Create `policy.rs` with only this module, run `cargo test -p gents --lib optimization::policy`, and confirm it fails to compile.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Pair, PairedEvidence};
    use proptest::prelude::*;

    fn params() -> PolicyV2 {
        PolicyV2 {
            min_pairs: 2,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 3000,
            min_effect_bp: 500,
            alpha_ppm: 50_000,
            max_rounds: 3,
            max_token_increase_bp: 2500,
            max_reruns: 1,
            monte_carlo_samples: 100_000,
        }
    }

    fn paired(cases: &[(&str, &[(u32, u32)])]) -> PairedEvidence {
        let mut pairs = Vec::new();
        for (case, scores) in cases {
            for (index, (baseline_bp, candidate_bp)) in scores.iter().enumerate() {
                pairs.push(Pair {
                    case_id: (*case).into(),
                    trial_index: index as u32,
                    baseline_bp: *baseline_bp,
                    candidate_bp: *candidate_bp,
                });
            }
        }
        PairedEvidence { keys: pairs.len(), pairs, dropped_baseline: 0, dropped_candidate: 0 }
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("case-{i}")).collect()
    }

    fn uniform(n: usize, baseline: u32, candidate: u32) -> (PairedEvidence, Vec<String>) {
        let ids = names(n);
        let scores = [(baseline, candidate), (baseline, candidate)];
        let cases: Vec<(&str, &[(u32, u32)])> =
            ids.iter().map(|id| (id.as_str(), &scores[..])).collect();
        (paired(&cases), ids)
    }

    #[test]
    fn the_permutation_test_is_exact_on_hand_computed_values() {
        // Six positive differences: only the all-plus sign vector reaches the
        // observed sum, so p = 1/64.
        assert_eq!(permutation_p_ppm(&[1, 2, 3, 4, 5, 6], 100_000, 7), 15_625);
        // All zero: every sign vector ties, so p = 1.
        assert_eq!(permutation_p_ppm(&[0, 0, 0, 0], 100_000, 7), 1_000_000);
        // {3,1}: sums are 4, 2, -2, -4; one of four is >= 4.
        assert_eq!(permutation_p_ppm(&[3, 1], 100_000, 7), 250_000);
        // {-3,-1}: observed -4; all four sums are >= -4.
        assert_eq!(permutation_p_ppm(&[-3, -1], 100_000, 7), 1_000_000);
        assert_eq!(permutation_p_ppm(&[], 100_000, 7), 1_000_000);
    }

    #[test]
    fn the_monte_carlo_branch_is_deterministic_for_a_seed() {
        let diffs: Vec<i128> = (1..=24).collect();
        let a = permutation_p_ppm(&diffs, 20_000, 42);
        assert_eq!(a, permutation_p_ppm(&diffs, 20_000, 42));
        assert!(a < 1_000, "24 aligned differences are overwhelmingly significant: {a}");
    }

    #[test]
    fn six_improving_cases_are_accepted_and_five_cannot_be() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &ids, None), 1);
        assert_eq!(report.decision, Decision::Accept);
        assert_eq!((report.improved, report.tied, report.worsened), (6, 0, 0));
        assert_eq!(report.p_ppm, Some(15_625));
        assert_eq!(report.mean_diff_bp, Some(3_000));

        let (five, ids) = uniform(5, 5_000, 8_000);
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&five, &ids, None), 1);
        assert_eq!(report.decision, Decision::Inconclusive(InconclusiveReason::TooFewCases));
    }

    #[test]
    fn a_tie_is_rejected_and_confirm_ignores_improvement() {
        let (tie, ids) = uniform(6, 5_000, 5_000);
        let evidence = evidence_from_pairs(&tie, &ids, None);
        assert_eq!(
            decide(Mode::Improve, &params(), &evidence, 1).decision,
            Decision::Reject(RejectReason::NoImprovement)
        );
        assert_eq!(decide(Mode::Confirm, &params(), &evidence, 1).decision, Decision::Accept);
    }

    #[test]
    fn a_broken_case_rejects_even_when_the_mean_improves() {
        let ids = names(6);
        let good = [(5_000u32, 9_000u32), (5_000, 9_000)];
        let broken = [(9_000u32, 1_000u32), (9_000, 1_000)];
        let mut cases: Vec<(&str, &[(u32, u32)])> =
            ids.iter().skip(1).map(|id| (id.as_str(), &good[..])).collect();
        cases.push((ids[0].as_str(), &broken[..]));
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&paired(&cases), &ids, None),
            1,
        );
        assert_eq!(report.decision, Decision::Reject(RejectReason::CaseRegression));
    }

    #[test]
    fn a_more_expensive_candidate_is_rejected_and_missing_usage_skips_the_gate() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let costly = TokenTotals {
            baseline_tokens: 1_000, baseline_trials: 10, candidate_tokens: 2_000, candidate_trials: 10,
        };
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &ids, Some(costly)), 1);
        assert_eq!(report.decision, Decision::Reject(RejectReason::CostRegression));
        assert!(!report.cost_skipped);
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &ids, None), 1);
        assert!(report.cost_skipped);
        assert_eq!(report.decision, Decision::Accept);
    }

    #[test]
    fn an_unexpected_or_missing_case_is_insufficient() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let mut expected = ids.clone();
        expected.push("case-missing".into());
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &expected, None), 1);
        assert_eq!(report.decision, Decision::Inconclusive(InconclusiveReason::Insufficient));
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &ids[..5], None), 1);
        assert_eq!(report.decision, Decision::Inconclusive(InconclusiveReason::Insufficient));
    }

    #[test]
    fn asymmetric_exclusion_is_insufficient() {
        let (mut six, ids) = uniform(6, 5_000, 8_000);
        six.keys += 2;
        six.dropped_candidate = 2;
        let report = decide(Mode::Improve, &params(), &evidence_from_pairs(&six, &ids, None), 1);
        assert_eq!(report.decision, Decision::Inconclusive(InconclusiveReason::Insufficient));
    }

    #[test]
    fn gates_costs_and_decisions_match_lean() {
        let lean = crate::lean_vocab_test::lean_optimization_cases();
        let policy = PolicyV2 {
            min_pairs: lean.params.min_pairs,
            max_not_evidence_bp: lean.params.max_not_evidence_bp,
            max_asymmetry_bp: lean.params.max_asymmetry_bp,
            case_tolerance_bp: lean.params.case_tolerance_bp,
            alpha_ppm: lean.params.alpha_ppm,
            max_rounds: lean.params.max_rounds,
            max_token_increase_bp: lean.params.max_token_increase_bp,
            ..params()
        };
        assert_eq!(alpha_effective_ppm(&policy), lean.params.alpha_effective_ppm);
        for row in &lean.gates {
            let evidence = Evidence {
                cases_match: row.cases_match,
                cases: row
                    .cases
                    .iter()
                    .enumerate()
                    .map(|(i, c)| CaseEvidence {
                        case_id: format!("case-{i}"),
                        pairs: c.pairs,
                        sum_baseline_bp: c.sum_baseline,
                        sum_candidate_bp: c.sum_candidate,
                    })
                    .collect(),
                keys: row.keys,
                dropped_baseline: row.dropped_baseline,
                dropped_candidate: row.dropped_candidate,
                tokens: None,
            };
            assert_eq!(sufficient(&policy, &evidence), row.sufficient, "{}", row.name);
            assert_eq!(no_case_regression(&policy, &evidence), row.no_case_regression, "{}", row.name);
        }
        for row in &lean.costs {
            let totals = TokenTotals {
                baseline_tokens: row.baseline_tokens,
                baseline_trials: row.baseline_trials,
                candidate_tokens: row.candidate_tokens,
                candidate_trials: row.candidate_trials,
            };
            assert_eq!(cost_ok(&policy, &totals), row.cost_ok, "{}", row.name);
        }
        for row in &lean.decisions {
            let mode = if row.mode == "improve" { Mode::Improve } else { Mode::Confirm };
            let decision = decide_gates(
                mode,
                Gates {
                    sufficient: row.sufficient,
                    no_case_regression: row.no_case_regression,
                    cost_ok: row.cost_ok,
                    improves: row.improves,
                },
                InconclusiveReason::Insufficient,
            );
            let got = match decision {
                Decision::Accept => "accept",
                Decision::Inconclusive(_) => "inconclusive",
                Decision::Reject(RejectReason::CaseRegression) => "reject_case_regression",
                Decision::Reject(RejectReason::CostRegression) => "reject_cost_regression",
                Decision::Reject(RejectReason::NoImprovement) => "reject_no_improvement",
            };
            assert_eq!(got, row.decision, "{row:?}");
        }
    }

    proptest! {
        /// Lowering one candidate score never produces an Accept that was not there.
        #[test]
        fn lowering_a_candidate_score_never_creates_an_accept(
            scores in proptest::collection::vec((0u32..=10_000, 0u32..=10_000), 12),
            victim in 0usize..12,
            drop in 1u32..=10_000,
        ) {
            let ids = names(6);
            let build = |scores: &[(u32, u32)]| {
                let cases: Vec<(&str, &[(u32, u32)])> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (id.as_str(), &scores[i * 2..i * 2 + 2]))
                    .collect();
                evidence_from_pairs(&paired(&cases), &ids, None)
            };
            let mut worse = scores.clone();
            worse[victim].1 = worse[victim].1.saturating_sub(drop);
            for mode in [Mode::Improve, Mode::Confirm] {
                if decide(mode, &params(), &build(&worse), 9).decision == Decision::Accept {
                    prop_assert_eq!(decide(mode, &params(), &build(&scores), 9).decision, Decision::Accept);
                }
            }
        }
    }
}
```

- [ ] **Step 2: Implement**

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::eval::PairedEvidence;

pub const POLICY_VERSION: &str = "v2";

/// Cases at or below this count are tested by exact enumeration.
const EXACT_CASE_LIMIT: usize = 20;

/// Frozen into a job at start. A rule change is a new version, never an edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyV2 {
    pub min_pairs: u64,
    pub max_not_evidence_bp: u64,
    pub max_asymmetry_bp: u64,
    pub case_tolerance_bp: u64,
    pub min_effect_bp: u64,
    pub alpha_ppm: u64,
    pub max_rounds: u32,
    pub max_token_increase_bp: u64,
    pub max_reruns: u32,
    pub monte_carlo_samples: u32,
}

impl PolicyV2 {
    /// Placeholder defaults. They are NOT calibrated; the A/A calibration run
    /// sets defensible values for a given definition and model.
    pub fn uncalibrated() -> Self {
        Self {
            min_pairs: 2,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 5000,
            min_effect_bp: 500,
            alpha_ppm: 50_000,
            max_rounds: 3,
            max_token_increase_bp: 2500,
            max_reruns: 1,
            monte_carlo_samples: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// All gates, on a validation run.
    Improve,
    /// Sufficiency and non-regression only, once, on the held-out run.
    Confirm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    CaseRegression,
    CostRegression,
    NoImprovement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InconclusiveReason {
    Insufficient,
    /// `2^-n_cases` exceeds the effective significance level: no score pattern
    /// could ever be accepted with this many cases.
    TooFewCases,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "reason")]
pub enum Decision {
    Accept,
    Reject(RejectReason),
    Inconclusive(InconclusiveReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseEvidence {
    pub case_id: String,
    pub pairs: u64,
    pub sum_baseline_bp: u64,
    pub sum_candidate_bp: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenTotals {
    pub baseline_tokens: u64,
    pub baseline_trials: u64,
    pub candidate_tokens: u64,
    pub candidate_trials: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub cases_match: bool,
    pub cases: Vec<CaseEvidence>,
    pub keys: u64,
    pub dropped_baseline: u64,
    pub dropped_candidate: u64,
    /// `None` when too many trials lack usage; the cost gate is then skipped.
    pub tokens: Option<TokenTotals>,
}

/// One entry per expected case, in the given order. A case with no pairs stays
/// with `pairs: 0` and fails gate 1. A pair for an unexpected case clears
/// `cases_match`.
pub fn evidence_from_pairs(
    paired: &PairedEvidence,
    expected_cases: &[String],
    tokens: Option<TokenTotals>,
) -> Evidence {
    let mut by_case: BTreeMap<&str, CaseEvidence> = expected_cases
        .iter()
        .map(|id| {
            (id.as_str(), CaseEvidence {
                case_id: id.clone(), pairs: 0, sum_baseline_bp: 0, sum_candidate_bp: 0,
            })
        })
        .collect();
    let mut cases_match = true;
    for pair in &paired.pairs {
        match by_case.get_mut(pair.case_id.as_str()) {
            Some(case) => {
                case.pairs += 1;
                case.sum_baseline_bp += pair.baseline_bp as u64;
                case.sum_candidate_bp += pair.candidate_bp as u64;
            }
            None => cases_match = false,
        }
    }
    Evidence {
        cases_match,
        cases: expected_cases.iter().map(|id| by_case[id.as_str()].clone()).collect(),
        keys: paired.keys as u64,
        dropped_baseline: paired.dropped_baseline as u64,
        dropped_candidate: paired.dropped_candidate as u64,
        tokens,
    }
}

/// `Optimization.alphaEffectivePpm`: Bonferroni over the job's rounds.
pub fn alpha_effective_ppm(policy: &PolicyV2) -> u64 {
    policy.alpha_ppm / policy.max_rounds.max(1) as u64
}

fn enough_cases(policy: &PolicyV2, cases: usize) -> bool {
    // 1_000_000 <= alpha_eff * 2^n, saturating so large n is simply true.
    let power = 1u128.checked_shl(cases as u32).unwrap_or(u128::MAX);
    1_000_000u128 <= (alpha_effective_ppm(policy) as u128).saturating_mul(power)
}

/// Gate 1, `Optimization.sufficient`.
pub fn sufficient(policy: &PolicyV2, evidence: &Evidence) -> bool {
    let keys = evidence.keys as u128;
    let (db, dc) = (evidence.dropped_baseline as u128, evidence.dropped_candidate as u128);
    evidence.cases_match
        && !evidence.cases.is_empty()
        && evidence.cases.iter().all(|case| policy.min_pairs <= case.pairs)
        && db * 10_000 <= policy.max_not_evidence_bp as u128 * keys
        && dc * 10_000 <= policy.max_not_evidence_bp as u128 * keys
        && db.abs_diff(dc) * 10_000 <= policy.max_asymmetry_bp as u128 * keys
        && enough_cases(policy, evidence.cases.len())
}

/// Gate 2a, `Optimization.noCaseRegression`.
pub fn no_case_regression(policy: &PolicyV2, evidence: &Evidence) -> bool {
    evidence.cases.iter().all(|case| {
        case.sum_baseline_bp as u128
            <= case.sum_candidate_bp as u128 + policy.case_tolerance_bp as u128 * case.pairs as u128
    })
}

/// Gate 2b, `Optimization.costOk`.
pub fn cost_ok(policy: &PolicyV2, tokens: &TokenTotals) -> bool {
    tokens.candidate_tokens as u128 * tokens.baseline_trials as u128 * 10_000
        <= tokens.baseline_tokens as u128
            * tokens.candidate_trials as u128
            * (10_000 + policy.max_token_increase_bp as u128)
}

fn gcd(a: u128, b: u128) -> u128 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Per-case mean differences on a common integer scale: `diff_c * (L / pairs_c)`
/// where `L` is the least common multiple of the pair counts. Returns the scaled
/// differences and `L`.
fn scaled_case_differences(evidence: &Evidence) -> (Vec<i128>, i128) {
    let lcm = evidence
        .cases
        .iter()
        .filter(|case| case.pairs > 0)
        .fold(1u128, |acc, case| acc / gcd(acc, case.pairs as u128) * case.pairs as u128)
        as i128;
    let diffs = evidence
        .cases
        .iter()
        .filter(|case| case.pairs > 0)
        .map(|case| {
            (case.sum_candidate_bp as i128 - case.sum_baseline_bp as i128) * (lcm / case.pairs as i128)
        })
        .collect();
    (diffs, lcm)
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One-sided paired sign-flip permutation test, in parts per million, rounded up.
/// The share of sign vectors whose signed sum is at least the observed sum. Exact
/// by enumeration up to `EXACT_CASE_LIMIT` cases; a seeded Monte Carlo above it,
/// with the usual `(count + 1) / (samples + 1)` correction.
pub fn permutation_p_ppm(diffs: &[i128], samples: u32, seed: u64) -> u64 {
    if diffs.is_empty() {
        return 1_000_000;
    }
    let observed: i128 = diffs.iter().sum();
    let signed_sum = |mask: u64| -> i128 {
        diffs
            .iter()
            .enumerate()
            .map(|(i, d)| if mask >> (i % 64) & 1 == 1 { -*d } else { *d })
            .sum()
    };
    let (count, total): (u128, u128) = if diffs.len() <= EXACT_CASE_LIMIT {
        let total = 1u64 << diffs.len();
        ((0..total).filter(|mask| signed_sum(*mask) >= observed).count() as u128, total as u128)
    } else {
        let mut state = seed;
        let mut count = 1u128;
        for _ in 0..samples {
            // More than 64 cases need more than one word of sign bits.
            let sum: i128 = diffs
                .chunks(64)
                .map(|chunk| {
                    let word = splitmix64(&mut state);
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(i, d)| if word >> i & 1 == 1 { -*d } else { *d })
                        .sum::<i128>()
                })
                .sum();
            if sum >= observed {
                count += 1;
            }
        }
        (count, samples as u128 + 1)
    };
    ((count * 1_000_000 + total - 1) / total) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gates {
    pub sufficient: bool,
    pub no_case_regression: bool,
    pub cost_ok: bool,
    pub improves: bool,
}

/// `Optimization.decideGates`. The first failing gate decides.
pub fn decide_gates(mode: Mode, gates: Gates, insufficient: InconclusiveReason) -> Decision {
    if !gates.sufficient {
        Decision::Inconclusive(insufficient)
    } else if !gates.no_case_regression {
        Decision::Reject(RejectReason::CaseRegression)
    } else if !gates.cost_ok {
        Decision::Reject(RejectReason::CostRegression)
    } else if mode == Mode::Confirm || gates.improves {
        Decision::Accept
    } else {
        Decision::Reject(RejectReason::NoImprovement)
    }
}

/// A decision and the numbers behind it, for the journal and for `show`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionReport {
    pub decision: Decision,
    pub policy_version: String,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    /// Mean per-case difference in basis points, rounded toward zero.
    pub mean_diff_bp: Option<i64>,
    pub p_ppm: Option<u64>,
    pub alpha_effective_ppm: u64,
    pub cost_skipped: bool,
}

/// `seed` feeds the Monte Carlo branch only. The driver derives it from the run
/// id, so a recomputed decision is identical.
pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport {
    let (diffs, lcm) = scaled_case_differences(evidence);
    let count = |predicate: fn(&i128) -> bool| diffs.iter().filter(|d| predicate(d)).count() as u32;
    let cases = diffs.len() as i128;
    let total: i128 = diffs.iter().sum();
    let p_ppm = (mode == Mode::Improve && !diffs.is_empty())
        .then(|| permutation_p_ppm(&diffs, policy.monte_carlo_samples, seed));
    let alpha = alpha_effective_ppm(policy);
    let improves = p_ppm.is_some_and(|p| p <= alpha)
        && total >= policy.min_effect_bp as i128 * cases * lcm;
    let gates = Gates {
        sufficient: sufficient(policy, evidence),
        no_case_regression: no_case_regression(policy, evidence),
        cost_ok: evidence.tokens.as_ref().is_none_or(|tokens| cost_ok(policy, tokens)),
        improves,
    };
    let insufficient = if evidence.cases_match
        && !evidence.cases.is_empty()
        && !enough_cases(policy, evidence.cases.len())
    {
        InconclusiveReason::TooFewCases
    } else {
        InconclusiveReason::Insufficient
    };
    DecisionReport {
        decision: decide_gates(mode, gates, insufficient),
        policy_version: POLICY_VERSION.to_owned(),
        improved: count(|d| *d > 0),
        tied: count(|d| *d == 0),
        worsened: count(|d| *d < 0),
        mean_diff_bp: (cases > 0).then(|| (total / (cases * lcm)) as i64),
        p_ppm,
        alpha_effective_ppm: alpha,
        cost_skipped: evidence.tokens.is_none(),
    }
}
```

`crates/gents/src/optimization/mod.rs`:

```rust
//! Configuration optimization (#1455), a consumer of the eval core contract.
//! This module currently holds the pure promotion policy. The job record, the
//! driver and promotion follow once the eval runner exists.

pub mod policy;

pub use policy::{
    alpha_effective_ppm, decide, decide_gates, evidence_from_pairs, permutation_p_ppm, CaseEvidence,
    Decision, DecisionReport, Evidence, Gates, InconclusiveReason, Mode, PolicyV2, RejectReason,
    TokenTotals, POLICY_VERSION,
};
```

In `crates/gents/src/lib.rs` add `pub mod optimization;` after `pub mod oneshot;`.

- [ ] **Step 3: Flip the ledger entry and the structure entry**

In `crates/gents/tests/conformance/structure.rs`, change the `Optimization` entry from its `Gap` to the form the file uses for a model whose consumer is a lib unit test; if the file only has `Module(...)` for files under `tests/conformance/`, keep it a `Gap` and rewrite its rationale to name the landed consumer `optimization::policy::tests::gates_costs_and_decisions_match_lean`.

In `CoverageLedger.lean`, replace the `optimization_cases` `followUpCoverage` entry with:

```lean
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases"
      "optimization::policy::tests::gates_costs_and_decisions_match_lean")
      "optimization" [Surface.operatorCli]
```

If Task 2's contingency was used, restore the `"optimization"` feature tag and its `featureSurfaceRequirements` entry.

- [ ] **Step 4: Validate and commit**

Run:
- `RUST_MIN_STACK=16777216 cargo test -p gents --lib optimization::policy`
- `cd crates/gents/proofs && lake build`
- `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
- `cargo test -p gents && cargo check --workspace --all-targets`

Expected: all pass. If `gates_costs_and_decisions_match_lean` disagrees on a row, the Rust arithmetic differs from `Proofs/Optimization.lean`; align Rust to Lean and do not edit the expected values. If the property test finds a counterexample, the most likely cause is the improvement gate's effect-size comparison; print the two `DecisionReport`s and check `mean_diff_bp` moved in the right direction before touching the test.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): PolicyV2 with a paired sign-flip permutation gate (#1455)"
```

PR description: baseline `optimization/11-conformance`; new owner `gents::optimization::policy`; pure, no database and no eval execution; parameter defaults are uncalibrated placeholders; at `alpha_ppm = 50000` and three rounds a job needs at least six cases to ever accept, and the policy reports `too_few_cases` otherwise.

---

## Not in this plan

`OptimizationJob`, the journal persistence, the driver, `promote`, `show`, `revert` and their CLI. They depend on the eval runner's API (`gents::eval::runner`, eval spec 2), which is not designed yet. `plans/2026-09-21-optimization-substrate-3-promotion-and-live-demo.md` Tasks 1 to 3 remain a useful starting point for `promote` and the CLI once the job record exists.
