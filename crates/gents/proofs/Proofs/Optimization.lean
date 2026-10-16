import Proofs.Eval
import Mathlib.Data.List.Basic

/-! Configuration optimization (#1455), on the eval core contract.
Refinement boundary: the improvement test (a paired sign-flip permutation test
in Rust) is an abstract `Bool`; eval runs, proposer and candidate construction
are outside the fence. -/
namespace Optimization

inductive SeedSplit where
  | train | validation | heldOut
  deriving DecidableEq, Repr

structure SeedCase where
  split : SeedSplit
  seedCollections : List String
  deriving DecidableEq, Repr

structure SeedTrigger where
  taskId : String
  eventSourceId : Option String
  enabled : Bool
  deriving DecidableEq, Repr

structure SeedSource where
  eventSourceId : String
  collection : String
  eventKind : Option String
  deriving DecidableEq, Repr

def seedReachesTask (taskId collection : String) (triggers : List SeedTrigger)
    (sources : List SeedSource) : Bool :=
  triggers.any fun trigger =>
    trigger.enabled && trigger.taskId == taskId &&
      match trigger.eventSourceId with
      | none => false
      | some id => sources.any fun source =>
          source.eventSourceId == id && source.collection == collection &&
            source.eventKind.getD "created" == "created"

def splitSeedsTask (taskId : String) (split : SeedSplit) (triggers : List SeedTrigger)
    (sources : List SeedSource) (cases : List SeedCase) : Bool :=
  cases.any fun c => decide (c.split = split) &&
    c.seedCollections.any (fun collection => seedReachesTask taskId collection triggers sources)

/-- A task-template optimization needs a seed route in both development splits;
only stage seeds count (fixtures precede runtime boot), and only structural
reachability, not delivery. A context target has no seed requirement. -/
def missingTaskSeedSplits (target : Option String) (taskEnabled : Bool)
    (triggers : List SeedTrigger) (sources : List SeedSource) (cases : List SeedCase) :
    List SeedSplit :=
  match target with
  | none => []
  | some taskId => [.train, .validation].filter fun split =>
      !(taskEnabled && splitSeedsTask taskId split triggers sources cases)

theorem context_requires_no_seed (enabled : Bool) (triggers : List SeedTrigger)
    (sources : List SeedSource) (cases : List SeedCase) :
    missingTaskSeedSplits none enabled triggers sources cases = [] := rfl

theorem disabled_task_missing_both_splits (task : String) (triggers : List SeedTrigger)
    (sources : List SeedSource) (cases : List SeedCase) :
    missingTaskSeedSplits (some task) false triggers sources cases = [.train, .validation] := by
  simp [missingTaskSeedSplits]

theorem task_seed_coverage_requires_both_splits (task : String) (enabled : Bool)
    (triggers : List SeedTrigger) (sources : List SeedSource) (cases : List SeedCase) :
    missingTaskSeedSplits (some task) enabled triggers sources cases = [] ↔
      enabled = true ∧ splitSeedsTask task .train triggers sources cases = true ∧
        splitSeedsTask task .validation triggers sources cases = true := by
  cases ht : splitSeedsTask task .train triggers sources cases <;>
    cases hv : splitSeedsTask task .validation triggers sources cases <;>
    cases enabled <;> simp [missingTaskSeedSplits, ht, hv]

theorem held_out_seed_cannot_cover_train (task : String) (triggers : List SeedTrigger)
    (sources : List SeedSource) (collections : List String) :
    splitSeedsTask task .train triggers sources [⟨.heldOut, collections⟩] = false := by
  simp [splitSeedsTask]

theorem held_out_seed_cannot_cover_validation (task : String) (triggers : List SeedTrigger)
    (sources : List SeedSource) (collections : List String) :
    splitSeedsTask task .validation triggers sources [⟨.heldOut, collections⟩] = false := by
  simp [splitSeedsTask]

structure SeedRendering where
  caseId : String
  stageId : String
  collection : String
  rendered : Bool
  deriving DecidableEq, Repr

def failedTaskSeedRenders (target : Option String) (taskEnabled : Bool)
    (triggers : List SeedTrigger) (sources : List SeedSource)
    (seeds : List SeedRendering) : List SeedRendering :=
  match target with
  | none => []
  | some taskId => seeds.filter fun seed =>
      taskEnabled && seedReachesTask taskId seed.collection triggers sources && !seed.rendered

theorem context_has_no_seed_render_failures (enabled : Bool)
    (triggers : List SeedTrigger) (sources : List SeedSource) (seeds : List SeedRendering) :
    failedTaskSeedRenders none enabled triggers sources seeds = [] := rfl

theorem failed_seed_is_exact_relevant_input (task : String) (enabled : Bool)
    (triggers : List SeedTrigger) (sources : List SeedSource) (seeds : List SeedRendering)
    (seed : SeedRendering) :
    seed ∈ failedTaskSeedRenders (some task) enabled triggers sources seeds ↔
      seed ∈ seeds ∧ enabled = true ∧
        seedReachesTask task seed.collection triggers sources = true ∧ seed.rendered = false := by
  simp [failedTaskSeedRenders, Bool.and_eq_true, Bool.not_eq_true, and_assoc]

theorem admitted_seeds_all_render (task : String) (triggers : List SeedTrigger)
    (sources : List SeedSource) (seeds : List SeedRendering) (seed : SeedRendering)
    (hadmitted : failedTaskSeedRenders (some task) true triggers sources seeds = [])
    (hseed : seed ∈ seeds) (hrelevant : seedReachesTask task seed.collection triggers sources = true) :
    seed.rendered = true := by
  cases hrendered : seed.rendered
  · have hfailed := (failed_seed_is_exact_relevant_input task true triggers sources seeds seed).mpr
      ⟨hseed, rfl, hrelevant, hrendered⟩
    simp [hadmitted] at hfailed
  · rfl

inductive Arm where
  | baseline | candidate
  deriving DecidableEq, Repr

def scoreBpMax : Nat := 10000

def unknownBp : Arm → Nat
  | .baseline => scoreBpMax
  | .candidate => 0

theorem candidate_unknown_is_worst (bp : Nat) : unknownBp .candidate ≤ bp := by
  simp [unknownBp]

theorem baseline_unknown_is_best (bp : Nat) (h : bp ≤ scoreBpMax) :
    bp ≤ unknownBp .baseline := by
  simpa [unknownBp] using h

structure CasePairs where
  pairs : Nat
  sumBaseline : Nat
  sumCandidate : Nat
  deriving DecidableEq, Repr

structure Evidence where
  casesMatch : Bool
  cases : List CasePairs
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

def alphaEffectivePpm (p : Params) : Nat := p.alphaPpm / (max p.maxRounds 1)

def absDiff (a b : Nat) : Nat := (a - b) + (b - a)

def sufficient (p : Params) (e : Evidence) : Bool :=
  e.casesMatch && !e.cases.isEmpty
    && e.cases.all (fun c => decide (p.minPairs ≤ c.pairs))
    && decide (e.droppedBaseline * 10000 ≤ p.maxNotEvidenceBp * e.keys)
    && decide (e.droppedCandidate * 10000 ≤ p.maxNotEvidenceBp * e.keys)
    && decide (absDiff e.droppedBaseline e.droppedCandidate * 10000 ≤ p.maxAsymmetryBp * e.keys)
    && decide (1000000 ≤ alphaEffectivePpm p * 2 ^ e.cases.length)

def noCaseRegression (p : Params) (e : Evidence) : Bool :=
  e.cases.all fun c => decide (c.sumBaseline ≤ c.sumCandidate + p.caseToleranceBp * c.pairs)

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

/-- Accept is monotone in the gate flags: failing a gate can never manufacture
an accept. -/
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

inductive Entry where
  | frozen | runStarted | proposed | structuralReject
  | decided (d : Decision)
  | budgetExhausted | finalized
  | promoted | promotionRefused | reverted
  deriving DecidableEq, Repr

abbrev Journal := List Entry

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
