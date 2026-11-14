import Proofs.Runtime.Optimization
import Proofs.Conformance.Family

namespace Conformance.Optimization

open _root_.Optimization

deriving instance Lean.ToJson for SeedSplit, SeedCase, SeedTrigger, SeedSource, Params, CasePairs,
  Mode

def bools : List Bool := [true, false]

structure SeedCoverageScenario where
  name : String
  target : Option String := some "target"
  taskEnabled : Bool := true
  triggers : List SeedTrigger := [⟨"target", some "events", true⟩]
  sources : List SeedSource := [⟨"events", "Work", none⟩]
  cases : List SeedCase := [⟨.train, ["Work"]⟩, ⟨.validation, ["Work"]⟩]

def seedCoverageScenarios : List SeedCoverageScenario :=
  [{ name := "both_splits_seed_target" },
   { name := "context_prompt_only", target := none, taskEnabled := false,
     triggers := [], sources := [], cases := [⟨.train, []⟩, ⟨.validation, []⟩] },
   { name := "task_prompt_only", cases := [⟨.train, []⟩, ⟨.validation, []⟩] },
   { name := "only_train_seeds", cases := [⟨.train, ["Work"]⟩, ⟨.validation, []⟩] },
   { name := "only_validation_seeds", cases := [⟨.train, []⟩, ⟨.validation, ["Work"]⟩] },
   { name := "only_held_out_seeds", cases := [⟨.heldOut, ["Work"]⟩] },
   { name := "wrong_collection", cases := [⟨.train, ["Other"]⟩, ⟨.validation, ["Other"]⟩] },
   { name := "wrong_task", triggers := [⟨"other", some "events", true⟩] },
   { name := "disabled_task", taskEnabled := false },
   { name := "disabled_trigger", triggers := [⟨"target", some "events", false⟩] },
   { name := "schedule_trigger", triggers := [⟨"target", none, true⟩] },
   { name := "missing_source", sources := [] },
   { name := "unreferenced_source", sources := [⟨"other", "Work", none⟩] },
   { name := "explicit_created", sources := [⟨"events", "Work", some "created"⟩] },
   { name := "not_a_created_event", sources := [⟨"events", "Work", some "updated"⟩] },
   { name := "later_matching_seed", cases := [⟨.train, ["Other", "Work"]⟩,
       ⟨.validation, ["Other", "Work"]⟩] },
   { name := "one_matching_case_per_split", cases := [⟨.train, []⟩, ⟨.train, ["Work"]⟩,
       ⟨.validation, []⟩, ⟨.validation, ["Work"]⟩] },
   { name := "unrelated_triggers_do_not_hide_route", triggers :=
       [⟨"other", some "events", true⟩, ⟨"target", some "events", false⟩,
        ⟨"target", some "events", true⟩] }]

structure ExactSeed where
  caseId : String := "case"
  stageId : String := "seed"
  collection : String := "PlanItem"
  document : String := "{\"goal\":\"ship\",\"owner\":\"alex\"}"
  rendered : Bool := true
  deriving Lean.ToJson

def ExactSeed.rendering (seed : ExactSeed) : SeedRendering :=
  ⟨seed.caseId, seed.stageId, seed.collection, seed.rendered⟩

structure SeedRenderingScenario where
  name : String
  target : Option String := some "plan"
  taskEnabled : Bool := true
  triggers : List SeedTrigger := [⟨"plan", some "plan-source", true⟩]
  sources : List SeedSource := [⟨"plan-source", "PlanItem", none⟩]
  text : String := "Plan {{ doc.goal }} for {{ doc.owner }}."
  seeds : List ExactSeed := [{}]

def seedRenderingScenarios : List SeedRenderingScenario :=
  [{ name := "all_exact_seeds_render" },
   { name := "missing_baseline_field", seeds :=
       [{ document := "{\"goal\":\"ship\"}", rendered := false }] },
   { name := "missing_added_field", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }} using {{ doc.missing }}.",
       seeds := [{ rendered := false }] },
   { name := "nested_missing_field", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }} using {{ doc.detail.name }}.",
       seeds := [{ document := "{\"goal\":\"ship\",\"owner\":\"alex\",\"detail\":{}}",
                   rendered := false }] },
   { name := "one_of_multiple_seeds_fails", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }} using {{ doc.detail }}.", seeds :=
       [{ caseId := "first", document := "{\"goal\":\"ship\",\"owner\":\"alex\",\"detail\":\"ready\"}" },
        { caseId := "second", rendered := false }] },
   { name := "added_field_present_in_every_seed", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }} using {{ doc.detail }}.", seeds :=
       [{ caseId := "first", document := "{\"goal\":\"ship\",\"owner\":\"alex\",\"detail\":\"ready\"}" },
        { caseId := "second", document := "{\"goal\":\"test\",\"owner\":\"sam\",\"detail\":\"complete\"}" }] },
   { name := "guarded_absent_field_renders", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }}{% if doc.detail is defined %}: {{ doc.detail }}{% endif %}." },
   { name := "runtime_document_id_is_available", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }} ({{ doc._docID }})." },
   { name := "trial_owner_binding_reaches_the_guarded_missing_field", text :=
       "Plan {{ doc.goal }} for {{ doc.owner }}{% if doc.node_did == node.node_did %}: {{ doc.detail }}{% endif %}.",
       seeds := [{ document := "{\"goal\":\"ship\",\"owner\":\"alex\",\"node_did\":\"$trial\"}", rendered := false }] },
   { name := "context_is_not_a_template", target := none,
       text := "Plan {{ doc.missing }}.", seeds := [{ rendered := false }] },
   { name := "unrelated_seed_is_not_rendered", seeds :=
       [{}, { collection := "Other", document := "{}", rendered := false }] },
   { name := "disabled_task_has_no_seed_rendering", taskEnabled := false,
       seeds := [{ document := "{}", rendered := false }] },
   { name := "disabled_trigger_has_no_seed_rendering", triggers :=
       [⟨"plan", some "plan-source", false⟩], seeds := [{ document := "{}", rendered := false }] },
   { name := "schedule_is_not_a_seed_route", triggers := [⟨"plan", none, true⟩],
       seeds := [{ document := "{}", rendered := false }] },
   { name := "updated_source_is_not_a_created_seed_route", sources :=
       [⟨"plan-source", "PlanItem", some "updated"⟩], seeds := [{ document := "{}", rendered := false }] }]

def params : Params :=
  { minPairs := 2, maxNotEvidenceBp := 2000, maxAsymmetryBp := 1000, caseToleranceBp := 3000,
    alphaPpm := 50000, maxRounds := 3, maxTokenIncreaseBp := 2500 }

def c (n b k : Nat) : CasePairs := ⟨n, b, k⟩

def six : List CasePairs :=
  [c 2 10000 16000, c 2 8000 14000, c 2 12000 18000, c 2 6000 12000, c 2 10000 15000, c 2 9000 16000]

def gateScenarios : List (String × Evidence) :=
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

def costScenarios : List (String × Nat × Nat × Nat × Nat) :=
  [("same_cost", 1000, 10, 1000, 10),
   ("at_the_limit", 1000, 10, 1250, 10),
   ("just_over_the_limit", 1000, 10, 1251, 10),
   ("cheaper", 1000, 10, 600, 10),
   ("unequal_trial_counts_within_limit", 1000, 10, 600, 5),
   ("unequal_trial_counts_over_limit", 1000, 10, 700, 5)]

inductive DecisionWire where
  | accept | inconclusive | rejectCaseRegression | rejectCostRegression | rejectNoImprovement
  deriving Lean.ToJson

def DecisionWire.ofDecision : Decision → DecisionWire
  | .accept => .accept
  | .inconclusive => .inconclusive
  | .reject .caseRegression => .rejectCaseRegression
  | .reject .costRegression => .rejectCostRegression
  | .reject .noImprovement => .rejectNoImprovement

@[conformance_family]
def alphaEffective : Conformance.Family Params Nat where
  inputs := [params]
  model := alphaEffectivePpm

structure GateInput where
  name : String
  params : Params
  casesMatch : Bool
  cases : List CasePairs
  keys : Nat
  droppedBaseline : Nat
  droppedCandidate : Nat
  deriving Lean.ToJson

def GateInput.evidence (i : GateInput) : Evidence :=
  ⟨i.casesMatch, i.cases, i.keys, i.droppedBaseline, i.droppedCandidate⟩

structure GateOutput where
  sufficient : Bool
  noCaseRegression : Bool
  deriving Lean.ToJson

@[conformance_family]
def gate : Conformance.Family GateInput GateOutput where
  inputs := gateScenarios.map fun (name, e) =>
    { name, params, casesMatch := e.casesMatch, cases := e.cases, keys := e.keys
      droppedBaseline := e.droppedBaseline, droppedCandidate := e.droppedCandidate }
  model := fun i =>
    { sufficient := sufficient i.params i.evidence
      noCaseRegression := noCaseRegression i.params i.evidence }

structure CostInput where
  name : String
  params : Params
  baselineTokens : Nat
  baselineTrials : Nat
  candidateTokens : Nat
  candidateTrials : Nat
  deriving Lean.ToJson

@[conformance_family]
def cost : Conformance.Family CostInput Bool where
  inputs := costScenarios.map fun (name, bt, bn, ct, cn) =>
    { name, params, baselineTokens := bt, baselineTrials := bn, candidateTokens := ct
      candidateTrials := cn }
  model := fun i =>
    costOk i.params i.baselineTokens i.baselineTrials i.candidateTokens i.candidateTrials

structure DecisionInput where
  mode : Mode
  sufficient : Bool
  noCaseRegression : Bool
  costOk : Bool
  improves : Bool
  deriving Lean.ToJson

@[conformance_family]
def decision : Conformance.Family DecisionInput DecisionWire where
  inputs := [Mode.improve, Mode.confirm].flatMap fun mode =>
    bools.flatMap fun sufficient => bools.flatMap fun noCaseRegression =>
      bools.flatMap fun costOk => bools.map fun improves =>
        { mode, sufficient, noCaseRegression, costOk, improves }
  model := fun i =>
    DecisionWire.ofDecision (decideGates i.mode i.sufficient i.noCaseRegression i.costOk i.improves)

structure SeedCoverageInput where
  name : String
  targetTask : Option String
  taskEnabled : Bool
  triggers : List SeedTrigger
  sources : List SeedSource
  cases : List SeedCase
  deriving Lean.ToJson

def SeedCoverageInput.ofScenario (s : SeedCoverageScenario) : SeedCoverageInput :=
  { name := s.name, targetTask := s.target, taskEnabled := s.taskEnabled, triggers := s.triggers
    sources := s.sources, cases := s.cases }

def seedCoverageMissing (i : SeedCoverageInput) : List SeedSplit :=
  missingTaskSeedSplits i.targetTask i.taskEnabled i.triggers i.sources i.cases

@[conformance_family]
def seedCoverage : Conformance.Family SeedCoverageInput (List SeedSplit) where
  inputs := seedCoverageScenarios.map SeedCoverageInput.ofScenario
  model := seedCoverageMissing

/-- The optimization driver refuses a task target before any job exists only when a
split lacks seeds; these are the scenarios whose task definition the driver harness
installs (one seeded stage per seeded split of the shared validation cases). -/
def refusedCoverageNames : List String :=
  ["task_prompt_only", "only_train_seeds", "only_validation_seeds", "only_held_out_seeds"]

@[conformance_family]
def seedCoverageRefused : Conformance.Family SeedCoverageInput (List SeedSplit) where
  inputs := (seedCoverageScenarios.filter fun s => s.name ∈ refusedCoverageNames).map
    SeedCoverageInput.ofScenario
  model := seedCoverageMissing

structure SeedRenderingInput where
  name : String
  targetTask : Option String
  taskEnabled : Bool
  triggers : List SeedTrigger
  sources : List SeedSource
  text : String
  seeds : List ExactSeed
  deriving Lean.ToJson

structure StageRef where
  caseId : String
  stageId : String
  deriving Lean.ToJson

def SeedRenderingInput.ofScenario (s : SeedRenderingScenario) : SeedRenderingInput :=
  { name := s.name, targetTask := s.target, taskEnabled := s.taskEnabled, triggers := s.triggers
    sources := s.sources, text := s.text, seeds := s.seeds }

def seedRenderingFailures (i : SeedRenderingInput) : List StageRef :=
  (failedTaskSeedRenders i.targetTask i.taskEnabled i.triggers i.sources
    (i.seeds.map ExactSeed.rendering)).map fun seed => { caseId := seed.caseId, stageId := seed.stageId }

@[conformance_family]
def seedRendering : Conformance.Family SeedRenderingInput (List StageRef) where
  inputs := seedRenderingScenarios.map SeedRenderingInput.ofScenario
  model := seedRenderingFailures

/-- The candidate scenario installs a task target whose `plan` route reads `PlanItem` and
proposes the scenario's text; only scenarios with that route and the task enabled can
reach the candidate seed gate. -/
def candidateRenderingNames : List String :=
  ["missing_added_field", "nested_missing_field", "one_of_multiple_seeds_fails",
   "added_field_present_in_every_seed", "guarded_absent_field_renders",
   "runtime_document_id_is_available", "trial_owner_binding_reaches_the_guarded_missing_field"]

structure CandidateOutput where
  failed : Bool
  deriving Lean.ToJson

@[conformance_family]
def seedRenderingCandidate : Conformance.Family SeedRenderingInput CandidateOutput where
  inputs := (seedRenderingScenarios.filter fun s => s.name ∈ candidateRenderingNames).map
    SeedRenderingInput.ofScenario
  model := fun i => { failed := !(seedRenderingFailures i).isEmpty }

end Conformance.Optimization
