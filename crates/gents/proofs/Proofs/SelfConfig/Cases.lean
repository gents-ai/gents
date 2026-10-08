import Proofs.SelfConfig.Theorems
import Proofs.SelfConfig.Auth
import Proofs.SelfConfig.AgentDecision

namespace SelfConfig.ContractCases

open SelfConfig

structure CaseRow where
  name : String
  target : Target
  guarded : Bool
  validates : Bool
  doc : List (FieldKey × FieldValue)
  patch : List (FieldKey × Option FieldValue)
  /-- Operator grants the invoking agent holds. -/
  held : Grants := Grants.bot
  /-- Tools documents by `tools_id`, for chain and clone rows. -/
  tools : List (FieldValue × List (FieldKey × FieldValue)) := []
  /-- `context_id` to its `tools_id` (`none` selects no Tools), for Behavior rows. -/
  contexts : List (FieldValue × Option FieldValue) := []
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
    | some "{\"enable_self_config\":true,\"enable_pack_install\":true}" =>
        some (true, false, true)
    | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"enable_pack_install\":true}" =>
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
  let engineerTag ← match doc "tags" with
    | some "[\"gents:engineer\"]" => some true
    | some "[\"gents:engineer\",\"ui:engineer\"]" => some true
    | some "[]" | some "[\"ui:engineer\"]" | none => some false
    | _ => none
  pure { enabled, engineerTag }

/-- Fixture decoder for the operator grants carried by the `self_config` values
below; every other value, and an absent group, carries none. Production
projects the shared typed decoder (`OperatorGrants::from_tools_json`). -/
def decodeGrants (doc : Doc) : Option Grants :=
  match doc "self_config" with
  | some "{\"enable_self_config\":true,\"enable_pack_install\":true}" =>
      some { Grants.bot with packInstall := true }
  | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"enable_pack_install\":true}" =>
      some { Grants.bot with packInstall := true }
  | _ => some Grants.bot

/-- Fixture decoder for the exact backend auth texts used below. -/
def decodeAuthText : String → Option Configuration.BackendAuth
  | "{\"kind\":\"environment\",\"variable\":\"KEY\"}" => some (.environment "KEY")
  | "{\"kind\":\"node_oauth\"}" => some (.nodeOAuth none)
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}" => some (.nodeOAuth (some "a1"))
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"a2\"}" => some (.nodeOAuth (some "a2"))
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"g2\"}" => some (.nodeOAuth (some "g2"))
  | _ => none

def decodeAuth (doc : Doc) : Option Configuration.BackendAuth :=
  (doc "auth").bind decodeAuthText

/-- The invoker-only no-lockout slice for Tools and Agent targets. -/
def lockoutGuard (t : Target) (stored : Doc) : Doc → Bool :=
  if t = .agent then keepsReach decodeReach stored
  else keepsControl decodeControl stored

def rowBackendOf (r : CaseRow) (id : String) : Option (String × Configuration.BackendAuth) :=
  (r.backends.find? (·.1 = id)).bind fun (_, kind, auth) => (decodeAuthText auth).map (kind, ·)

/-- Tools fixtures for chain and clone rows, without and with the pack grant. -/
def plainTools : List (FieldKey × FieldValue) :=
  [("self_config", "{\"enable_self_config\":true}")]

def grantedTools : List (FieldKey × FieldValue) :=
  [("self_config", "{\"enable_self_config\":true,\"enable_pack_install\":true}")]

/-- The Tools document a Context or Agent selects, through the row's tables. -/
def rowResolve (r : CaseRow) : Doc → Option Doc :=
  let tools (id : FieldValue) : Option Doc :=
    (r.tools.find? (·.1 == id)).map (fun entry => Doc.ofList entry.2)
  match r.target with
  | .agentContext => fun doc => (doc "tools_id").bind tools
  | .agent => fun doc =>
      ((doc "context_id").bind fun context =>
        (r.contexts.find? (·.1 == context)).bind (·.2)).bind tools
  | _ => fun _ => none

/-- The always-on operator-grant slice. It is not part of the guarded
dispatch: the native owner runs it on every Tools write from the shared validate
slot, on every Context or Agent write whose Tools selection changes
(`guard_reselection_keeps_grants_in_txn`), and on a clone when its request is
authored or previewed (`clone_keeps_grants_in_txn`), not on the Agent write
that later publishes it. -/
def grantGuard (r : CaseRow) (stored : Doc) : Doc → Bool :=
  match r.target with
  | .tools => keepsGrants decodeGrants r.held stored
  | .agentContext | .agent => chainKeepsGrants decodeGrants r.held (rowResolve r) stored
  | _ => fun _ => true

/-- Every Tools, Context and Agent row replays the always-on grant slice. A guarded row also
replays its target's typed guard: the no-lockout slice for Tools and Agent,
the auth fence for Backend and the account choice fence for Profile (default
= the original account), which Rust enforces in `validate` on every model
write rather than only under no-lockout. -/
def caseGuard (r : CaseRow) (stored : Doc) : Doc → Bool :=
  fun candidate =>
    grantGuard r stored candidate &&
      (if !r.guarded then true
       else if r.target = .inferenceBackend then authGuard decodeAuth stored candidate
       else if r.target = .inferenceProfile then
         profileGuard (rowBackendOf r) (fun _ => none) stored candidate
       else lockoutGuard r.target stored candidate)

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
  grantsKeptAfterAccept : Bool
  selectedBefore : Option (List (FieldKey × FieldValue))
  selectedAfter : Option (List (FieldKey × FieldValue))
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
  , grantsKeptAfterAccept := !outcome.isSome || grantGuard r stored result
  , selectedBefore := (rowResolve r stored).map (project .tools)
  , selectedAfter := (rowResolve r merged).map (project .tools)
  }

/-- Values are decoded group values abstracted as strings; nested validation
is supplied to `step`, using the same owner as ordinary configuration. -/
def examples : List (Target × FieldKey × FieldValue) :=
  [ (.agent, "context_id", "context-1")
  , (.agentContext, "system_prompt", "You are concise.")
  , (.compaction, "threshold", "0.75")
  , (.tools, "host", "{\"root\":\"/workspace\"}")
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
  , doc := [(t.uniqueField, "doc-1"), ("node_did", "did:key:node-a")]
  , patch := [(k, some v)] }

/-- One provider with two accounts, another with its original and a second
account, and one account-free backend. -/
def profileBackends : List (String × String × String) :=
  [ ("\"chat-a1\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a1-alt\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a2\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a2\"}")
  , ("\"grok-original\"", "XaiGrokOAuth", "{\"kind\":\"node_oauth\"}")
  , ("\"grok-g2\"", "XaiGrokOAuth", "{\"kind\":\"node_oauth\",\"account_ref\":\"g2\"}")
  , ("\"local\"", "OpenAiCompatible", "{\"kind\":\"environment\",\"variable\":\"KEY\"}") ]

def scenarios : List CaseRow := examplesToRows ++
  [ { name := "agent_owner_patch_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("node_did", "did:key:node-a")]
    , patch := [("node_did", some "did:key:node-b")] }
  , { name := "agent_invalid_reference_rejected"
    , target := .agent, guarded := false, validates := false
    , doc := [("context_id", "context-1")]
    , patch := [("context_id", some "missing-context")] }
  , { name := "datastore_owner_patch_rejected"
    , target := .datastoreToolSurface, guarded := false, validates := true
    , doc := [("surface_id", "jobs"), ("node_did", "did:key:node-a")]
    , patch := [("node_did", some "did:key:node-b")] }
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
    , doc := [("agent_id", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("enabled", some "false")] }
  , { name := "agent_guarded_engineer_tag_removal_rejected"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_id", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("tags", some "[\"ui:engineer\"]")] }
  , { name := "agent_guarded_tag_addition_accepted"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_id", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("tags", some "[\"gents:engineer\",\"ui:engineer\"]")] }
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
    , doc := [("auth", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth\",\"account_ref\":\"a2\"}")] }
  , { name := "backend_oauth_reference_set_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")] }
  , { name := "backend_oauth_reference_dropped_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth\"}")] }
  , { name := "backend_oauth_original_introduce_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth\"}")] }
  , { name := "backend_oauth_endpoint_edit_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("endpoint", some "\"http://127.0.0.1:2/v1\"")] }
  , { name := "backend_oauth_to_environment_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")]
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
  , { name := "tools_grant_self_raise_without_held_rejected"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"enable_pack_install\":true}")] }
  , { name := "tools_grant_raise_with_held_accepted"
    , target := .tools, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"enable_pack_install\":true}")] }
  , { name := "tools_grant_unrelated_edit_on_granted_tools_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"enable_pack_install\":true}")]
    , patch := [("subagents", some "{\"enabled\":true}")] }
  , { name := "tools_grant_narrowing_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"enable_pack_install\":true}")]
    , patch := [("self_config", some "{\"enable_self_config\":true}")] }
  , { name := "tools_grant_clear_on_granted_tools_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"enable_pack_install\":true}")]
    , patch := [("self_config", none)] }
  , { name := "tools_guarded_grant_raise_without_held_rejected"
    , target := .tools, guarded := true, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true,\"self_config_no_lockout\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"enable_pack_install\":true}")] }
  , { name := "tools_guarded_grant_raise_with_held_accepted"
    , target := .tools, guarded := true, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("self_config", "{\"enable_self_config\":true,\"self_config_no_lockout\":true}")]
    , patch := [("self_config",
        some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"enable_pack_install\":true}")] }
  , { name := "context_grant_reselect_without_held_rejected"
    , target := .agentContext, guarded := false, validates := true
    , doc := [("context_id", "ctx"), ("tools_id", "plain")]
    , patch := [("tools_id", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "context_grant_reselect_with_held_accepted"
    , target := .agentContext, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("context_id", "ctx"), ("tools_id", "plain")]
    , patch := [("tools_id", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "context_reselect_between_granted_tools_accepted"
    , target := .agentContext, guarded := false, validates := true
    , doc := [("context_id", "ctx"), ("tools_id", "granted")]
    , patch := [("tools_id", some "granted-2")]
    , tools := [("granted", grantedTools), ("granted-2", grantedTools)] }
  , { name := "context_create_selecting_granted_tools_without_held_rejected"
    , target := .agentContext, guarded := false, validates := true
    , doc := [("context_id", "ctx-new")]
    , patch := [("tools_id", some "granted")]
    , tools := [("granted", grantedTools)] }
  , { name := "behavior_grant_reselect_without_held_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("behavior_id", "worker"), ("context_id", "ctx-plain")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-plain", some "plain"), ("ctx-granted", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "behavior_reselect_away_from_granted_tools_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("behavior_id", "worker"), ("context_id", "ctx-granted")]
    , patch := [("context_id", some "ctx-plain")]
    , contexts := [("ctx-plain", some "plain"), ("ctx-granted", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "clone_granted_source_without_held_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("behavior_id", "copy")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-granted", some "granted")]
    , tools := [("granted", grantedTools)] }
  , { name := "clone_granted_source_with_held_accepted"
    , target := .agent, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("behavior_id", "copy")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-granted", some "granted")]
    , tools := [("granted", grantedTools)] }
  , { name := "clone_plain_source_without_held_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("behavior_id", "copy")]
    , patch := [("context_id", some "ctx-plain")]
    , contexts := [("ctx-plain", some "plain")]
    , tools := [("plain", plainTools)] }
  , { name := "clone_source_without_tools_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("behavior_id", "copy")]
    , patch := [("context_id", some "ctx-bare")]
    , contexts := [("ctx-bare", none)] }
  ]

def selfConfigCases : List CaseWitness :=
  scenarios.map buildWitness

theorem self_config_cases_witness_theorems :
    selfConfigCases.all (fun w =>
      w.protectedPreserved && w.containmentHolds && w.unchangedOnReject
        && w.controlKeptAfterAccept && w.grantsKeptAfterAccept) = true := by
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

theorem self_config_cases_cover_grant_refusals :
    (selfConfigCases.any (fun w =>
        decide (w.row.target = .tools) && !w.row.guarded && w.row.validates
          && w.admissiblePatch && !w.accepted)
      && selfConfigCases.any (fun w =>
        decide (w.row.target = .tools) && w.row.held.packInstall && w.accepted)) = true := by
  native_decide

theorem self_config_cases_cover_reselection :
    (selfConfigCases.any (fun w => decide (w.row.target = .agentContext)
        && w.selectedBefore.isSome && w.selectedAfter.isSome && !w.accepted)
      && selfConfigCases.any (fun w => decide (w.row.target = .agentContext)
        && w.selectedBefore.isNone && w.selectedAfter.isSome && !w.accepted)
      && selfConfigCases.any (fun w => decide (w.row.target = .agent)
        && w.selectedBefore.isSome && !w.accepted)
      && selfConfigCases.any (fun w => decide (w.row.target = .agent)
        && w.selectedBefore.isNone && w.selectedAfter.isSome && !w.accepted)
      && selfConfigCases.any (fun w =>
        w.selectedBefore.isNone && w.selectedAfter.isSome && w.accepted)
      && selfConfigCases.any (fun w => w.selectedBefore.isSome && w.selectedAfter.isSome
        && w.accepted && !w.row.held.packInstall)) = true := by
  native_decide

def publishedProfiles : List String := ["fast", "deep"]

structure AgentDecisionRow where
  name : String
  catalog : AgentCatalog
  profiles : List String := publishedProfiles
  operation : AgentOperation
  target : String
  makeDefault : Bool := false
  deriving Repr

structure AgentDecisionWitness where
  row : AgentDecisionRow
  accepted : Bool
  deriving Repr

def agentCatalog : AgentCatalog :=
  { agents := [("default", true), ("engineer", true), ("worker", true), ("idle", false)]
  , protectedIds := ["engineer"]
  , defaultId := some "default" }

def emptyAgentCatalog : AgentCatalog :=
  { agents := [], protectedIds := [], defaultId := none }

def freshCreate : AgentCreateInput :=
  { name := "reviewer", systemPrompt := "Review diffs.", profile := "fast" }

/-- Rows cover the catalog decision for every operation and each create-input
and edit-field predicate; verdicts come from `agentOperationAdmitted`. -/
def agentDecisionScenarios : List AgentDecisionRow :=
  [ { name := "agent_default_disable_rejected"
    , catalog := agentCatalog, operation := .disable, target := "default" }
  , { name := "agent_protected_edit_rejected"
    , catalog := agentCatalog, operation := .edit { profile := some (.set "deep") }
    , target := "engineer" }
  , { name := "agent_protected_disable_rejected"
    , catalog := agentCatalog, operation := .disable, target := "engineer" }
  , { name := "agent_non_default_disable_accepted"
    , catalog := agentCatalog, operation := .disable, target := "worker" }
  , { name := "agent_disable_while_designating_default_rejected"
    , catalog := agentCatalog, operation := .disable, target := "worker", makeDefault := true }
  , { name := "agent_unprotected_edit_accepted"
    , catalog := agentCatalog, operation := .edit { profile := some (.set "deep") }
    , target := "worker" }
  , { name := "agent_edit_missing_rejected"
    , catalog := agentCatalog, operation := .edit { profile := some (.set "deep") }
    , target := "ghost" }
  , { name := "agent_edit_omitted_fields_accepted"
    , catalog := agentCatalog, operation := .edit {}, target := "worker" }
  , { name := "agent_edit_name_only_without_profile_accepted"
    , catalog := agentCatalog, operation := .edit { name := some (.set "Worker") }
    , target := "worker" }
  , { name := "agent_edit_name_clear_accepted"
    , catalog := agentCatalog, operation := .edit { name := some .clear }, target := "worker" }
  , { name := "agent_edit_blank_name_rejected"
    , catalog := agentCatalog, operation := .edit { name := some (.set "") }
    , target := "worker" }
  , { name := "agent_edit_blank_prompt_rejected"
    , catalog := agentCatalog, operation := .edit { systemPrompt := some (.set "  ") }
    , target := "worker" }
  , { name := "agent_edit_profile_clear_rejected"
    , catalog := agentCatalog, operation := .edit { profile := some .clear }
    , target := "worker" }
  , { name := "agent_edit_unpublished_profile_rejected"
    , catalog := agentCatalog, operation := .edit { profile := some (.set "ghost") }
    , target := "worker" }
  , { name := "agent_create_fresh_id_accepted"
    , catalog := agentCatalog, operation := .create freshCreate, target := "reviewer" }
  , { name := "agent_create_existing_id_rejected"
    , catalog := agentCatalog, operation := .create freshCreate, target := "worker" }
  , { name := "agent_create_blank_profile_rejected"
    , catalog := agentCatalog, operation := .create { freshCreate with profile := " " }
    , target := "reviewer" }
  , { name := "agent_create_unpublished_profile_rejected"
    , catalog := agentCatalog, operation := .create { freshCreate with profile := "ghost" }
    , target := "reviewer" }
  , { name := "agent_create_fresh_without_prompt_rejected"
    , catalog := agentCatalog, operation := .create { freshCreate with systemPrompt := " " }
    , target := "reviewer" }
  , { name := "agent_create_fresh_empty_prompt_empty_catalog_rejected"
    , catalog := emptyAgentCatalog, profiles := ["fast"]
    , operation := .create { name := "worker", systemPrompt := "", profile := "fast" }
    , target := "worker" }
  , { name := "agent_create_clone_inherits_prompt_accepted"
    , catalog := agentCatalog
    , operation := .create { freshCreate with systemPrompt := "", profile := "deep"
                                            , cloneFrom := "worker" }
    , target := "reviewer" }
  , { name := "agent_create_clone_disabled_source_rejected"
    , catalog := agentCatalog
    , operation := .create { freshCreate with systemPrompt := "", profile := "deep"
                                            , cloneFrom := "idle" }
    , target := "reviewer" } ]

def agentDecisionCases : List AgentDecisionWitness :=
  agentDecisionScenarios.map fun r =>
    { row := r
    , accepted := agentOperationAdmitted r.profiles r.catalog r.operation r.target
        r.makeDefault }

/-- Regression expectations, checked against model execution. -/
theorem agent_decision_cases_regressions :
    (agentDecisionCases.filter (·.accepted)).map (·.row.name) =
      ["agent_non_default_disable_accepted", "agent_unprotected_edit_accepted",
       "agent_edit_omitted_fields_accepted",
       "agent_edit_name_only_without_profile_accepted", "agent_edit_name_clear_accepted",
       "agent_create_fresh_id_accepted", "agent_create_clone_inherits_prompt_accepted"] := by
  native_decide

/-! ## Materialization through the admission owner -/

/-- The documents of one candidate registry under a single owning node. Lean
builds the `Registry` from this list form and the emitter serializes the same
lists, so a consumer constructs its documents from the emitted inputs. -/
structure CandidateFixture where
  nodeDid : String
  agents : List (String × Configuration.Agent)
  contexts : List (String × Configuration.Context)
  profiles : List (String × Configuration.SelectedModel)
  backends : List (String × Configuration.Backend)

def CandidateFixture.registry (f : CandidateFixture) : Configuration.Registry :=
  { tasks := fun _ _ => none
  , agents := fun scope id =>
      if scope = f.nodeDid then (f.agents.lookup id).map (⟨f.nodeDid, ·⟩) else none
  , contexts := fun scope id =>
      if scope = f.nodeDid then (f.contexts.lookup id).map (⟨f.nodeDid, ·⟩) else none
  , profiles := fun scope id =>
      if scope = f.nodeDid then (f.profiles.lookup id).map (⟨f.nodeDid, ·⟩) else none
  , backends := fun scope id =>
      if scope = f.nodeDid then (f.backends.lookup id).map (⟨f.nodeDid, ·⟩) else none }

/-- One node owning an enabled `worker` Agent with no context and the `fast`
profile on an enabled backend, so canonical resolution succeeds for `worker`.
Candidate construction is separate from create-input admission: the create
input never supplies the candidate's context. -/
def workerFixture : CandidateFixture :=
  { nodeDid := "node"
  , agents := [("worker", { contextId := none, profileId := "fast", enabled := true })]
  , contexts := []
  , profiles := [("fast", { backendId := "local", model := "m", effort := none })]
  , backends := [("local", { enabled := true })] }

def workerCandidate : Configuration.Registry := workerFixture.registry

structure AgentMaterializationRow where
  name : String
  decision : AgentDecisionRow

def agentMaterializationScenarios : List AgentMaterializationRow :=
  [ { name := "materialize_fresh_create_without_prompt_nothing"
    , decision :=
        { name := "", catalog := emptyAgentCatalog, profiles := ["fast"]
        , operation := .create { name := "worker", systemPrompt := "", profile := "fast" }
        , target := "worker" } }
  , { name := "materialize_fresh_create_with_prompt_session"
    , decision :=
        { name := "", catalog := emptyAgentCatalog, profiles := ["fast"]
        , operation := .create { name := "worker", systemPrompt := "Work.", profile := "fast" }
        , target := "worker" } }
  , { name := "materialize_edit_profile_clear_nothing"
    , decision :=
        { name := "", catalog := agentCatalog, operation := .edit { profile := some .clear }
        , target := "worker" } }
  , { name := "materialize_edit_name_only_session"
    , decision :=
        { name := "", catalog := agentCatalog, operation := .edit { name := some (.set "W") }
        , target := "worker" } }
  , { name := "materialize_disable_nothing"
    , decision :=
        { name := "", catalog := agentCatalog, operation := .disable, target := "worker" } } ]

def agentMaterializationCases : List (String × Option Configuration.ResolvedSessionConfig) :=
  agentMaterializationScenarios.map fun r =>
    (r.name, materializedAgent r.decision.profiles r.decision.catalog r.decision.operation
      r.decision.target r.decision.makeDefault workerCandidate workerFixture.nodeDid)

/-- Regression expectations, checked against model execution. The rejected
fresh create resolves canonically (absent context is permitted) yet
materializes nothing. -/
theorem agent_materialization_cases_regressions :
    (agentMaterializationCases.filter (·.2.isSome)).map (·.1) =
      ["materialize_fresh_create_with_prompt_session",
       "materialize_edit_name_only_session"] ∧
    (Configuration.resolveAgent workerCandidate workerFixture.nodeDid "worker").toOption.isSome = true := by
  native_decide

end SelfConfig.ContractCases
