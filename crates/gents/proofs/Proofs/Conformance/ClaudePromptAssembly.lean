import Proofs.Conformance.Family
import Proofs.Conformance.ContractCases.PromptAssembly
import Proofs.Session.PromptAssembly.ClaudeAttribution
import Proofs.Session.PromptAssembly.ClaudeBody
import Proofs.Session.PromptAssembly.ClaudeCache
import Proofs.Session.PromptAssembly.ClaudeOrigin
import Proofs.Session.PromptAssembly.ClaudeWire

namespace Conformance.ClaudePromptAssembly

open Lean

deriving instance ToJson for PromptAssembly.ClaudeAttribution.Provider

structure AttributionProject where
  id : String
  source : String
  deriving ToJson

structure AttributionInput where
  version : String
  fingerprint : String
  entrypoint : String
  provider : PromptAssembly.ClaudeAttribution.Provider
  firstPartyBaseUrl : Bool
  workload : Option String
  subagent : Bool
  previousRequestId : Option String
  promptId : Option String
  turnOrigin : Option String
  promptIndex : Option Nat
  turnIndex : Option Nat
  gateway : Option String
  project : Option AttributionProject
  deriving ToJson

def AttributionInput.ofFields (f : PromptAssembly.ClaudeAttribution.Fields) : AttributionInput :=
  { version := f.version, fingerprint := f.fingerprint, entrypoint := f.entrypoint
    provider := f.provider, firstPartyBaseUrl := f.firstPartyBaseUrl, workload := f.workload
    subagent := f.subagent, previousRequestId := f.previousRequestId, promptId := f.promptId
    turnOrigin := f.turnOrigin, promptIndex := f.promptIndex, turnIndex := f.turnIndex
    gateway := f.gateway
    project := f.project.map fun (id, source) => ({ id, source } : AttributionProject) }

def AttributionInput.toFields (i : AttributionInput) : PromptAssembly.ClaudeAttribution.Fields :=
  { version := i.version, fingerprint := i.fingerprint, entrypoint := i.entrypoint
    provider := i.provider, firstPartyBaseUrl := i.firstPartyBaseUrl, workload := i.workload
    subagent := i.subagent, previousRequestId := i.previousRequestId, promptId := i.promptId
    turnOrigin := i.turnOrigin, promptIndex := i.promptIndex, turnIndex := i.turnIndex
    gateway := i.gateway, project := i.project.map fun p => (p.id, p.source) }

@[conformance_family]
def attribution : Conformance.Family AttributionInput String where
  inputs := Conformance.ContractCases.promptAssemblyClaudeAttributionCases.map fun c =>
    AttributionInput.ofFields c.fields
  model := fun i => PromptAssembly.ClaudeAttribution.header i.toFields

deriving instance ToJson for PromptAssembly.ClaudeOrigin.Cause,
  PromptAssembly.ClaudeOrigin.Kind, PromptAssembly.ClaudeOrigin.Origin

structure OriginInput where
  origin : PromptAssembly.ClaudeOrigin.Origin
  prompts : Nat
  positionLost : Bool
  deriving ToJson

structure OriginPosition where
  promptIndex : Nat
  turnIndex : Nat
  deriving ToJson

structure OriginObservation where
  turnOrigin : Option String
  turnPosition : Option OriginPosition
  identity : String
  entrypoint : String
  background : Bool
  workload : Option String
  subagent : Bool
  requestClass : String
  mainThreadSource : Bool
  agentic : Bool
  deriving ToJson

def OriginObservation.observe (i : OriginInput) : OriginObservation :=
  let mode := PromptAssembly.ClaudeOrigin.wireMode i.origin
  { turnOrigin := PromptAssembly.ClaudeOrigin.turnOrigin i.origin
    turnPosition :=
      (PromptAssembly.ClaudeOrigin.turnPosition i.origin i.prompts i.positionLost).map
        fun (prompt, turn) => ({ promptIndex := prompt, turnIndex := turn } : OriginPosition)
    identity := PromptAssembly.ClaudeOrigin.identity i.origin
    entrypoint := mode.entrypoint
    background := mode.background
    workload := mode.workload
    subagent := mode.subagent
    requestClass := mode.requestClass.name
    mainThreadSource := mode.mainThreadSource
    agentic := mode.agentic }

@[conformance_family]
def origin : Conformance.Family OriginInput OriginObservation where
  inputs := Conformance.ContractCases.promptAssemblyClaudeOriginCases.flatMap fun c =>
    [false, true].map fun lost =>
      ({ origin := c.origin, prompts := 3, positionLost := lost } : OriginInput)
  model := OriginObservation.observe

deriving instance ToJson for PromptAssembly.ClaudeBody.ModelFacts

inductive FieldConfigKind where
  | adaptive
  | disabled
  | enabled
  deriving ToJson

structure FieldThinkingConfig where
  kind : FieldConfigKind
  mechanical : Bool
  budget : Option Nat
  deriving ToJson

structure FieldRequest where
  thinking : FieldThinkingConfig
  maxTokensOverride : Option Nat
  requestedEffort : Option String
  firstParty : Bool
  firstPartyDefaultUrl : Bool
  experimentalBetasDisabled : Bool
  agentic : Bool
  fork : Bool
  displayUpdates : Bool
  contextManagementBeta : Bool
  perTurnControlBeta : Bool
  previousMessageId : Option String
  deriving ToJson

structure FieldInput where
  facts : PromptAssembly.ClaudeBody.ModelFacts
  request : FieldRequest
  deriving ToJson

def FieldThinkingConfig.toModel (c : FieldThinkingConfig) : PromptAssembly.ClaudeBody.ThinkingConfig :=
  match c.kind with
  | .adaptive => .adaptive
  | .disabled => .disabled c.mechanical
  | .enabled => .enabled c.budget

def FieldThinkingConfig.ofModel : PromptAssembly.ClaudeBody.ThinkingConfig → FieldThinkingConfig
  | .adaptive => { kind := .adaptive, mechanical := false, budget := none }
  | .disabled mechanical => { kind := .disabled, mechanical := mechanical, budget := none }
  | .enabled budget => { kind := .enabled, mechanical := false, budget := budget }

def FieldRequest.ofModel (r : PromptAssembly.ClaudeBody.Request) : FieldRequest :=
  { thinking := FieldThinkingConfig.ofModel r.thinking
    maxTokensOverride := r.maxTokensOverride
    requestedEffort := r.requestedEffort
    firstParty := r.firstParty
    firstPartyDefaultUrl := r.firstPartyDefaultUrl
    experimentalBetasDisabled := r.experimentalBetasDisabled
    agentic := r.agentic
    fork := r.fork
    displayUpdates := r.displayUpdates
    contextManagementBeta := r.contextManagementBeta
    perTurnControlBeta := r.perTurnControlBeta
    previousMessageId := r.previousMessageId }

def FieldRequest.toModel (r : FieldRequest) : PromptAssembly.ClaudeBody.Request :=
  { thinking := r.thinking.toModel
    maxTokensOverride := r.maxTokensOverride
    requestedEffort := r.requestedEffort
    firstParty := r.firstParty
    firstPartyDefaultUrl := r.firstPartyDefaultUrl
    experimentalBetasDisabled := r.experimentalBetasDisabled
    agentic := r.agentic
    fork := r.fork
    displayUpdates := r.displayUpdates
    contextManagementBeta := r.contextManagementBeta
    perTurnControlBeta := r.perTurnControlBeta
    previousMessageId := r.previousMessageId }

inductive FieldThinkingKind where
  | absent
  | disabled
  | adaptive
  | enabled
  deriving ToJson

structure FieldThinking where
  kind : FieldThinkingKind
  budget : Option Nat
  updates : Bool
  deriving ToJson

structure FieldDiagnostics where
  present : Bool
  previousMessageId : Option String
  deriving ToJson

structure FieldOutcome where
  maxTokens : Nat
  thinking : FieldThinking
  temperature : Option Nat
  contextManagement : Bool
  effort : Option String
  diagnostics : FieldDiagnostics
  perTurnEffort : Option String
  deriving ToJson

def FieldThinking.ofModel : Option PromptAssembly.ClaudeBody.Thinking → FieldThinking
  | none => { kind := .absent, budget := none, updates := false }
  | some .disabled => { kind := .disabled, budget := none, updates := false }
  | some (.adaptive updates) => { kind := .adaptive, budget := none, updates := updates }
  | some (.enabled budget updates) => { kind := .enabled, budget := some budget, updates := updates }

def FieldDiagnostics.ofModel : Option (Option String) → FieldDiagnostics
  | none => { present := false, previousMessageId := none }
  | some previous => { present := true, previousMessageId := previous }

def FieldOutcome.ofModel (d : PromptAssembly.ClaudeBody.Decision) : FieldOutcome :=
  { maxTokens := d.maxTokens
    thinking := FieldThinking.ofModel d.thinking
    temperature := d.temperature
    contextManagement := d.contextManagement
    effort := d.effort
    diagnostics := FieldDiagnostics.ofModel d.diagnostics
    perTurnEffort := d.perTurnEffort }

@[conformance_family]
def fieldDecision : Conformance.Family FieldInput FieldOutcome where
  inputs := Conformance.ContractCases.promptAssemblyClaudeFieldCases.map fun c =>
    ({ facts := c.facts, request := FieldRequest.ofModel c.request } : FieldInput)
  model := fun i =>
    FieldOutcome.ofModel (PromptAssembly.ClaudeBody.evaluate i.facts i.request.toModel)

structure FieldRowInput where
  isUser : List Bool
  deriving ToJson

@[conformance_family]
def fieldRow : Conformance.Family FieldRowInput (Option Nat) where
  inputs := Conformance.ContractCases.promptAssemblyClaudeFieldRowCases.map fun c =>
    ({ isUser := c.isUser } : FieldRowInput)
  model := fun i => PromptAssembly.ClaudeBody.rowIndex i.isUser

deriving instance ToJson for PromptAssembly.ClaudeCache.Role

structure CacheBlock where
  kind : String
  text : Option String
  deriving ToJson

structure CacheContent where
  text : Option String
  blocks : Option (List CacheBlock)
  deriving ToJson

structure CacheMessage where
  role : PromptAssembly.ClaudeCache.Role
  content : CacheContent
  deriving ToJson

structure CacheInput where
  caching : Bool
  skipWrite : Bool
  messages : List CacheMessage
  deriving ToJson

structure CachePlaced where
  role : PromptAssembly.ClaudeCache.Role
  text : Option String
  blocks : Option (List CacheBlock)
  marker : Option Nat
  deriving ToJson

def CacheBlock.ofModel : PromptAssembly.ClaudeCache.Block → CacheBlock
  | .text text => { kind := "text", text := some text }
  | .thinking => { kind := "thinking", text := none }
  | .redactedThinking => { kind := "redacted_thinking", text := none }
  | .fallback => { kind := "fallback", text := none }
  | .other kind => { kind := kind, text := none }

def CacheBlock.toModel (b : CacheBlock) : PromptAssembly.ClaudeCache.Block :=
  match b.kind with
  | "text" => .text (b.text.getD "")
  | "thinking" => .thinking
  | "redacted_thinking" => .redactedThinking
  | "fallback" => .fallback
  | kind => .other kind

def CacheContent.ofModel : PromptAssembly.ClaudeCache.Content → CacheContent
  | .str text => { text := some text, blocks := none }
  | .blocks blocks => { text := none, blocks := some (blocks.map CacheBlock.ofModel) }

def CacheContent.toModel (c : CacheContent) : PromptAssembly.ClaudeCache.Content :=
  match c.blocks with
  | some blocks => .blocks (blocks.map CacheBlock.toModel)
  | none => .str (c.text.getD "")

def CacheMessage.ofModel (m : PromptAssembly.ClaudeCache.Message) : CacheMessage :=
  { role := m.role, content := CacheContent.ofModel m.content }

def CacheMessage.toModel (m : CacheMessage) : PromptAssembly.ClaudeCache.Message :=
  { role := m.role, content := m.content.toModel }

def CachePlaced.ofModel (p : PromptAssembly.ClaudeCache.Placed) : CachePlaced :=
  { role := p.role, text := p.text
    blocks := p.blocks.map fun blocks => blocks.map CacheBlock.ofModel
    marker := p.marker }

@[conformance_family]
def cachePlacement : Conformance.Family CacheInput (List CachePlaced) where
  inputs := Conformance.ContractCases.promptAssemblyClaudeCacheCases.map fun c =>
    ({ caching := c.caching, skipWrite := c.skipWrite
       messages := c.messages.map CacheMessage.ofModel } : CacheInput)
  model := fun i =>
    (PromptAssembly.ClaudeCache.place i.caching i.skipWrite (i.messages.map CacheMessage.toModel)).map
      CachePlaced.ofModel

inductive StartSignatureKind where
  | text
  | number
  deriving ToJson

structure StartSignature where
  kind : StartSignatureKind
  text : Option String
  number : Option Nat
  deriving ToJson

inductive StartEventKind where
  | thinkingDelta
  | signatureDelta
  | contentStop
  deriving ToJson

structure StartEvent where
  kind : StartEventKind
  index : Nat
  fragment : Option String
  deriving ToJson

structure StartInput where
  index : Nat
  thinking : String
  signature : Option StartSignature
  later : List StartEvent
  deriving ToJson

inductive StartOutcomeKind where
  | wireError
  | contentError
  | ok
  deriving ToJson

inductive StartPartKind where
  | text
  | redacted
  | encrypted
  | summary
  deriving ToJson

inductive StartBlockKind where
  | text
  | reasoning
  | toolUse
  deriving ToJson

structure StartPart where
  kind : StartPartKind
  payload : String
  signature : Option String
  deriving ToJson

structure StartBlock where
  kind : StartBlockKind
  value : Option String
  parts : Option (List StartPart)
  id : Option Nat
  name : Option String
  arguments : Option String
  deriving ToJson

structure StartOutcome where
  kind : StartOutcomeKind
  error : Option String
  content : List StartBlock
  deriving ToJson

def StartSignature.ofValue? : Json → Option StartSignature
  | .str value => some { kind := .text, text := some value, number := none }
  | .num value => some { kind := .number, text := none, number := some value.mantissa.toNat }
  | _ => none

def StartSignature.toValue (s : StartSignature) : Json :=
  match s.kind with
  | .text => Json.str (s.text.getD "")
  | .number => Lean.toJson (s.number.getD 0)

def StartEvent.ofModel? : PromptAssembly.ClaudeMap.StreamEvent → Option StartEvent
  | .thinkingDelta index fragment =>
      some { kind := .thinkingDelta, index := index, fragment := some fragment }
  | .signatureDelta index fragment =>
      some { kind := .signatureDelta, index := index, fragment := some fragment }
  | .contentStop index => some { kind := .contentStop, index := index, fragment := none }
  | _ => none

def StartEvent.toModel (e : StartEvent) : PromptAssembly.ClaudeMap.StreamEvent :=
  match e.kind with
  | .thinkingDelta => .thinkingDelta e.index (e.fragment.getD "")
  | .signatureDelta => .signatureDelta e.index (e.fragment.getD "")
  | .contentStop => .contentStop e.index

def StartInput.ofModel? (start : PromptAssembly.ClaudeWire.ThinkingStart)
    (later : List PromptAssembly.ClaudeMap.StreamEvent) : Option StartInput := do
  let signature ← (match start.signature with
    | none => (some none : Option (Option StartSignature))
    | some value => (StartSignature.ofValue? value).map some)
  let later ← later.mapM StartEvent.ofModel?
  pure { index := start.index, thinking := start.thinking, signature := signature, later := later }

def StartInput.toStart (i : StartInput) : PromptAssembly.ClaudeWire.ThinkingStart :=
  { index := i.index, thinking := i.thinking, signature := i.signature.map StartSignature.toValue }

def StartPart.ofModel : CanonicalOutput.ReasoningPart String → StartPart
  | .text payload signature => { kind := .text, payload := payload, signature := signature }
  | .redacted payload => { kind := .redacted, payload := payload, signature := none }
  | .encrypted payload => { kind := .encrypted, payload := payload, signature := none }
  | .summary payload => { kind := .summary, payload := payload, signature := none }

def StartBlock.ofModel : PromptAssembly.ClaudeMap.StreamBlock → StartBlock
  | .text value =>
      { kind := .text, value := some value, parts := none, id := none, name := none
        arguments := none }
  | .reasoning parts =>
      { kind := .reasoning, value := none, parts := some (parts.map StartPart.ofModel), id := none
        name := none, arguments := none }
  | .toolUse id name arguments =>
      { kind := .toolUse, value := none, parts := none, id := some id, name := some name
        arguments := some arguments }

def StartOutcome.ofModel :
    Except PromptAssembly.ClaudeWire.EvaluationError
      (List PromptAssembly.ClaudeMap.ContentStep × List PromptAssembly.ClaudeMap.StreamBlock) →
    StartOutcome
  | .error (.wire _) => { kind := .wireError, error := none, content := [] }
  | .error (.content error) =>
      { kind := .contentError, error := some (PromptAssembly.ClaudeMap.errorName error)
        content := [] }
  | .ok (_, content) => { kind := .ok, error := none, content := content.map StartBlock.ofModel }

@[conformance_family]
def wireStart : Conformance.Family StartInput StartOutcome where
  inputs := PromptAssembly.ClaudeWire.cases.filterMap fun c => StartInput.ofModel? c.start c.later
  model := fun i =>
    StartOutcome.ofModel (PromptAssembly.ClaudeWire.evaluate i.toStart (i.later.map StartEvent.toModel))

open Conformance.ContractCases in
def bodyCase? {α : Type} [ToJson α] (key : PromptAssemblyClaudeBodyCase → α) (i : α) :
    Option PromptAssemblyClaudeBodyCase :=
  promptAssemblyClaudeBodyCases.find? fun c => toJson (key c) == toJson i

open Conformance.ContractCases in
example : promptAssemblyClaudeBodyCases.all (fun c =>
    c.system.head? == some c.attribution &&
      c.system[1]? == some (PromptAssembly.ClaudeOrigin.identity c.origin)) := by
  native_decide

structure BodySystemInput where
  attribution : String
  preamble : Option String
  rows : List String
  deriving ToJson

structure BodySystem where
  system : List String
  systemMarked : List Bool
  deriving ToJson

open Conformance.ContractCases in
def BodySystemInput.ofCase (c : PromptAssemblyClaudeBodyCase) : BodySystemInput :=
  { attribution := c.attribution, preamble := c.preamble, rows := c.rows }

/-- The system blocks the attribution header, the identity, the preamble and the
system rows split into. -/
@[conformance_family]
def bodySystem : Conformance.Family BodySystemInput BodySystem where
  inputs := Conformance.ContractCases.promptAssemblyClaudeBodyCases.map BodySystemInput.ofCase
  model := fun i => match bodyCase? BodySystemInput.ofCase i with
    | some c => { system := c.system, systemMarked := c.systemMarked }
    | none => { system := [], systemMarked := [] }

structure BodyEffortInput where
  model : String
  supportedEfforts : Option (List String)
  effort : Option String
  deriving ToJson

structure BodyEffort where
  bodyEffort : Option String
  deriving ToJson

open Conformance.ContractCases in
def BodyEffortInput.ofCase (c : PromptAssemblyClaudeBodyCase) : BodyEffortInput :=
  { model := c.model, supportedEfforts := c.supportedEfforts, effort := c.effort }

@[conformance_family]
def bodyEffort : Conformance.Family BodyEffortInput BodyEffort where
  inputs := Conformance.ContractCases.promptAssemblyClaudeBodyCases.map BodyEffortInput.ofCase
  model := fun i => { bodyEffort := (bodyCase? BodyEffortInput.ofCase i).bind (·.decision.effort) }

structure BodyPlanInput where
  model : String
  firstUserText : String
  fingerprintPlaceholder : String
  promptId : String
  preamble : Option String
  rows : List String
  tools : List String
  supportedEfforts : Option (List String)
  effort : Option String
  /-- The request fields the model evaluates; the plan derives them from the same session. -/
  request : FieldRequest
  deriving ToJson

structure BodyThinking where
  kind : String
  budget : Option Nat
  updates : Bool
  deriving ToJson

structure BodyPlan where
  system : List String
  systemMarked : List Bool
  toolsPresent : Bool
  facts : PromptAssembly.ClaudeBody.ModelFacts
  maxTokens : Nat
  thinking : BodyThinking
  temperature : Option Nat
  contextManagement : Bool
  bodyEffort : Option String
  diagnostics : FieldDiagnostics
  perTurnEffort : Option String
  deriving ToJson

def BodyThinking.ofModel : Option PromptAssembly.ClaudeBody.Thinking → BodyThinking
  | none => { kind := "none", budget := none, updates := false }
  | some .disabled => { kind := "disabled", budget := none, updates := false }
  | some (.adaptive updates) => { kind := "adaptive", budget := none, updates }
  | some (.enabled budget updates) => { kind := "enabled", budget := some budget, updates }

open Conformance.ContractCases in
def BodyPlanInput.ofCase (c : PromptAssemblyClaudeBodyCase) : BodyPlanInput :=
  { model := c.model, firstUserText := c.firstUserText
    fingerprintPlaceholder := claudeFingerprintPlaceholder, promptId := c.promptId
    preamble := c.preamble, rows := c.rows, tools := c.tools
    supportedEfforts := c.supportedEfforts, effort := c.effort
    request := FieldRequest.ofModel c.request }

open Conformance.ContractCases in
def BodyPlan.ofCase (c : PromptAssemblyClaudeBodyCase) : BodyPlan :=
  { system := c.system, systemMarked := c.systemMarked, toolsPresent := c.toolsPresent
    facts := c.facts, maxTokens := c.decision.maxTokens
    thinking := BodyThinking.ofModel c.decision.thinking
    temperature := c.decision.temperature
    contextManagement := c.decision.contextManagement
    bodyEffort := c.decision.effort
    diagnostics := FieldDiagnostics.ofModel c.decision.diagnostics
    perTurnEffort := c.decision.perTurnEffort }

/-- The whole Messages request plan for an interactive first-party turn. -/
@[conformance_family]
def bodyPlan : Conformance.Family BodyPlanInput BodyPlan where
  inputs := Conformance.ContractCases.promptAssemblyClaudeBodyCases.map BodyPlanInput.ofCase
  model := fun i => match bodyCase? BodyPlanInput.ofCase i with
    | some c => BodyPlan.ofCase c
    | none =>
      { system := [], systemMarked := [], toolsPresent := false
        facts := PromptAssembly.ClaudeBody.sonnet5, maxTokens := 0
        thinking := BodyThinking.ofModel none, temperature := none, contextManagement := false
        bodyEffort := none, diagnostics := FieldDiagnostics.ofModel none, perTurnEffort := none }

end Conformance.ClaudePromptAssembly
