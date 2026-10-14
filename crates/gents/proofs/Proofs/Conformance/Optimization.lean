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

private def splitJson : SeedSplit → String
  | .train => "\"train\"" | .validation => "\"validation\"" | .heldOut => "\"held_out\""

private def optionalStringJson : Option String → String
  | none => "null" | some value => jsonString value

private def seedTriggerJson (trigger : SeedTrigger) : String :=
  "{\"task_id\":" ++ jsonString trigger.taskId
    ++ ",\"event_source_id\":" ++ optionalStringJson trigger.eventSourceId
    ++ ",\"enabled\":" ++ boolJson trigger.enabled ++ "}"

private def seedSourceJson (source : SeedSource) : String :=
  "{\"event_source_id\":" ++ jsonString source.eventSourceId
    ++ ",\"source_collection\":" ++ jsonString source.collection
    ++ ",\"event_kind\":" ++ optionalStringJson source.eventKind ++ "}"

private def seedCaseJson (c : SeedCase) : String :=
  "{\"split\":" ++ splitJson c.split
    ++ ",\"seed_collections\":" ++ jsonArray (c.seedCollections.map jsonString) ++ "}"

private structure SeedCoverageScenario where
  name : String
  target : Option String := some "target"
  taskEnabled : Bool := true
  triggers : List SeedTrigger := [⟨"target", some "events", true⟩]
  sources : List SeedSource := [⟨"events", "Work", none⟩]
  cases : List SeedCase := [⟨.train, ["Work"]⟩, ⟨.validation, ["Work"]⟩]

private def seedCoverageScenarios : List SeedCoverageScenario :=
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

private def seedCoverageRow (s : SeedCoverageScenario) : String :=
  "{\"name\":" ++ jsonString s.name
    ++ ",\"target_task\":" ++ optionalStringJson s.target
    ++ ",\"task_enabled\":" ++ boolJson s.taskEnabled
    ++ ",\"triggers\":" ++ jsonArray (s.triggers.map seedTriggerJson)
    ++ ",\"sources\":" ++ jsonArray (s.sources.map seedSourceJson)
    ++ ",\"cases\":" ++ jsonArray (s.cases.map seedCaseJson)
    ++ ",\"missing_splits\":" ++ jsonArray
      ((missingTaskSeedSplits s.target s.taskEnabled s.triggers s.sources s.cases).map splitJson)
    ++ "}"

private def decisionRows : List String :=
  [Mode.improve, Mode.confirm].flatMap fun m =>
    bools.flatMap fun s => bools.flatMap fun r => bools.flatMap fun c => bools.map fun i =>
      "{\"mode\":" ++ modeJson m ++ ",\"sufficient\":" ++ boolJson s
        ++ ",\"no_case_regression\":" ++ boolJson r ++ ",\"cost_ok\":" ++ boolJson c
        ++ ",\"improves\":" ++ boolJson i
        ++ ",\"decision\":" ++ decisionJson (decideGates m s r c i) ++ "}"

private structure ExactSeed where
  caseId : String := "case"
  stageId : String := "seed"
  collection : String := "PlanItem"
  document : String := "{\"goal\":\"ship\",\"owner\":\"alex\"}"
  rendered : Bool := true

private def ExactSeed.rendering (seed : ExactSeed) : SeedRendering :=
  ⟨seed.caseId, seed.stageId, seed.collection, seed.rendered⟩

private structure SeedRenderingScenario where
  name : String
  target : Option String := some "plan"
  taskEnabled : Bool := true
  triggers : List SeedTrigger := [⟨"plan", some "plan-source", true⟩]
  sources : List SeedSource := [⟨"plan-source", "PlanItem", none⟩]
  text : String := "Plan {{ doc.goal }} for {{ doc.owner }}."
  seeds : List ExactSeed := [{}]

private def seedRenderingScenarios : List SeedRenderingScenario :=
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

private def exactSeedJson (seed : ExactSeed) : String :=
  "{\"case_id\":" ++ jsonString seed.caseId
    ++ ",\"stage_id\":" ++ jsonString seed.stageId
    ++ ",\"collection\":" ++ jsonString seed.collection
    ++ ",\"document\":" ++ seed.document
    ++ ",\"rendered\":" ++ boolJson seed.rendered ++ "}"

private def seedRenderingRow (s : SeedRenderingScenario) : String :=
  let failures := failedTaskSeedRenders s.target s.taskEnabled s.triggers s.sources
    (s.seeds.map ExactSeed.rendering)
  "{\"name\":" ++ jsonString s.name
    ++ ",\"target_task\":" ++ optionalStringJson s.target
    ++ ",\"task_enabled\":" ++ boolJson s.taskEnabled
    ++ ",\"triggers\":" ++ jsonArray (s.triggers.map seedTriggerJson)
    ++ ",\"sources\":" ++ jsonArray (s.sources.map seedSourceJson)
    ++ ",\"text\":" ++ jsonString s.text
    ++ ",\"seeds\":" ++ jsonArray (s.seeds.map exactSeedJson)
    ++ ",\"failed_stages\":" ++ jsonArray (failures.map fun seed =>
      "[" ++ jsonString seed.caseId ++ "," ++ jsonString seed.stageId ++ "]") ++ "}"

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
    ++ ",\"costs\":" ++ jsonArray (costScenarios.map costRow)
    ++ ",\"seed_coverage\":" ++ jsonArray (seedCoverageScenarios.map seedCoverageRow)
    ++ ",\"seed_rendering\":" ++ jsonArray (seedRenderingScenarios.map seedRenderingRow) ++ "}"

end Conformance.Optimization
