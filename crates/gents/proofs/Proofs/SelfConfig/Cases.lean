import Proofs.SelfConfig.Theorems

namespace SelfConfig.ContractCases

open SelfConfig

structure CaseRow where
  name : String
  target : Target
  guarded : Bool
  validates : Bool
  doc : List (FieldKey × FieldValue)
  patch : List (FieldKey × Option FieldValue)
  deriving Repr

def rowPatch (r : CaseRow) : Patch :=
  r.patch.map (fun e =>
    { key := e.1
    , op := match e.2 with
        | some v => PatchOp.set v
        | none => PatchOp.clear })

def caseGuard (r : CaseRow) : Doc → Bool :=
  if r.guarded then gateOn else fun _ => true

def project (t : Target) (doc : Doc) : List (FieldKey × FieldValue) :=
  (allFields t).filterMap (fun k => (doc k).map (fun v => (k, v)))

structure CaseWitness where
  row : CaseRow
  admissiblePatch : Bool
  accepted : Bool
  result : List (FieldKey × FieldValue)
  protectedPreserved : Bool
  containmentHolds : Bool
  unchangedOnReject : Bool
  gateOnAfterAccept : Bool
  deriving Repr

def buildWitness (r : CaseRow) : CaseWitness :=
  let stored : Doc := Doc.ofList r.doc
  let patch := rowPatch r
  let outcome := step (fun _ => r.validates) (caseGuard r) r.target stored patch
  let result := outcome.getD stored
  let merged := applyPatch r.target stored patch
  { row := r
  , admissiblePatch := admissible r.target patch
  , accepted := outcome.isSome
  , result := project r.target result
  , protectedPreserved := (protectedFields r.target).all
      (fun k => decide (result k = stored k))
  , containmentHolds := (allFields r.target).all
      (fun k =>
        decide (merged k = stored k)
          || ((writableFields r.target).contains k
              && patch.any (fun e => e.key == k)))
  , unchangedOnReject :=
      outcome.isSome || decide (project r.target result = project r.target stored)
  , gateOnAfterAccept :=
      !(r.guarded && outcome.isSome) || gateOn result
  }

def behaviorDoc : List (FieldKey × FieldValue) :=
  [ ("behavior_id", "beh-1"), ("agent_did", "did:key:agent-a")
  , ("system_prompt", "You are concise."), ("backend_id", "backend-local")
  , ("model_name", "m-small"), ("inference_profile_id", "profile-default")
  , ("enabled", "true"), ("created_at", "2026-01-01T00:00:00Z") ]

def selectionDoc : List (FieldKey × FieldValue) :=
  [ ("selection_id", "sel-1"), ("agent_did", "did:key:agent-a")
  , ("tool_policy_version", "v1"), ("enable_defra_query", "false")
  , ("enable_self_config", "true"), ("enable_bash", "false") ]

def profileDoc : List (FieldKey × FieldValue) :=
  [ ("profile_id", "profile-default"), ("temperature", "0.7")
  , ("max_turns", "40"), ("deadline_duration_secs", "600") ]

def backendDoc : List (FieldKey × FieldValue) :=
  [ ("backend_id", "backend-local"), ("name", "local")
  , ("endpoint", "http://127.0.0.1:11434/v1"), ("api_key", "sk-secret")
  , ("probe_status", "healthy"), ("enabled", "true") ]

def serviceDoc : List (FieldKey × FieldValue) :=
  [ ("service_id", "svc-1"), ("hostname", "tools.local")
  , ("mcp_port", "8931"), ("status", "online") ]

def taskDoc : List (FieldKey × FieldValue) :=
  [ ("task_id", "task-1"), ("behavior_id", "beh-1")
  , ("prompt_template", "Summarize {{doc}}"), ("enabled", "true") ]

def scheduleDoc : List (FieldKey × FieldValue) :=
  [ ("schedule_id", "sched-1"), ("task_id", "task-1")
  , ("interval_secs", "3600"), ("enabled", "true"), ("fire_count", "12")
  , ("next_run_at", "2026-01-02T00:00:00Z") ]

def triggerDoc : List (FieldKey × FieldValue) :=
  [ ("trigger_id", "trig-1"), ("task_id", "task-1")
  , ("source_collection", "AgentRequest"), ("event_kind", "create")
  , ("enabled", "false"), ("fire_count", "3") ]

def scenarios : List CaseRow :=
  [ { name := "behavior_prompt_and_model_patch_accepted"
    , target := .agentBehavior, guarded := false, validates := true
    , doc := behaviorDoc
    , patch := [ ("system_prompt", some "You are thorough.")
               , ("model_name", some "m-large") ] }
  , { name := "behavior_agent_did_patch_inadmissible"
    , target := .agentBehavior, guarded := false, validates := true
    , doc := behaviorDoc
    , patch := [ ("agent_did", some "did:key:attacker")
               , ("system_prompt", some "hijacked") ] }
  , { name := "behavior_unique_key_patch_inadmissible"
    , target := .agentBehavior, guarded := false, validates := true
    , doc := behaviorDoc
    , patch := [ ("behavior_id", some "beh-2") ] }
  , { name := "behavior_validation_failure_rejects_wholesale"
    , target := .agentBehavior, guarded := false, validates := false
    , doc := behaviorDoc
    , patch := [ ("backend_id", some "backend-missing") ] }
  , { name := "tools_gate_patch_accepted"
    , target := .toolSelection, guarded := false, validates := true
    , doc := selectionDoc
    , patch := [ ("enable_defra_query", some "true")
               , ("defra_query_collections", some "agent-config") ] }
  , { name := "tools_policy_version_patch_inadmissible"
    , target := .toolSelection, guarded := false, validates := true
    , doc := selectionDoc
    , patch := [ ("tool_policy_version", some "v2") ] }
  , { name := "tools_self_disable_unguarded_accepted"
    , target := .toolSelection, guarded := false, validates := true
    , doc := selectionDoc
    , patch := [ ("enable_self_config", some "false") ] }
  , { name := "tools_self_disable_no_lockout_rejected"
    , target := .toolSelection, guarded := true, validates := true
    , doc := selectionDoc
    , patch := [ ("enable_self_config", some "false") ] }
  , { name := "tools_guarded_unrelated_patch_accepted"
    , target := .toolSelection, guarded := true, validates := true
    , doc := selectionDoc
    , patch := [ ("enable_bash", some "true")
               , ("bash_mode", some "read_only") ] }
  , { name := "profile_set_and_clear_accepted"
    , target := .inferenceProfile, guarded := false, validates := true
    , doc := profileDoc
    , patch := [ ("temperature", some "0.2")
               , ("deadline_duration_secs", none) ] }
  , { name := "backend_endpoint_patch_accepted"
    , target := .inferenceBackend, guarded := false, validates := true
    , doc := backendDoc
    , patch := [ ("endpoint", some "http://127.0.0.1:8080/v1")
               , ("api_key_env_var", some "LOCAL_KEY") ] }
  , { name := "backend_api_key_patch_inadmissible"
    , target := .inferenceBackend, guarded := false, validates := true
    , doc := backendDoc
    , patch := [ ("api_key", some "sk-stolen") ] }
  , { name := "backend_probe_status_patch_inadmissible"
    , target := .inferenceBackend, guarded := false, validates := true
    , doc := backendDoc
    , patch := [ ("probe_status", some "healthy")
               , ("endpoint", some "http://127.0.0.1:8080/v1") ] }
  , { name := "mcp_service_endpoint_patch_accepted"
    , target := .toolServiceRegistry, guarded := false, validates := true
    , doc := serviceDoc
    , patch := [ ("hostname", some "tools2.local")
               , ("mcp_port", some "9000") ] }
  , { name := "task_prompt_patch_accepted"
    , target := .task, guarded := false, validates := true
    , doc := taskDoc
    , patch := [ ("prompt_template", some "Review {{doc}} carefully")
               , ("description", some "nightly review") ] }
  , { name := "task_behavior_link_patch_inadmissible"
    , target := .task, guarded := false, validates := true
    , doc := taskDoc
    , patch := [ ("behavior_id", some "beh-other") ] }
  , { name := "schedule_cadence_patch_accepted"
    , target := .schedule, guarded := false, validates := true
    , doc := scheduleDoc
    , patch := [ ("interval_secs", none), ("cron", some "0 3 * * *")
               , ("timezone", some "UTC") ] }
  , { name := "schedule_runtime_fields_patch_inadmissible"
    , target := .schedule, guarded := false, validates := true
    , doc := scheduleDoc
    , patch := [ ("fire_count", some "0")
               , ("next_run_at", some "2030-01-01T00:00:00Z") ] }
  , { name := "event_trigger_filter_patch_accepted"
    , target := .eventTrigger, guarded := false, validates := true
    , doc := triggerDoc
    , patch := [ ("filter", some "{\"status\":\"failed\"}")
               , ("enabled", some "true") ] }
  ]

def selfConfigCases : List CaseWitness :=
  scenarios.map buildWitness

theorem self_config_cases_witness_theorems :
    selfConfigCases.all (fun w =>
      w.protectedPreserved && w.containmentHolds && w.unchangedOnReject
        && w.gateOnAfterAccept) = true := by
  native_decide

theorem self_config_cases_cover_rejections :
    (selfConfigCases.any (fun w => !w.admissiblePatch && !w.accepted))
      && (selfConfigCases.any (fun w =>
            w.admissiblePatch && !w.row.validates && !w.accepted))
      && (selfConfigCases.any (fun w =>
            w.row.guarded && !w.accepted && w.admissiblePatch
              && w.row.validates)) = true := by
  native_decide

theorem self_config_cases_cover_all_targets :
    allTargets.all (fun t =>
      selfConfigCases.any (fun w => decide (w.row.target = t))) = true := by
  native_decide

end SelfConfig.ContractCases
