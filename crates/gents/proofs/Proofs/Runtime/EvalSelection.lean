import Lean

namespace Runtime.EvalSelection
open Lean

structure Verdict where
  verdictId : String
  stageIndex : Nat
  check : String
  tier : String
  kind : String
  providerReason : Option String
  scoreBp : Option Nat
  weight : Nat
  regradeOf : Option String
  deriving BEq, ToJson

/-- Regrading removes the named verdict before grouping. Concurrent surviving
regrades are resolved by verdict ID, so database row order cannot choose a winner. -/
def select (rows : List Verdict) : List Verdict :=
  let superseded := rows.filterMap (·.regradeOf)
  let live := rows.filter fun row => !superseded.contains row.verdictId
  let winners := live.filter fun row => !(live.any fun other =>
    other.stageIndex == row.stageIndex && other.check == row.check && row.verdictId < other.verdictId)
  let unique := winners.foldl (fun selected row =>
    if selected.any (fun old => old.stageIndex == row.stageIndex && old.check == row.check)
    then selected else selected ++ [row]) []
  unique.mergeSort fun a b =>
    if a.stageIndex == b.stageIndex then a.check ≤ b.check else a.stageIndex ≤ b.stageIndex

structure VerdictInput where
  rows : List Verdict
  deriving ToJson

structure Trial where
  caseId : String
  trialIndex : Nat
  classification : String
  score : Nat
  deriving BEq, ToJson

structure Pair where
  caseId : String
  trialIndex : Nat
  baselineBp : Nat
  candidateBp : Nat
  deriving ToJson

structure Paired where
  pairs : List Pair
  keys : Nat
  droppedBaseline : Nat
  droppedCandidate : Nat
  deriving ToJson

structure PairInput where
  baseline : List Trial
  candidate : List Trial
  deriving ToJson

def sameKey (a b : Trial) : Bool := a.caseId == b.caseId && a.trialIndex == b.trialIndex

/-- Repeated cell keys are unknown, even when the duplicate is not evidence;
unknown evidence is imputed against the candidate. Missing/excluded cells are dropped. -/
def side (rows : List Trial) (key : Trial) (unknown : Nat) : Option Nat :=
  match rows.filter (sameKey key) with
  | [] => none
  | [row] => if row.classification == "scored" then some (min 10000 row.score)
      else if row.classification == "unknown" then some unknown else none
  | _ => some unknown

def pair (input : PairInput) : Paired := Id.run do
  let keys := (input.baseline ++ input.candidate).foldl (fun keys row =>
    if keys.any (sameKey row) then keys else row :: keys) []
  let keys := keys.mergeSort fun a b =>
    if a.caseId == b.caseId then a.trialIndex ≤ b.trialIndex else a.caseId ≤ b.caseId
  let mut result : Paired := ⟨[], keys.length, 0, 0⟩
  for key in keys do
    let baseline := side input.baseline key 10000
    let candidate := side input.candidate key 0
    match baseline, candidate with
    | some b, some c => result := { result with pairs := result.pairs ++ [⟨key.caseId, key.trialIndex, b, c⟩] }
    | b, c => result := { result with
        droppedBaseline := result.droppedBaseline + if b.isNone then 1 else 0
        droppedCandidate := result.droppedCandidate + if c.isNone then 1 else 0 }
  return result

end Runtime.EvalSelection
