import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Tools.SelfConfig.Cases
import Proofs.Tools.SelfConfig.Selection
import Proofs.Conformance.Contracts.Json.SelfConfig
import Proofs.Conformance.Contracts.Json.CommandPolicy

namespace Conformance.Contracts

open Lean SelfConfig SelfConfig.ContractCases

deriving instance ToJson for SelfConfig.ActorKind, SelfConfig.FrontEnd, SelfConfig.AgentRefusal,
  Configuration.ReasoningEffort

structure AgentEntryWire where
  agentDid : String
  enabled : Bool
  tags : List String
  deriving ToJson

/-- The acting actor and the grants native ACP gives it
(`SelfConfig.actorGrantsOf`): `catalog` is `nodes manage_agents` on the Node and
`configures` the Agents whose document it may update. -/
structure ActorWire where
  did : String
  kind : SelfConfig.ActorKind
  catalog : Bool
  configures : List String
  deriving ToJson

structure CreateInputWire where
  displayName : String
  systemPrompt : String
  inferenceProfileId : String
  cloneFrom : String
  deriving ToJson

/-- `frontEnds` lists every front end: admission never reads it
(`SelfConfig.admission_ignores_front_end`). The edit is its patch entries in
`agentEditKeys` order; `enabled` is the text `true` or `false`. -/
structure AgentDecisionInput where
  name : String
  agents : List AgentEntryWire
  defaultId : Option String
  publishedProfiles : List String
  actor : ActorWire
  frontEnds : List SelfConfig.FrontEnd
  operation : String
  target : String
  makeDefault : Bool
  createInput : Option CreateInputWire
  editPatch : Option (List PatchEntryWire)
  deriving ToJson

def editPatchOf (f : AgentEditFields) : List PatchEntryWire :=
  let entries : List (FieldKey × Option PatchOp) :=
    [("display_name", f.name), ("description", f.description),
     ("system_prompt", f.systemPrompt), ("inference_profile_id", f.profile),
     ("context_id", f.contextId),
     ("enabled", f.enabled.map fun b => .set (if b then "true" else "false")),
     ("tags", f.tags)]
  entries.filterMap fun (field, op) => op.map fun op =>
    { field, action := match op with | .set _ => "set" | .clear => "clear", value := op.value }

def AgentDecisionInput.ofRow (r : AgentDecisionRow) : AgentDecisionInput :=
  { name := r.name
  , agents := r.catalog.agents.map fun a => { agentDid := a.id, enabled := a.enabled, tags := a.tags }
  , defaultId := r.catalog.defaultId
  , publishedProfiles := r.profiles
  , actor :=
      { did := r.actor.did, kind := r.actor.kind, catalog := r.actor.grants.catalog
        configures := r.actor.grants.configures }
  , frontEnds := [.tool, .cli, .desktop]
  , operation := match r.request.operation with
      | .create _ => "create" | .edit _ => "edit" | .disable => "disable"
  , target := r.request.target
  , makeDefault := r.request.makeDefault
  , createInput := match r.request.operation with
      | .create i =>
          some { displayName := i.name, systemPrompt := i.systemPrompt
                 inferenceProfileId := i.profile, cloneFrom := i.cloneFrom }
      | _ => none
  , editPatch := match r.request.operation with
      | .edit f => some (editPatchOf f)
      | _ => none }

def AgentDecisionInput.catalog (i : AgentDecisionInput) : AgentCatalog :=
  { agents := i.agents.map fun a => { id := a.agentDid, enabled := a.enabled, tags := a.tags }
  , defaultId := i.defaultId }

def AgentDecisionInput.actorOf (i : AgentDecisionInput) : Actor :=
  { did := i.actor.did, kind := i.actor.kind
  , grants := { catalog := i.actor.catalog, configures := i.actor.configures } }

def patchOpOf (entries : List PatchEntryWire) (field : String) : Option PatchOp :=
  (entries.find? (·.field == field)).map fun e =>
    if e.action == "set" then .set (e.value.getD "") else .clear

def editFieldsOf (entries : List PatchEntryWire) : AgentEditFields :=
  { name := patchOpOf entries "display_name"
  , description := patchOpOf entries "description"
  , systemPrompt := patchOpOf entries "system_prompt"
  , profile := patchOpOf entries "inference_profile_id"
  , contextId := patchOpOf entries "context_id"
  , enabled := (patchOpOf entries "enabled").map (· == .set "true")
  , tags := patchOpOf entries "tags" }

def AgentDecisionInput.request (i : AgentDecisionInput) : SelfConfig.AgentRequest :=
  { operation := match i.operation, i.createInput, i.editPatch with
      | "create", some c, _ =>
          .create { name := c.displayName, systemPrompt := c.systemPrompt
                    profile := c.inferenceProfileId, cloneFrom := c.cloneFrom }
      | "edit", _, some entries => .edit (editFieldsOf entries)
      | _, _, _ => .disable
  , target := i.target
  , makeDefault := i.makeDefault }

/-- A refused preview reports the same `refusal` as a successful answer
(`SelfConfig.preview_refusal_matches_commit`). -/
structure AgentDecisionOutput where
  accepted : Bool
  refusal : Option SelfConfig.AgentRefusal
  deriving ToJson

example : (agentDecisionCases.map (·.row.name)).eraseDups.length = agentDecisionCases.length := by
  native_decide

@[conformance_family]
def selfConfigAgentDecision : Family AgentDecisionInput AgentDecisionOutput where
  inputs := agentDecisionCases.map fun w => .ofRow w.row
  model := fun i =>
    let admission := agentAdmission i.publishedProfiles i.catalog i.actorOf i.request
    { accepted := admission.isOk
    , refusal := match admission with
        | .error e => some e
        | .ok _ => none }

structure CandidateAgentWire where
  agentDid : String
  creationNonce : String
  contextId : Option String
  inferenceProfileId : String
  enabled : Bool
  deriving ToJson

structure CandidateContextWire where
  contextId : String
  systemPrompt : String
  skillIds : List String
  toolNames : List String
  deriving ToJson

structure CandidateProfileWire where
  profileId : String
  backendId : String
  modelName : String
  reasoningEffort : Option Configuration.ReasoningEffort
  deriving ToJson

structure CandidateBackendWire where
  backendId : String
  enabled : Bool
  deriving ToJson

structure CandidateWire where
  nodeDid : String
  agents : List CandidateAgentWire
  contexts : List CandidateContextWire
  inferenceProfiles : List CandidateProfileWire
  inferenceBackends : List CandidateBackendWire
  deriving ToJson

def CandidateWire.ofFixture (f : CandidateFixture) : CandidateWire :=
  { nodeDid := f.nodeDid
  , agents := f.agents.map fun (id, a) =>
      { agentDid := id, creationNonce := id, contextId := a.contextId
        inferenceProfileId := a.profileId, enabled := a.enabled }
  , contexts := f.contexts.map fun (id, c) =>
      { contextId := id, systemPrompt := c.instructions, skillIds := c.skillIds
        toolNames := c.toolNames }
  , inferenceProfiles := f.profiles.map fun (id, m) =>
      { profileId := id, backendId := m.backendId, modelName := m.model
        reasoningEffort := m.effort }
  , inferenceBackends := f.backends.map fun (id, b) => { backendId := id, enabled := b.enabled } }

def CandidateWire.fixture (c : CandidateWire) : CandidateFixture :=
  { nodeDid := c.nodeDid
  , agents := c.agents.map fun a =>
      (a.agentDid, { contextId := a.contextId, profileId := a.inferenceProfileId
                     enabled := a.enabled })
  , contexts := c.contexts.map fun x =>
      (x.contextId, { instructions := x.systemPrompt, skillIds := x.skillIds
                      toolNames := x.toolNames })
  , profiles := c.inferenceProfiles.map fun p =>
      (p.profileId, { backendId := p.backendId, model := p.modelName, effort := p.reasoningEffort })
  , backends := c.inferenceBackends.map fun b => (b.backendId, { enabled := b.enabled }) }

structure AgentMaterializationInput where
  name : String
  agents : List AgentEntryWire
  defaultId : Option String
  publishedProfiles : List String
  actor : ActorWire
  frontEnds : List SelfConfig.FrontEnd
  operation : String
  target : String
  makeDefault : Bool
  createInput : Option CreateInputWire
  editPatch : Option (List PatchEntryWire)
  candidate : CandidateWire
  deriving ToJson

def AgentMaterializationInput.decision (i : AgentMaterializationInput) : AgentDecisionInput :=
  { name := i.name, agents := i.agents, defaultId := i.defaultId
  , publishedProfiles := i.publishedProfiles, actor := i.actor, frontEnds := i.frontEnds
  , operation := i.operation, target := i.target, makeDefault := i.makeDefault
  , createInput := i.createInput, editPatch := i.editPatch }

structure SessionWire where
  instructions : String
  skillIds : List String
  toolNames : List String
  backendId : String
  model : String
  effort : Option Configuration.ReasoningEffort
  deriving ToJson

@[conformance_family]
def selfConfigAgentMaterialization :
    Family AgentMaterializationInput (Option SessionWire) where
  inputs := agentMaterializationScenarios.map fun r =>
    let d := AgentDecisionInput.ofRow { r.decision with name := r.name }
    { name := d.name, agents := d.agents, defaultId := d.defaultId
    , publishedProfiles := d.publishedProfiles, actor := d.actor, frontEnds := d.frontEnds
    , operation := d.operation, target := d.target, makeDefault := d.makeDefault
    , createInput := d.createInput, editPatch := d.editPatch
    , candidate := .ofFixture workerFixture }
  model := fun i =>
    let d := i.decision
    let fixture := i.candidate.fixture
    (materializedAgent d.publishedProfiles d.catalog d.actorOf d.request fixture.registry
      fixture.nodeDid).map fun r =>
        { instructions := r.context.instructions, skillIds := r.context.skillIds
          toolNames := r.context.toolNames, backendId := r.inference.backendId
          model := r.inference.model, effort := r.inference.effort }

def siblingToolsOperations : List SiblingToolsOperation := Id.run do
  let mut rows : List SiblingToolsOperation := []
  for owner in [true, false] do
    for sharedContext in [false, true] do
      for sharedTools in [false, true] do
        for requested in [none, some .inherit, some .disabled, some .enabled] do
          for existing in [CommandPolicy.NetworkMode.inherit, .disabled, .enabled] do
            rows := rows ++ [{ ownerMatches := owner
                             , sharedContext := sharedContext, sharedTools := sharedTools
                             , requestedNetwork := requested
                             , existingNetwork := existing }]
  return rows

structure SiblingToolsInput where
  ownerMatches : Bool
  sharedContext : Bool
  sharedTools : Bool
  requestedNetwork : Option CommandPolicy.NetworkMode
  existingNetwork : CommandPolicy.NetworkMode
  deriving ToJson

structure SiblingToolsOutput where
  admitted : Bool
  resultNetwork : CommandPolicy.NetworkMode
  deriving ToJson

@[conformance_family]
def selfConfigSiblingTools : Family SiblingToolsInput SiblingToolsOutput where
  inputs := siblingToolsOperations.map fun op =>
    { ownerMatches := op.ownerMatches, sharedContext := op.sharedContext
      sharedTools := op.sharedTools, requestedNetwork := op.requestedNetwork
      existingNetwork := op.existingNetwork }
  model := fun i =>
    let op : SiblingToolsOperation :=
      { ownerMatches := i.ownerMatches, sharedContext := i.sharedContext
        sharedTools := i.sharedTools, requestedNetwork := i.requestedNetwork
        existingNetwork := i.existingNetwork }
    { admitted := op.admitted, resultNetwork := op.resultNetwork }

structure ToolFlagInput where
  existing : Bool
  requested : Option Bool
  deriving ToJson

@[conformance_family]
def selfConfigToolFlag : Family ToolFlagInput Bool where
  inputs := [false, true].flatMap fun existing =>
    [none, some false, some true].map fun requested => { existing, requested }
  model := fun i => selectedToolFlag i.requested i.existing

structure JobPolicyText where
  dynamic : String
  dynamic_targets : List String
  trusted_packs : List String
  deriving ToJson, FromJson

structure JobText where
  agent_targets : List String
  max_live : Nat
  max_spawns : Nat
  max_messages_per_slot : Nat
  result_bytes : Nat
  max_wall_secs : Nat
  policy : JobPolicyText
  deriving ToJson, FromJson

structure PluginText where
  plugin : String
  job : JobText
  deriving ToJson, FromJson

/-- A Tools `integrations` text as its JSON structure, so the wire carries it
as a value. -/
structure IntegrationsText where
  plugins : List PluginText
  deriving ToJson, FromJson

def IntegrationsText.parse (text : FieldValue) : IntegrationsText :=
  ((Json.parse text).bind fromJson?).toOption.getD { plugins := [] }

def IntegrationsText.text (i : IntegrationsText) : FieldValue :=
  (toJson i).compress

structure JobGuardInput where
  name : String
  writer : String
  selectedBefore : Bool
  nativeAuthorized : Bool
  nativeSourceAuthorized : Bool
  addSchemaTool : Bool
  storedIntegrations : Option IntegrationsText
  candidateIntegrations : IntegrationsText
  deriving ToJson

def JobGuardInput.ofRow (r : JobGuardRow) : JobGuardInput :=
  { name := r.name, writer := if r.operator then "operator" else "model"
  , selectedBefore := r.selectedBefore, nativeAuthorized := r.nativeAuthorized
  , nativeSourceAuthorized := r.nativeSourceAuthorized, addSchemaTool := r.addSchemaTool
  , storedIntegrations := r.stored.map IntegrationsText.parse
  , candidateIntegrations := .parse r.candidate }

def JobGuardInput.accepted (i : JobGuardInput) : Bool :=
  JobGuardRow.accepted
    { name := i.name, operator := i.writer == "operator", selectedBefore := i.selectedBefore
    , nativeAuthorized := i.nativeAuthorized, nativeSourceAuthorized := i.nativeSourceAuthorized
    , addSchemaTool := i.addSchemaTool, stored := i.storedIntegrations.map (·.text)
    , candidate := i.candidateIntegrations.text }

/-- The model writes native document authorization grants and both pass: the
guard alone decides these rows. -/
@[conformance_family]
def selfConfigJobGuard : Family JobGuardInput Bool where
  inputs := (jobGuardRows.filter fun r =>
    !r.operator && r.nativeAuthorized && r.nativeSourceAuthorized).map .ofRow
  model := JobGuardInput.accepted

/-- Native ACP decides these rows: a signed node grants or withholds the
Agent's Tools update and the schema-tool source, so only a served node can
build them. -/
@[conformance_family]
def selfConfigJobGuardNative : Family JobGuardInput Bool where
  inputs := (jobGuardRows.filter fun r => r.name.startsWith "native_agent_update_").map .ofRow
  model := JobGuardInput.accepted

/-- model-production-gap: an operator Tools write bypasses the grant guard, and
no production scenario drives an operator write of a job block. -/
@[conformance_family]
def selfConfigJobGuardUnreached : Family JobGuardInput Bool where
  inputs := (jobGuardRows.filter (·.operator)).map .ofRow
  model := JobGuardInput.accepted

example : (jobGuardRows.filter (·.operator)).all (·.accepted) = true := by native_decide

example :
    ((jobGuardRows.filter fun r => r.name.startsWith "native_agent_update_").any fun r =>
      r.nativeAuthorized && r.nativeSourceAuthorized) = true ∧
    ((jobGuardRows.filter fun r => r.name.startsWith "native_agent_update_").any fun r =>
      !r.nativeAuthorized) = true ∧
    ((jobGuardRows.filter fun r => r.name.startsWith "native_agent_update_").any fun r =>
      r.nativeAuthorized && !r.nativeSourceAuthorized && r.addSchemaTool) = true := by
  native_decide

end Conformance.Contracts
