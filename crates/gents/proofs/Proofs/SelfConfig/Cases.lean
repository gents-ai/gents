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
  held : Grants := Grants.bot
  tools : List (FieldValue × List (FieldKey × FieldValue)) := []
  contexts : List (FieldValue × Option FieldValue) := []
  backends : List (String × String × String) := []
  deriving Repr

def rowPatch (r : CaseRow) : Patch :=
  r.patch.map (fun e =>
    { key := e.1
    , op := match e.2 with
        | some v => PatchOp.set v
        | none => PatchOp.clear })

/-- Fixture decoder: production uses the shared typed decoder, not this finite
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

def decodeReach (doc : Doc) : Option Reach := do
  let enabled ← match doc "enabled" with
    | some "true" | none => some true
    | some "false" => some false
    | _ => none
  pure { enabled }

/-- Tools `integrations` text for the plugin `acme/wf` with a job block: the
specified `max_live` and policy mode `dynamic`. -/
def jobIntegrations (maxLive : Nat) (dynamic : String) : FieldValue :=
  "{\"plugins\":[{\"plugin\":\"acme/wf\",\"job\":{\"agent_targets\":[\"target\"],\"max_live\":"
    ++ toString maxLive
    ++ ",\"max_spawns\":128,\"max_messages_per_slot\":4,\"result_bytes\":65536,\"max_wall_secs\":7200,"
    ++ "\"policy\":{\"dynamic\":\"" ++ dynamic
    ++ "\",\"dynamic_targets\":[],\"trusted_packs\":[]}}}]}"

/-- Fixture decoder; production projects `OperatorGrants::from_tools_json`. -/
def decodeGrants (doc : Doc) : Option Grants :=
  match doc "self_config" with
  | some "{\"enable_self_config\":true,\"enable_pack_install\":true}" =>
      some { Grants.bot with packInstall := true }
  | some "{\"enable_self_config\":true,\"self_config_no_lockout\":true,\"enable_pack_install\":true}" =>
      some { Grants.bot with packInstall := true }
  | _ => some Grants.bot

def awsAuthText : String :=
  "{\"kind\":\"aws_credentials\",\"access_key_id\":\"AKIDEXAMPLE\",\"secret_access_key\":\"secret\"}"

def googleAuthText : String :=
  "{\"kind\":\"google_credentials\",\"credentials_json\":\"{}\"}"

def decodeAuthText : String → Option Configuration.BackendAuth
  | "{\"kind\":\"environment\",\"variable\":\"KEY\"}" => some (.environment "KEY")
  | "{\"kind\":\"node_oauth\"}" => some (.nodeOAuth none)
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}" => some (.nodeOAuth (some "a1"))
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"a2\"}" => some (.nodeOAuth (some "a2"))
  | "{\"kind\":\"node_oauth\",\"account_ref\":\"g2\"}" => some (.nodeOAuth (some "g2"))
  | "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}" => some (.nodeOAuthPool false)
  | "{\"kind\":\"node_oauth_pool\",\"allow_overage\":true}" => some (.nodeOAuthPool true)
  | "{\"kind\":\"credential\",\"id\":\"c1\",\"version\":\"v1\"}" => some (.credential "c1" "v1")
  | "{\"kind\":\"credential\",\"id\":\"c2\",\"version\":\"v2\"}" => some (.credential "c2" "v2")
  | "{\"kind\":\"api_key\",\"key\":\"operator-secret\"}" => some (.apiKey "operator-secret")
  | text =>
    if text = awsAuthText then some (.awsCredentials "AKIDEXAMPLE" "secret" none)
    else if text = googleAuthText then some (.googleCredentials "{}")
    else none

def decodeAuth (doc : Doc) : Option Configuration.BackendAuth :=
  (doc "auth").bind decodeAuthText

def lockoutGuard (t : Target) (stored : Doc) : Doc → Bool :=
  if t = .agent then keepsReach decodeReach stored
  else keepsControl decodeControl stored

def rowBackendOf (r : CaseRow) (id : String) : Option (String × Configuration.BackendAuth) :=
  (r.backends.find? (·.1 = id)).bind fun (_, kind, auth) => (decodeAuthText auth).map (kind, ·)

def plainTools : List (FieldKey × FieldValue) :=
  [("self_config", "{\"enable_self_config\":true}")]

def grantedTools : List (FieldKey × FieldValue) :=
  [("self_config", "{\"enable_self_config\":true,\"enable_pack_install\":true}")]

def rowResolve (r : CaseRow) : Doc → Option Doc :=
  let tools (id : FieldValue) : Option Doc :=
    (r.tools.find? (·.1 == id)).map (fun entry => Doc.ofList entry.2)
  match r.target with
  | .agentContext => fun doc => (doc "tools_id").bind tools
  | .agent => fun doc =>
      ((doc "context_id").bind fun context =>
        (r.contexts.find? (·.1 == context)).bind (·.2)).bind tools
  | _ => fun _ => none

/-- The always-on operator-grant slice: run by the native owner on every Tools
write and on every Context/Agent write or clone whose Tools selection changes. -/
def grantGuard (r : CaseRow) (stored : Doc) : Doc → Bool :=
  match r.target with
  | .tools => keepsGrants decodeGrants r.held stored
  | .agentContext | .agent => chainKeepsGrants decodeGrants r.held (rowResolve r) stored
  | _ => fun _ => true

/-- A guarded row replays its target's typed guard: the no-lockout slice for
Tools and Agent, the auth fence for Backend, the account choice fence for
Profile (default = the original account), all enforced by Rust in `validate`. -/
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

def examples : List (Target × FieldKey × FieldValue) :=
  [ (.agent, "context_id", "context-1")
  , (.agentContext, "system_prompt", "You are concise.")
  , (.compaction, "threshold", "0.75")
  , (.tools, "host", "{\"root\":\"/workspace\"}")
  , (.agentTarget, "agent_did", "gatekeeper")
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

def profileBackends : List (String × String × String) :=
  [ ("\"chat-a1\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a1-alt\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")
  , ("\"chat-a2\"", "ChatGptCodex", "{\"kind\":\"node_oauth\",\"account_ref\":\"a2\"}")
  , ("\"grok-original\"", "XaiGrokOAuth", "{\"kind\":\"node_oauth\"}")
  , ("\"grok-g2\"", "XaiGrokOAuth", "{\"kind\":\"node_oauth\",\"account_ref\":\"g2\"}")
  , ("\"local\"", "OpenAiCompatible", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")
  , ("\"claude-pool\"", "ClaudeCliSubscription",
      "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}")
  , ("\"claude-pool-alt\"", "ClaudeCliSubscription",
      "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}")
  , ("\"claude-pool-overage\"", "ClaudeCliSubscription",
      "{\"kind\":\"node_oauth_pool\",\"allow_overage\":true}")
  , ("\"claude-original\"", "ClaudeCliSubscription", "{\"kind\":\"node_oauth\"}") ]

def scenarios : List CaseRow := examplesToRows ++
  [ { name := "agent_owner_patch_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("node_did", "did:key:node-a")]
    , patch := [("node_did", some "did:key:node-b")] }
  , { name := "agent_creation_nonce_set_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "worker"), ("creation_nonce", "original")]
    , patch := [("creation_nonce", some "replacement")] }
  , { name := "agent_creation_nonce_clear_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "worker"), ("creation_nonce", "original")]
    , patch := [("creation_nonce", none)] }
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
    , doc := [("agent_did", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("enabled", some "false")] }
  , { name := "agent_guarded_engineer_tag_removal_accepted"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_did", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("tags", some "[\"ui:engineer\"]")] }
  , { name := "agent_guarded_tag_addition_accepted"
    , target := .agent, guarded := true, validates := true
    , doc := [("agent_did", "default"), ("tags", "[\"gents:engineer\"]")]
    , patch := [("tags", some "[\"gents:engineer\",\"ui:engineer\"]")] }
  , { name := "task_targeting_invoker_unguarded_accepted"
    , target := .task, guarded := false, validates := true
    , doc := [("task_id", "engineer-inbox"), ("agent_did", "default")]
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
  , { name := "backend_pool_introduce_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth\",\"account_ref\":\"a1\"}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}")] }
  , { name := "backend_pool_overage_change_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}")]
    , patch := [("auth", some "{\"kind\":\"node_oauth_pool\",\"allow_overage\":true}")] }
  , { name := "backend_pool_kept_endpoint_edit_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"node_oauth_pool\",\"allow_overage\":false}")]
    , patch := [("endpoint", some "\"http://127.0.0.1:2/v1\"")] }
  , { name := "backend_aws_credentials_introduce_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"aws_credentials\",\"access_key_id\":\"AKIDEXAMPLE\",\"secret_access_key\":\"secret\"}")] }
  , { name := "backend_credential_reference_change_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"credential\",\"id\":\"c1\",\"version\":\"v1\"}")]
    , patch := [("auth", some "{\"kind\":\"credential\",\"id\":\"c2\",\"version\":\"v2\"}")] }
  , { name := "backend_credential_to_raw_key_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"credential\",\"id\":\"c1\",\"version\":\"v1\"}")]
    , patch := [("auth", some "{\"kind\":\"api_key\",\"key\":\"operator-secret\"}")] }
  , { name := "backend_aws_credentials_kept_endpoint_edit_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"aws_credentials\",\"access_key_id\":\"AKIDEXAMPLE\",\"secret_access_key\":\"secret\"}")]
    , patch := [("endpoint", some "\"http://127.0.0.1:2/v1\"")] }
  , { name := "backend_google_credentials_introduce_rejected"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"environment\",\"variable\":\"KEY\"}")]
    , patch := [("auth", some "{\"kind\":\"google_credentials\",\"credentials_json\":\"{}\"}")] }
  , { name := "backend_google_credentials_kept_endpoint_edit_accepted"
    , target := .inferenceBackend, guarded := true, validates := true
    , doc := [("auth", "{\"kind\":\"google_credentials\",\"credentials_json\":\"{}\"}")]
    , patch := [("endpoint", some "\"http://127.0.0.1:2/v1\"")] }
  , { name := "profile_switch_to_pool_rejected"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"claude-original\"")]
    , patch := [("backend_id", some "\"claude-pool\"")]
    , backends := profileBackends }
  , { name := "profile_same_pool_alt_backend_accepted"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"claude-pool\"")]
    , patch := [("backend_id", some "\"claude-pool-alt\"")]
    , backends := profileBackends }
  , { name := "profile_pool_overage_switch_rejected"
    , target := .inferenceProfile, guarded := true, validates := true
    , doc := [("backend_id", "\"claude-pool\"")]
    , patch := [("backend_id", some "\"claude-pool-overage\"")]
    , backends := profileBackends }
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
    , patch := [("agents", some "{\"enabled\":true}")] }
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
  , { name := "agent_grant_reselect_without_held_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "worker"), ("context_id", "ctx-plain")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-plain", some "plain"), ("ctx-granted", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "agent_reselect_away_from_granted_tools_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "worker"), ("context_id", "ctx-granted")]
    , patch := [("context_id", some "ctx-plain")]
    , contexts := [("ctx-plain", some "plain"), ("ctx-granted", some "granted")]
    , tools := [("plain", plainTools), ("granted", grantedTools)] }
  , { name := "clone_granted_source_without_held_rejected"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "copy")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-granted", some "granted")]
    , tools := [("granted", grantedTools)] }
  , { name := "clone_granted_source_with_held_accepted"
    , target := .agent, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("agent_did", "copy")]
    , patch := [("context_id", some "ctx-granted")]
    , contexts := [("ctx-granted", some "granted")]
    , tools := [("granted", grantedTools)] }
  , { name := "clone_plain_source_without_held_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "copy")]
    , patch := [("context_id", some "ctx-plain")]
    , contexts := [("ctx-plain", some "plain")]
    , tools := [("plain", plainTools)] }
  , { name := "clone_source_without_tools_accepted"
    , target := .agent, guarded := false, validates := true
    , doc := [("agent_did", "copy")]
    , patch := [("context_id", some "ctx-bare")]
    , contexts := [("ctx-bare", none)] }
  , { name := "tools_job_ceiling_narrowing_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}"),
        ("integrations", jobIntegrations 8 "check")]
    , patch := [("integrations", some (jobIntegrations 4 "check"))] }
  , { name := "tools_job_ceiling_widening_with_held_accepted"
    , target := .tools, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("self_config", "{\"enable_self_config\":true}"),
        ("integrations", jobIntegrations 4 "check")]
    , patch := [("integrations", some (jobIntegrations 8 "check"))] }
  , { name := "tools_job_policy_edit_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}"),
        ("integrations", jobIntegrations 4 "check")]
    , patch := [("integrations", some (jobIntegrations 4 "off"))] }
  , { name := "tools_job_block_added_accepted"
    , target := .tools, guarded := false, validates := true
    , doc := [("self_config", "{\"enable_self_config\":true}")]
    , patch := [("integrations", some (jobIntegrations 4 "check"))] }
  , { name := "context_select_job_tools_from_none_with_held_accepted"
    , target := .agentContext, guarded := false, validates := true
    , held := { Grants.bot with packInstall := true }
    , doc := [("context_id", "ctx")]
    , patch := [("tools_id", some "jobbed")]
    , tools := [("jobbed", plainTools ++ [("integrations", jobIntegrations 4 "check")])] }
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

theorem self_config_cases_cover_job_guard :
    (selfConfigCases.any (fun w =>
        w.row.name == "tools_job_ceiling_narrowing_accepted" && w.accepted)
      && ["tools_job_ceiling_widening_with_held_accepted", "tools_job_policy_edit_accepted",
          "tools_job_block_added_accepted",
          "context_select_job_tools_from_none_with_held_accepted"].all (fun n =>
        selfConfigCases.any (fun w =>
          w.row.name == n && w.admissiblePatch && w.accepted))) = true := by
  native_decide

def publishedProfiles : List String := ["fast", "deep"]

/-! ### The node's ACP state for agent operations

Every Agent is created by `Identity.Household.commitAgentCreation`. The User
owns the Node; `admin` is an enrolled member holding `nodes#administrator`;
`operator` is a person granted `administrator` on `worker` alone. The Engineer
holds `config_writer` on itself (`gents init`) and owns `helper`, which it
created. `lead` is an Agent holding the catalog through `nodes#administrator`:
the grant the `node` category needs and that no Agent receives today
(`SelfConfig.category_gaps`). `worker` holds no grant. -/
def agentAcpState : Identity.Household.State :=
  let base : Identity.Household.State :=
    { objects := [⟨"nodes", "node", "user"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"nodes", "node", "administrator", .actor "lead"⟩]
    , enrolled := [] }
  let created := [("default", "user"), ("engineer", "user"), ("worker", "user"),
    ("idle", "user"), ("lead", "user"), ("helper", "engineer")].foldl
    (fun s (id, creator) => Identity.Household.commitAgentCreation s id creator id "node" true)
    base
  { created with relations :=
      ⟨"agents", "engineer", "config_writer", .actor "engineer"⟩ ::
      ⟨"agents", "lead", "config_writer", .actor "lead"⟩ ::
      ⟨"agents", "worker", "administrator", .actor "operator"⟩ :: created.relations }

def agentCatalog : AgentCatalog :=
  { agents := [⟨"default", true, []⟩, ⟨"engineer", true, ["gents:engineer"]⟩,
      ⟨"worker", true, []⟩, ⟨"idle", false, []⟩, ⟨"lead", true, []⟩, ⟨"helper", true, []⟩]
  , defaultId := some "default" }

def emptyAgentCatalog : AgentCatalog :=
  { agents := [], defaultId := none }

def fixtureActor (did : String) (kind : ActorKind) : Actor :=
  { did, kind
  , grants := actorGrantsOf agentAcpState did "node" (agentCatalog.agents.map (·.id)) }

def userActor : Actor := fixtureActor "user" .person
def adminActor : Actor := fixtureActor "admin" .person
def operatorActor : Actor := fixtureActor "operator" .person
def strangerActor : Actor := fixtureActor "stranger" .person
def engineerActor : Actor := fixtureActor "engineer" .agent
def leadActor : Actor := fixtureActor "lead" .agent
def workerActor : Actor := fixtureActor "worker" .agent

/-- The grants each fixture actor holds, read from `agentAcpState` by native
ACP evaluation: owner and administrator reach the catalog and every Agent; a
per-Agent grant reaches that Agent alone; `config_writer` reaches the holder
itself; ownership reaches the Agents an Agent created; nothing else does. -/
theorem fixture_actor_grants :
    [userActor, adminActor, operatorActor, strangerActor, engineerActor, leadActor,
      workerActor].map (fun a => (a.did, a.grants)) =
    [("user", ⟨true, ["default", "engineer", "worker", "idle", "lead", "helper"]⟩),
     ("admin", ⟨true, ["default", "engineer", "worker", "idle", "lead", "helper"]⟩),
     ("operator", ⟨false, ["worker"]⟩),
     ("stranger", ⟨false, []⟩),
     ("engineer", ⟨false, ["engineer", "helper"]⟩),
     ("lead", ⟨true, ["default", "engineer", "worker", "idle", "lead", "helper"]⟩),
     ("worker", ⟨false, []⟩)] := by
  native_decide

/-- A clone holds no category: after `commitAgentCreation` publishes it from
an Engineer source, the clone's DID neither holds the catalog nor configures
itself or its source. -/
theorem clone_holds_no_category :
    actorGrantsOf
      (Identity.Household.commitAgentCreation agentAcpState "clone" "engineer" "clone" "node" true)
      "clone" "node" ["clone", "engineer"] = ⟨false, []⟩ := by
  native_decide

structure AgentDecisionRow where
  name : String
  catalog : AgentCatalog := agentCatalog
  profiles : List String := publishedProfiles
  actor : Actor := userActor
  request : AgentRequest
  deriving Repr

structure AgentDecisionWitness where
  row : AgentDecisionRow
  admission : AgentAdmission
  deriving Repr

def AgentDecisionWitness.accepted (w : AgentDecisionWitness) : Bool := w.admission.isOk

def freshCreate : AgentCreateInput :=
  { name := "reviewer", systemPrompt := "Review diffs.", profile := "fast" }

def cloneOf (source : String) : AgentCreateInput :=
  { freshCreate with systemPrompt := "", profile := "deep", cloneFrom := source }

def editOf (target : String) (f : AgentEditFields) (makeDefault : Bool := false) : AgentRequest :=
  { operation := .edit f, target, makeDefault }

def disableOf (target : String) (makeDefault : Bool := false) : AgentRequest :=
  { operation := .disable, target, makeDefault }

def createOf (target : String) (i : AgentCreateInput := freshCreate) (makeDefault : Bool := false) :
    AgentRequest :=
  { operation := .create i, target, makeDefault }

def agentDecisionScenarios : List AgentDecisionRow :=
  [ -- the lockout rules
    { name := "agent_default_disable_rejected", request := disableOf "default" }
  , { name := "agent_default_enabled_false_edit_rejected"
    , request := editOf "default" { enabled := some false } }
  , { name := "agent_disable_while_designating_default_rejected"
    , request := disableOf "worker" (makeDefault := true) }
  , { name := "agent_enabled_false_edit_designating_default_rejected"
    , request := editOf "worker" { enabled := some false } (makeDefault := true) }
  , { name := "agent_choose_disabled_default_rejected", request := chooseDefault "idle" }
  , { name := "agent_choose_disabled_default_enabling_accepted"
    , request := editOf "idle" { enabled := some true } (makeDefault := true) }
  , { name := "agent_choose_default_accepted", request := chooseDefault "worker" }
  , { name := "agent_self_disable_rejected", actor := engineerActor
    , request := disableOf "engineer" }
  , { name := "agent_self_enabled_false_edit_rejected", actor := leadActor
    , request := editOf "lead" { enabled := some false } }
  , { name := "agent_non_default_disable_accepted", request := disableOf "worker" }
    -- no protected agent
  , { name := "agent_engineer_edit_accepted"
    , request := editOf "engineer" { profile := some (.set "deep") } }
  , { name := "agent_engineer_disable_accepted", request := disableOf "engineer" }
  , { name := "agent_engineer_disabled_by_agent_accepted", actor := leadActor
    , request := disableOf "engineer" }
  , { name := "agent_engineer_drops_own_tag_accepted", actor := engineerActor
    , request := editOf "engineer" { tags := some (.set "[]") } }
    -- the one edit covers every Agent field
  , { name := "agent_edit_context_description_tags_accepted"
    , request := editOf "worker"
        { contextId := some (.set "worker:context"), description := some (.set "Reviews.")
        , tags := some (.set "[\"ui:reviewer\"]") } }
  , { name := "agent_edit_context_clear_accepted"
    , request := editOf "worker" { contextId := some .clear } }
  , { name := "agent_edit_blank_context_rejected"
    , request := editOf "worker" { contextId := some (.set " ") } }
  , { name := "agent_reenable_accepted", request := editOf "idle" { enabled := some true } }
  , { name := "agent_worker_edit_accepted"
    , request := editOf "worker" { profile := some (.set "deep") } }
  , { name := "agent_edit_missing_rejected"
    , request := editOf "ghost" { profile := some (.set "deep") } }
  , { name := "agent_edit_omitted_fields_accepted", request := editOf "worker" {} }
  , { name := "agent_edit_name_only_without_profile_accepted"
    , request := editOf "worker" { name := some (.set "Worker") } }
  , { name := "agent_edit_name_clear_accepted", request := editOf "worker" { name := some .clear } }
  , { name := "agent_edit_blank_name_rejected", request := editOf "worker" { name := some (.set "") } }
  , { name := "agent_edit_blank_prompt_rejected"
    , request := editOf "worker" { systemPrompt := some (.set "  ") } }
  , { name := "agent_edit_prompt_accepted"
    , request := editOf "worker" { systemPrompt := some (.set "Review diffs.") } }
  , { name := "agent_edit_profile_clear_rejected", request := editOf "worker" { profile := some .clear } }
  , { name := "agent_edit_unpublished_profile_rejected"
    , request := editOf "worker" { profile := some (.set "ghost") } }
    -- create and clone
  , { name := "agent_create_fresh_id_accepted", request := createOf "reviewer" }
  , { name := "agent_create_as_default_accepted"
    , request := createOf "reviewer" (makeDefault := true) }
  , { name := "agent_create_existing_id_rejected", request := createOf "worker" }
  , { name := "agent_create_blank_profile_rejected"
    , request := createOf "reviewer" { freshCreate with profile := " " } }
  , { name := "agent_create_unpublished_profile_rejected"
    , request := createOf "reviewer" { freshCreate with profile := "ghost" } }
  , { name := "agent_create_fresh_without_prompt_rejected"
    , request := createOf "reviewer" { freshCreate with systemPrompt := " " } }
  , { name := "agent_create_fresh_empty_prompt_empty_catalog_rejected"
    , catalog := emptyAgentCatalog, profiles := ["fast"]
    , request := createOf "worker" { name := "worker", systemPrompt := "", profile := "fast" } }
  , { name := "agent_create_clone_inherits_prompt_accepted", request := createOf "reviewer" (cloneOf "worker") }
  , { name := "agent_create_clone_of_engineer_accepted"
    , request := createOf "reviewer" (cloneOf "engineer") }
  , { name := "agent_create_clone_disabled_source_rejected"
    , request := createOf "reviewer" (cloneOf "idle") }
    -- the same category rule for every actor, read from ACP
  , { name := "agent_create_by_node_administrator_accepted", actor := adminActor
    , request := createOf "reviewer" }
  , { name := "agent_create_by_catalog_agent_accepted", actor := leadActor
    , request := createOf "reviewer" }
  , { name := "agent_create_without_catalog_person_rejected", actor := operatorActor
    , request := createOf "reviewer" }
  , { name := "agent_create_without_catalog_agent_rejected", actor := engineerActor
    , request := createOf "reviewer" }
  , { name := "agent_clone_without_catalog_agent_rejected", actor := engineerActor
    , request := createOf "reviewer" (cloneOf "engineer") }
  , { name := "agent_choose_default_without_catalog_rejected", actor := operatorActor
    , request := chooseDefault "worker" }
  , { name := "agent_choose_default_by_catalog_agent_accepted", actor := leadActor
    , request := chooseDefault "worker" }
  , { name := "agent_edit_by_per_agent_administrator_accepted", actor := operatorActor
    , request := editOf "worker" { name := some (.set "Worker") } }
  , { name := "agent_disable_by_per_agent_administrator_accepted", actor := operatorActor
    , request := disableOf "worker" }
  , { name := "agent_edit_foreign_without_grant_rejected", actor := operatorActor
    , request := editOf "engineer" { name := some (.set "Builder") } }
  , { name := "agent_edit_own_with_self_config_accepted", actor := engineerActor
    , request := editOf "engineer" { name := some (.set "Builder") } }
  , { name := "agent_edit_created_agent_by_owner_accepted", actor := engineerActor
    , request := editOf "helper" { name := some (.set "Helper") } }
  , { name := "agent_edit_other_agent_without_grant_rejected", actor := engineerActor
    , request := editOf "worker" { name := some (.set "Worker") } }
  , { name := "agent_edit_own_without_self_config_rejected", actor := workerActor
    , request := editOf "worker" { name := some (.set "Worker") } }
  , { name := "agent_disable_by_stranger_rejected", actor := strangerActor
    , request := disableOf "worker" } ]

def agentDecisionCases : List AgentDecisionWitness :=
  agentDecisionScenarios.map fun r =>
    { row := r, admission := agentAdmission r.profiles r.catalog r.actor r.request }

theorem agent_decision_cases_regressions :
    (agentDecisionCases.filter (·.accepted)).map (·.row.name) =
      ["agent_choose_disabled_default_enabling_accepted", "agent_choose_default_accepted",
       "agent_non_default_disable_accepted", "agent_engineer_edit_accepted",
       "agent_engineer_disable_accepted", "agent_engineer_disabled_by_agent_accepted",
       "agent_engineer_drops_own_tag_accepted", "agent_edit_context_description_tags_accepted",
       "agent_edit_context_clear_accepted", "agent_reenable_accepted",
       "agent_worker_edit_accepted", "agent_edit_omitted_fields_accepted",
       "agent_edit_name_only_without_profile_accepted", "agent_edit_name_clear_accepted",
       "agent_edit_prompt_accepted", "agent_create_fresh_id_accepted",
       "agent_create_as_default_accepted", "agent_create_clone_inherits_prompt_accepted",
       "agent_create_clone_of_engineer_accepted", "agent_create_by_node_administrator_accepted",
       "agent_create_by_catalog_agent_accepted", "agent_choose_default_by_catalog_agent_accepted",
       "agent_edit_by_per_agent_administrator_accepted",
       "agent_disable_by_per_agent_administrator_accepted",
       "agent_edit_own_with_self_config_accepted", "agent_edit_created_agent_by_owner_accepted"] := by
  native_decide

/-- Each refused row is refused for the reason its name states. -/
theorem agent_decision_refusals :
    (agentDecisionCases.filterMap fun w => match w.admission with
      | .error e => some (w.row.name, e)
      | .ok _ => none) =
      [("agent_default_disable_rejected", .defaultDisabled),
       ("agent_default_enabled_false_edit_rejected", .defaultDisabled),
       ("agent_disable_while_designating_default_rejected", .defaultDisabled),
       ("agent_enabled_false_edit_designating_default_rejected", .defaultDisabled),
       ("agent_choose_disabled_default_rejected", .defaultDisabled),
       ("agent_self_disable_rejected", .selfLockout),
       ("agent_self_enabled_false_edit_rejected", .selfLockout),
       ("agent_edit_blank_context_rejected", .invalidInput),
       ("agent_edit_missing_rejected", .targetMissing),
       ("agent_edit_blank_name_rejected", .invalidInput),
       ("agent_edit_blank_prompt_rejected", .invalidInput),
       ("agent_edit_profile_clear_rejected", .invalidInput),
       ("agent_edit_unpublished_profile_rejected", .invalidInput),
       ("agent_create_existing_id_rejected", .targetExists),
       ("agent_create_blank_profile_rejected", .invalidInput),
       ("agent_create_unpublished_profile_rejected", .invalidInput),
       ("agent_create_fresh_without_prompt_rejected", .invalidInput),
       ("agent_create_fresh_empty_prompt_empty_catalog_rejected", .invalidInput),
       ("agent_create_clone_disabled_source_rejected", .invalidInput),
       ("agent_create_without_catalog_person_rejected", .catalogNotHeld),
       ("agent_create_without_catalog_agent_rejected", .catalogNotHeld),
       ("agent_clone_without_catalog_agent_rejected", .catalogNotHeld),
       ("agent_choose_default_without_catalog_rejected", .catalogNotHeld),
       ("agent_edit_foreign_without_grant_rejected", .agentNotHeld),
       ("agent_edit_other_agent_without_grant_rejected", .agentNotHeld),
       ("agent_edit_own_without_self_config_rejected", .agentNotHeld),
       ("agent_disable_by_stranger_rejected", .agentNotHeld)] := by
  native_decide

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

/-- The create input never supplies the candidate's context. -/
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
        , request := createOf "worker" { name := "worker", systemPrompt := "", profile := "fast" } } }
  , { name := "materialize_fresh_create_with_prompt_session"
    , decision :=
        { name := "", catalog := emptyAgentCatalog, profiles := ["fast"]
        , request := createOf "worker" { name := "worker", systemPrompt := "Work.", profile := "fast" } } }
  , { name := "materialize_edit_profile_clear_nothing"
    , decision := { name := "", request := editOf "worker" { profile := some .clear } } }
  , { name := "materialize_edit_name_only_session"
    , decision := { name := "", request := editOf "worker" { name := some (.set "W") } } }
  , { name := "materialize_disable_nothing"
    , decision := { name := "", request := disableOf "worker" } } ]

def agentMaterializationCases : List (String × Option Configuration.ResolvedSessionConfig) :=
  agentMaterializationScenarios.map fun r =>
    (r.name, materializedAgent r.decision.profiles r.decision.catalog r.decision.actor
      r.decision.request workerCandidate workerFixture.nodeDid)

theorem agent_materialization_cases_regressions :
    (agentMaterializationCases.filter (·.2.isSome)).map (·.1) =
      ["materialize_fresh_create_with_prompt_session",
       "materialize_edit_name_only_session"] ∧
    (Configuration.resolveAgent workerCandidate workerFixture.nodeDid "worker").toOption.isSome = true := by
  native_decide

end SelfConfig.ContractCases
