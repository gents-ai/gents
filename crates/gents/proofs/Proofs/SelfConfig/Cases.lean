import Proofs.SelfConfig.Theorems
import Proofs.SelfConfig.Auth

namespace SelfConfig.ContractCases

open SelfConfig

structure CaseRow where
  name : String
  target : Target
  guarded : Bool
  validates : Bool
  doc : List (FieldKey × FieldValue)
  patch : List (FieldKey × Option FieldValue)
  /-- Backends a profile row can select: (`backend_id` JSON text, provider
  kind, auth JSON text). -/
  backends : List (String × String × String) := []
  deriving Repr

def rowPatch (r : CaseRow) : Patch :=
  r.patch.map (fun e =>
    { key := e.1
    , op := match e.2 with
        | some v => PatchOp.set v
        | none => PatchOp.clear })

/-- Fixture decoder for the canonical nested values used below; absent groups
decode as disabled. Production uses the shared typed decoder, not this finite
fixture table. -/
def decodeControl (doc : Doc) : Option Control := do
  let (selfConfig, noLockout, toolsAuthority) ← match doc "self_config" with
    | some "{\"enable_self_config\":true}" => some (true, false, true)
    | some "{\"enable_self_config\":false}" => some (false, false, true)
    | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true}" =>
        some (true, true, true)
    | some "{\"enable_self_config\":true,\"self_config_no_lockout\":false}" =>
        some (true, false, true)
    | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"self_config_categories\":[\"profile\"]}" =>
        some (true, true, false)
    | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"self_config_categories\":[\"tools\"]}" =>
        some (true, true, true)
    | none => some (false, false, true)
    | _ => none
  let agents ← match doc "agents" with
    | some "{\"enabled\":true}" => some true
    | some "{\"enabled\":false}" => some false
    | none => some false
    | _ => none
  pure { selfConfig, agents, noLockout, toolsAuthority }

/-- Fixture decoder for the agent values used below. -/
def decodeReach (doc : Doc) : Option Reach := do
  let enabled ← match doc "enabled" with
    | some "true" | none => some true
    | some "false" => some false
    | _ => none
  let setupTag ← match doc "tags" with
    | some "[\"gents:setup-steward\"]" => some true
    | some "[\"gents:setup-steward\",\"ui:engineer\"]" => some true
    | some "[]" | some "[\"ui:engineer\"]" | none => some false
    | _ => none
  pure { enabled, setupTag }

/-- Fixture decoder for the exact backend auth texts used below. -/
def decodeAuthText : String → Option Configuration.BackendAuth
  | "{\"kind\":\"environment\",\"variable\":\"KEY\"}" => some (.environment "KEY")
  | "{\"kind\":\"principal_oauth\"}" => some (.principalOAuth none)
  | "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}" => some (.principalOAuth (some "a1"))
  | "{\"kind\":\"principal_oauth\",\"account_ref\":\"a2\"}" => some (.principalOAuth (some "a2"))
  | "{\"kind\":\"principal_oauth\",\"account_ref\":\"g2\"}" => some (.principalOAuth (some "g2"))
  | _ => none

def decodeAuth (doc : Doc) : Option Configuration.BackendAuth :=
  (doc "auth").bind decodeAuthText

/-- The invoker-only no-lockout slice for Tools and Agent targets. -/
def lockoutGuard (t : Target) (stored : Doc) : Doc → Bool :=
  if t = .agent then keepsReach decodeReach stored
  else keepsControl decodeControl stored

def rowBackendOf (r : CaseRow) (id : String) : Option (String × Configuration.BackendAuth) :=
  (r.backends.find? (·.1 = id)).bind fun (_, kind, auth) => (decodeAuthText auth).map (kind, ·)

/-- A guarded row replays its target's typed guard: the no-lockout slice for
Tools and Agent, the auth fence for Backend and the account choice fence
for Profile (default = the original account), which Rust enforces in
`validate` on every model write rather than only under no-lockout. -/
def caseGuard (r : CaseRow) (stored : Doc) : Doc → Bool :=
  if !r.guarded then fun _ => true
  else if r.target = .inferenceBackend then authGuard decodeAuth stored
  else if r.target = .inferenceProfile then profileGuard (rowBackendOf r) (fun _ => none) stored
  else lockoutGuard r.target stored

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
  controlKeptAfterAccept : Bool
  deriving Repr

def buildWitness (r : CaseRow) : CaseWitness :=
  let stored : Doc := Doc.ofList r.doc
  let patch := rowPatch r
  let outcome := step (fun _ => r.validates) (caseGuard r stored) r.target stored patch
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
  , controlKeptAfterAccept :=
      !(r.guarded && outcome.isSome) || caseGuard r stored result
  }

/-- Values are decoded group values abstracted as strings; nested validation
is supplied to `step`, using the same owner as ordinary configuration. -/
def examples : List (Target × FieldKey × FieldValue) :=
  [ (.agent, "context_id", "context-1")
  , (.agentContext, "system_prompt", "You are concise.")
  , (.compaction, "threshold", "0.75")
  , (.tools, "host", "{root: /workspace}")
  , (.agentTarget, "agent_id", "gatekeeper")
  , (.skill, "instructions", "Read the checklist before reviewing.")
  , (.datastoreToolSurface, "entries", "[{tool_name: submit_job, collection: Job}]")
  , (.inferenceProfile, "model_name", "model-1")
  , (.inferenceSampling, "temperature", "0.2")
  , (.inferenceExecution, "max_turns", "40")
  , (.inferenceRetryPolicy, "max_transport_retries", "3")
  , (.inferenceBackend, "endpoint", "http://localhost:8080/v1")
  , (.toolServiceRegistry, "hostname", "tools.local")
  , (.task, "prompt_template", "Review {{doc}}")
  , (.schedule, "cadence", "{interval_secs: 3600}")
  , (.trigger, "source", "{kind: schedule, schedule_id: schedule-1}")
  , (.eventSource, "group", "{expected_count: {kind: fixed, count: 3}}") ]

def examplesToRows : List CaseRow := examples.map fun (t, k, v) =>
  { name := t.collectionName ++ "_configured_field_accepted"
  , target := t, guarded := false, validates := true
  , doc := [(t.uniqueField, "doc-1"), ("node_did", "did:key:agent-a")]
  , patch := [(k, some v)] }

/-- One provider with two accounts, another with its original and a second
account, and one account-free backend. -/
def profileBackends : List (String × String × String) :=
  [ ("\"chat-a1\"", "ChatGptCodex", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a1-alt\"", "ChatGptCodex", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a2\"", "ChatGptCodex", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a2\"}")
  , ("\"grok-original\"", "XaiGrokOAuth", "{\"kind\":\"principal_oauth\"}")
  , ("\"grok-g2\"", "XaiGrokOAuth", "{\"kind\":\"principal_oauth\",\"account_ref\":\"g2\"}")
  , ("\"local\"", "OpenAiCompatible", "{\"kind\":\"environment\",\"variable\":\"KEY\"}") ]

def scenarios : List CaseRow := examplesToRows ++
  [ { name := "agent_owner_patch_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("node_did", "did:key:agent-a")]
    , patch := [("node_did", some "did:key:agent-b")] }
  , { name := "agent_invalid_reference_rejected"
    , target := .agent, guarded := false, validates := false
    , doc := [("context_id", "context-1")]
    , patch := [("context_id", some "missing-context")] }
  , { name := "datastore_owner_patch_rejected"
    , target := .datastoreToolSurface, guarded := false, validates := true
    , doc := [("surface_id", "jobs"), ("node_did", "did:key:agent-a")]
    , patch := [("node_did", some "did:key:agent-b")] }
  , { name := "datastore_invalid_entries_rejected"
    , target := .datastoreToolSurface, guarded := false, validates := false
    , doc := [("surface_id", "jobs"), ("entries", "valid")]
    , patch := [("entries", some "invalid")] }
  , { name := "tools_self_disable_unguarded_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("self_config", some "{\"enable_self_config\":false}")] }
  , { name := "tools_self_disable_guarded_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("self_config", some "{\"enable_self_config\":false}")] }
  , { name := "tools_guarded_host_patch_accepted"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("host", some "{\"root\":\"/workspace\"}")] }
  , { name := "tools_guarded_self_surface_selection_accepted"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("datastore",
        some "{\"enable_defra_query\":true,\"datastore_tool_surface_ids\":[\"engineer-mailbox\"]}")] }
  , { name := "tools_guarded_agents_enable_accepted"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("agents", some "{\"enabled\":true}")] }
  , { name := "tools_guarded_agents_removal_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}"),
              ("agents", "{\"enabled\":true}")]
    , patch := [("agents", some "{\"enabled\":false}")] }
  , { name := "tools_guarded_agents_clear_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}"),
              ("agents", "{\"enabled\":true}")]
    , patch := [("agents", none)] }
  , { name := "tools_guarded_self_config_clear_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("self_config", none)] }
  , { name := "tools_guarded_no_lockout_removal_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"self_config_no_lockout\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"self_config_no_lockout\":false}")] }
  , { name := "tools_guarded_tools_authority_removal_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"self_config_no_lockout\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"self_config_categories\":[\"profile\"]}")] }
  , { name := "tools_guarded_category_narrowing_keeping_tools_accepted"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"self_config_no_lockout\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"self_config_categories\":[\"tools\"]}")] }
  , { name := "agent_guarded_self_disable_rejected"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_id", "default"), ("tags", "[\"gents:setup-steward\"]")]
    , patch := [("enabled", some "false")] }
  , { name := "agent_guarded_setup_tag_removal_rejected"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_id", "default"), ("tags", "[\"gents:setup-steward\"]")]
    , patch := [("tags", some "[\"ui:engineer\"]")] }
  , { name := "agent_guarded_tag_addition_accepted"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_id", "default"), ("tags", "[\"gents:setup-steward\"]")]
    , patch := [("tags", some "[\"gents:setup-steward\",\"ui:engineer\"]")] }
  , { name := "task_targeting_invoker_unguarded_accepted"
    , target := .task, guarded := false, validates := true
    , doc := [("task_id", "engineer-inbox"), ("agent_id", "default")]
    , patch := [("prompt_template", some "Review {{ doc.outcome }}")] }
  , { name := "backend_observation_patch_rejected"
    , target := .inferenceBackend, guarded := false, validates := true
    , doc := [("backend_id", "backend-1")]
    , patch := [("probe_status", some "healthy")] }
  , { name := "backend_oauth_account_change_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"principal_oauth\",\"account_ref\":\"a2\"}")] }
  , { name := "backend_oauth_reference_set_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")] }
  , { name := "backend_oauth_reference_dropped_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"principal_oauth\"}")] }
  , { name := "backend_oauth_original_introduce_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"principal_oauth\"}")] }
  , { name := "backend_oauth_endpoint_edit_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("endpoint", some "\"http://127.0.0.1:2/v1\"")] }
  , { name := "backend_oauth_to_environment_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"principal_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"environment\",\"variable\":\"KEY\"}")] }
  , { name := "profile_keep_backend_model_edit_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("model_name", some "\"model-2\"")]
    , backends := profileBackends }
  , { name := "profile_same_provider_same_account_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("backend_id", some "\"chat-a1-alt\"")]
    , backends := profileBackends }
  , { name := "profile_same_provider_switch_rejected"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("backend_id", some "\"chat-a2\"")]
    , backends := profileBackends }
  , { name := "profile_cross_provider_default_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("backend_id", some "\"grok-original\"")]
    , backends := profileBackends }
  , { name := "profile_cross_provider_non_default_rejected"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("backend_id", some "\"grok-g2\"")]
    , backends := profileBackends }
  , { name := "profile_to_account_free_backend_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"chat-a1\"")]
    , patch := [("backend_id", some "\"local\"")]
    , backends := profileBackends }
  , { name := "profile_create_default_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := []
    , patch := [("backend_id", some "\"grok-original\"")]
    , backends := profileBackends }
  , { name := "profile_create_non_default_rejected"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := []
    , patch := [("backend_id", some "\"chat-a2\"")]
    , backends := profileBackends }
  , { name := "profile_optional_sampling_clear_accepted"
    , target := .inferenceProfile, guarded := false, validates := true
    , doc := [("sampling_id", "sampling-1")]
    , patch := [("sampling_id", none)] }
  ]

def selfConfigCases : List CaseWitness :=
  scenarios.map buildWitness

theorem self_config_cases_witness_theorems :
    selfConfigCases.all (fun w =>
      w.protectedPreserved && w.containmentHolds && w.unchangedOnReject
        && w.controlKeptAfterAccept) = true := by
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
