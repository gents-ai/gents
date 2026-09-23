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

/-- Accept is monotone in the gate flags: if a stricter assignment accepts, so does every
weaker one; equivalently, failing a gate can never manufacture an accept. -/
theorem accept_monotone (mode : Mode) (s r c i s' r' c' i' : Bool)
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
