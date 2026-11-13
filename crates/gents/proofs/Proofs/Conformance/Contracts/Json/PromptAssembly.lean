import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Conformance.Contracts.Json.FileInventoryClosure
import Proofs.Conformance.ContractCases.PromptAssembly
import Proofs.Session.Compaction.Executable
import Proofs.Session.PromptAssembly.ClaudeWire
import Proofs.Session.PromptAssembly.ResponsesStorage
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

structure RoutingAffinityInput where
  codex : Bool
  main : Bool
  executionSession : Option String
  responses : List (Option String)
  deriving Lean.ToJson

structure RoutingAffinityOutput where
  expectedSession : Option String
  expectedToken : Option String
  deriving Lean.ToJson

def RoutingAffinityInput.state (i : RoutingAffinityInput) : PromptAssembly.Provider.RoutingAffinity :=
  i.responses.foldl PromptAssembly.Provider.RoutingAffinity.observe
    (PromptAssembly.Provider.RoutingAffinity.start i.codex i.main i.executionSession)

@[conformance_family]
def routingAffinity : Family RoutingAffinityInput RoutingAffinityOutput where
  inputs := routingAffinityCases.map fun row =>
    { codex := row.codex, main := row.main, executionSession := row.session
      responses := row.responses }
  model := fun i => { expectedSession := i.state.session, expectedToken := i.state.token }

def routingAffinityDispatchTokens (state : PromptAssembly.Provider.RoutingAffinity) :
    List (Option String) → List (Option String)
  | [] => []
  | response :: rest =>
      state.token :: routingAffinityDispatchTokens (state.observe response) rest

/-- The owned loop's main Codex execution in one session: each provider dispatch
reads the execution's token, then observes the server token that response
carries. A failed stream, a tool continuation and a nested compaction all stay
in that one execution. -/
structure RoutingAffinityTrace where
  responses : List (Option String)
  deriving Lean.ToJson

@[conformance_family]
def routingAffinityDispatches : Family RoutingAffinityTrace (List (Option String)) where
  inputs := [{ responses := [some "first", some "first", some "first"] },
    { responses := [some "first", some "later", none] }]
  model := fun i =>
    routingAffinityDispatchTokens
      (PromptAssembly.Provider.RoutingAffinity.start true true (some "session")) i.responses

structure RetryFrontierInput where
  name : String
  scopedTerminal : Bool
  sameSession : Bool
  sameRequester : Bool
  running : Bool
  background : Bool
  published : Bool
  deriving Lean.ToJson

/-- Whether a retry successor resumes from its parent's published input:
production `retry_has_published_input` on a served node. -/
@[conformance_family]
def retryFrontierAdmission : Family RetryFrontierInput (Option Bool) where
  inputs :=
    [("before-input", true, true, true, false, false, false),
     ("ready", true, true, true, false, false, true),
     ("live-parent", false, true, true, false, false, true),
     ("foreign-requester", true, true, false, false, false, true),
     ("unsettled", true, true, true, true, false, true),
     ("background", true, true, true, true, true, true),
     ("resend", true, false, true, false, false, true),
     ("resend-other-requester", true, false, false, false, false, true)].map
      fun (name, scopedTerminal, sameSession, sameRequester, running, background, published) =>
        { name, scopedTerminal, sameSession, sameRequester, running, background, published }
  model := fun i =>
    PromptAssembly.CurrentInput.admitResume i.scopedTerminal i.sameSession i.sameRequester
      (PromptAssembly.CurrentInput.settledForResume i.running i.background) i.published

structure RetryEntryInput where
  resume : Bool
  history : List Nat
  context : Option Nat
  authored : Nat
  deriving Lean.ToJson

def retryEntryInputs : List RetryEntryInput :=
  [ (false, ([] : List Nat), (none : Option Nat)), (false, [1, 2, 3], none),
    (true, [1], none), (true, [1, 2, 3], none), (true, [1, 2, 3, 4], none),
    (true, [1, 2, 3], some 11), (false, [1], some 11) ].map fun (resume, history, context) =>
    { resume, history, context, authored := 9 }

/-- The retry continuation prompt is message `10`; message `4` is an assistant
turn, so a history ending in it has an assistant tail. -/
def retryEntryMessages (i : RetryEntryInput) : List Nat :=
  PromptAssembly.CurrentInput.entryWithContext i.resume i.history i.authored 10 i.context
    (i.history.getLast? == some 4)

/-- The provider entry a request starts from: production `request_entry`. -/
@[conformance_family]
def retryEntryFrontier : Family RetryEntryInput (List Nat) where
  inputs := retryEntryInputs
  model := retryEntryMessages

structure RetryEntryPublication where
  publish : Bool
  deriving Lean.ToJson

/-- Whether the owned loop announces the authored prompt for publication:
production `run_loop_stream` with `resume_from_history`. -/
@[conformance_family]
def retryEntryPublish : Family RetryEntryInput RetryEntryPublication where
  inputs := retryEntryInputs
  model := fun i => { publish := PromptAssembly.CurrentInput.publishesAuthoredInput i.resume }

structure RetryEntryCaptureOutput where
  promptCopies : Nat
  toolResults : Nat
  publish : Bool
  deriving Lean.ToJson

/-- A served runtime retries a failed parent whose recorded frontier is the
prompt (`1`), a tool call (`2`) and its result (`3`); the provider request it
renders is captured. Only that retry is built end to end: the other entries
need frontiers a served runtime does not leave behind on failure. -/
@[conformance_family]
def retryEntryCapture : Family RetryEntryInput RetryEntryCaptureOutput where
  inputs := retryEntryInputs.filter fun i => i.resume && i.history == [1, 2, 3] && i.context.isNone
  model := fun i =>
    { promptCopies := (retryEntryMessages i).count 1
      toolResults := (retryEntryMessages i).count 3
      publish := PromptAssembly.CurrentInput.publishesAuthoredInput i.resume }

deriving instance Lean.ToJson for CurrentInputHeaderCase, CurrentInputCase

@[conformance_family]
def currentInputSelection : Family CurrentInputCase (List Nat) where
  inputs := currentInputCases
  model := CurrentInputCase.retainedIndices

deriving instance Lean.ToJson for WakeInputRow

structure WakeInputWire where
  name : String
  claimed : List String
  acknowledged : List String
  rows : List WakeInputRow
  notifications : List WakeInputRow
  deriving Lean.ToJson

def WakeInputWire.ofCase (c : WakeInputCase) : WakeInputWire :=
  { name := c.name, claimed := c.claimed, acknowledged := c.acknowledged
    rows := c.history, notifications := c.notifications }

def WakeInputWire.currentKeys (i : WakeInputWire) : List String :=
  PromptAssembly.CurrentInput.wakeNotificationKeys i.claimed i.acknowledged

def WakeInputWire.expectedOrder (i : WakeInputWire) : List String :=
  (PromptAssembly.CurrentInput.wakeEntry
    (fun row => row.notificationKey.any i.currentKeys.contains) i.rows i.notifications).map
      (·.header)

structure WakeInputOutput where
  currentKeys : List String
  expectedOrder : List String
  deriving Lean.ToJson

/-- The notification keys a wake still owes and the provider order of its
history and notifications: production `current_keys` and `project`. -/
@[conformance_family]
def wakeInputProjection : Family WakeInputWire WakeInputOutput where
  inputs := wakeInputCases.map .ofCase
  model := fun i => { currentKeys := i.currentKeys, expectedOrder := i.expectedOrder }

structure WakeProviderOrderInput where
  wake : WakeInputWire
  /-- The headers whose content the served daemon's captured provider input
  can locate. -/
  observable : List String
  deriving Lean.ToJson

/-- A served daemon runs a background wake and then its successor; the
successor's captured provider input is ordered. Only the successor scenario is
built: it is the case whose previous wake left an answer and a new
notification behind. -/
@[conformance_family]
def wakeInputProviderOrder : Family WakeProviderOrderInput (List String) where
  inputs := (wakeInputCases.filter (·.name == "successor_wake_moves_only_new_notification")).map
    fun c => { wake := .ofCase c, observable := ["notification-a", "notification-b", "answer-0"] }
  model := fun i => i.wake.expectedOrder.filter i.observable.contains

/-- A served node claims a background wake whose session also holds a
notification imported from another session. Only that case is built: it is
the case about a claimed key whose notification came from elsewhere. -/
@[conformance_family]
def wakeInputClaimedKeys : Family WakeInputWire (List String) where
  inputs := (wakeInputCases.filter (·.name == "imported_notification_stays_in_history")).map
    .ofCase
  model := WakeInputWire.currentKeys

def jsonNatArray (values : List Nat) : String :=
  jsonArray (values.map toString)

deriving instance Lean.ToJson for PromptAssemblyItemCase, PromptAssemblyRowCase,
  PromptAssembly.Content.OrderMode, PromptAssemblyAssistantOrderCase

/-- The model item an emitted content item names. -/
def _root_.Conformance.ContractCases.PromptAssemblyItemCase.toModel (item : PromptAssemblyItemCase) : PromptAssembly.Content.Item :=
  match item.item with
  | "text" => .text item.value
  | "other" => .other item.value
  | _ => .call item.value

structure AssistantOrderOutput where
  expected : List PromptAssemblyItemCase
  twice : List PromptAssemblyItemCase
  deriving Lean.ToJson

/-- One assistant turn's content order for a provider order mode, once and
again: production `normalize_assistant_content_order`. -/
@[conformance_family]
def promptAssemblyAssistantOrder :
    Family PromptAssemblyAssistantOrderCase AssistantOrderOutput where
  inputs := promptAssemblyAssistantOrderCases
  model := fun i =>
    let once := PromptAssembly.Content.normalizeFor i.orderMode (i.input.map (·.toModel))
    { expected := once.map itemCase
      twice := (PromptAssembly.Content.normalizeFor i.orderMode once).map itemCase }

deriving instance Lean.ToJson for PromptAssemblySanitizeCase

/-- The model row an emitted row names, at its position in the transcript. The
emitted witnesses number their rows by position, so the position is the row's
message id and sequence. -/
def _root_.Conformance.ContractCases.PromptAssemblyRowCase.toModel (sequence : Nat) (row : PromptAssemblyRowCase) :
    PromptAssembly.Provider.ProviderRow :=
  let role : Transcript.MessageRole := if row.role == "assistant" then .assistant else .user
  let kind : Transcript.MessageKind := match row.kind, row.callIds with
    | "assistantToolCalls", ids => .assistantToolCalls ids.toFinset
    | "toolResult", [callId] => .toolResult callId ⟨0, callId, 0⟩
    | _, _ => .ordinary
  { row := ⟨sequence, 0, sequence, role, kind⟩, content := row.content.map (·.toModel) }

def promptAssemblyRowsToModel (rows : List PromptAssemblyRowCase) :
    List PromptAssembly.Provider.ProviderRow :=
  rows.zipIdx.map fun (row, index) => row.toModel index

structure SanitizeOutput where
  expected : List PromptAssemblyRowCase
  twice : List PromptAssemblyRowCase
  deriving Lean.ToJson

/-- A loaded transcript sanitized for a grouped provider, once and again:
production `sanitize_history_for_provider`. -/
@[conformance_family]
def promptAssemblySanitize : Family PromptAssemblySanitizeCase SanitizeOutput where
  inputs := promptAssemblySanitizeCases
  model := fun i =>
    let once := PromptAssembly.Provider.sanitizeForProviderGlobal
      (promptAssemblyRowsToModel i.input)
    { expected := rowCases once
      twice := rowCases (PromptAssembly.Provider.sanitizeForProviderGlobal once) }

structure SanitizeSplitInput where
  name : String
  input : List PromptAssemblyRowCase
  index : Nat
  deriving Lean.ToJson

/-- The suffix of a loaded transcript from each position, sanitized on its own:
production `sanitize_history_for_provider` on a history that starts mid-turn. -/
@[conformance_family]
def promptAssemblySanitizeSplit : Family SanitizeSplitInput (List PromptAssemblyRowCase) where
  inputs := promptAssemblySanitizeCases.flatMap fun c =>
    (List.range (c.input.length + 1)).map fun index => { name := c.name, input := c.input, index }
  model := fun i =>
    rowCases (PromptAssembly.Provider.sanitizeForProviderGlobal
      ((promptAssemblyRowsToModel i.input).drop i.index))

deriving instance Lean.ToJson for PromptAssemblyModeSanitizeCase

structure ModeSanitizeOutput where
  expected : List PromptAssemblyRowCase
  twice : List PromptAssemblyRowCase
  association : List PromptAssemblyRowCase
  associationIndices : List (List Nat)
  deriving Lean.ToJson

/-- A loaded transcript sanitized for a provider order mode, once and again,
and the provider-neutral association the owned loop keeps beside it with each
row's retained source block indices: production
`sanitize_history_for_provider`, `provider_view_tagged` and
`sanitize_tagged_history`. -/
@[conformance_family]
def promptAssemblyModeSanitize : Family PromptAssemblyModeSanitizeCase ModeSanitizeOutput where
  inputs := promptAssemblyModeSanitizeCases
  model := fun i =>
    let rows := promptAssemblyRowsToModel i.input
    let once := PromptAssembly.Provider.sanitizeForProviderGlobalFor i.orderMode rows
    let association := PromptAssembly.Provider.sanitizeForProviderGlobalFor .nativePreserved rows
    { expected := rowCases once
      twice := rowCases (PromptAssembly.Provider.sanitizeForProviderGlobalFor i.orderMode once)
      association := rowCases association
      associationIndices := association.map (retainedBlockIndices rows) }

structure PromptAssemblyLayerInput where
  skillCount : Nat
  summaryCount : Nat
  conversationLen : Nat
  deriving Lean.ToJson

@[conformance_family]
def promptAssemblyLayer : Family PromptAssemblyLayerInput (List String) where
  inputs := promptAssemblyLayerCases.map fun c =>
    { skillCount := c.skillCount, summaryCount := c.summaryCount
      conversationLen := c.conversationLen }
  model := fun i =>
    (PromptAssembly.assemble i.skillCount i.summaryCount i.conversationLen).map slotName

structure PromptAssemblyBudgetInput where
  contextWindow : Nat
  maxOutputTokens : Nat
  thresholdBasisPoints : Nat
  promptTokens : Nat
  requestTokens : Nat
  deriving Lean.ToJson

structure PromptAssemblyBudgetOutput where
  configuredThresholdBudget : Nat
  effectiveInputBudget : Nat
  compactionInputBudget : Nat
  effectiveOutputTokens : Nat
  shouldCompact : Bool
  providerSafe : Bool
  canDispatch : Bool
  deriving Lean.ToJson

@[conformance_family]
def promptAssemblyBudget : Family PromptAssemblyBudgetInput PromptAssemblyBudgetOutput where
  inputs := promptAssemblyBudgetCases.map fun c =>
    { contextWindow := c.contextWindow, maxOutputTokens := c.maxOutputTokens
      thresholdBasisPoints := c.thresholdBasisPoints, promptTokens := c.promptTokens
      requestTokens := c.requestTokens }
  model := fun i =>
    let c := budgetCase
      { name := "", contextWindow := i.contextWindow, maxOutputTokens := i.maxOutputTokens
        thresholdBasisPoints := i.thresholdBasisPoints, promptTokens := i.promptTokens
        requestTokens := i.requestTokens }
    { configuredThresholdBudget := c.configuredThresholdBudget
      effectiveInputBudget := c.effectiveInputBudget
      compactionInputBudget := c.compactionInputBudget
      effectiveOutputTokens := c.effectiveOutputTokens
      shouldCompact := c.shouldCompact
      providerSafe := c.providerSafe
      canDispatch := c.canDispatch }

structure PromptAssemblyTurnBudgetInput where
  contextWindow : Nat
  maxOutputTokens : Nat
  thresholdBasisPoints : Nat
  turnInputTokens : List Nat
  deriving Lean.ToJson

structure PromptAssemblyTurnBudgetOutput where
  configuredThresholdBudget : Nat
  effectiveInputBudget : Nat
  compactionInputBudget : Nat
  turnOutputTokens : List Nat
  turnShouldCompact : List Bool
  turnCanDispatch : List Bool
  deriving Lean.ToJson

@[conformance_family]
def promptAssemblyTurnBudget :
    Family PromptAssemblyTurnBudgetInput PromptAssemblyTurnBudgetOutput where
  inputs := promptAssemblyTurnBudgetCases.map fun c =>
    { contextWindow := c.contextWindow, maxOutputTokens := c.maxOutputTokens
      thresholdBasisPoints := c.thresholdBasisPoints, turnInputTokens := c.turnInputTokens }
  model := fun i =>
    let c := turnBudgetCase
      { name := "", contextWindow := i.contextWindow, maxOutputTokens := i.maxOutputTokens
        thresholdBasisPoints := i.thresholdBasisPoints, turnInputTokens := i.turnInputTokens }
    { configuredThresholdBudget := c.configuredThresholdBudget
      effectiveInputBudget := c.effectiveInputBudget
      compactionInputBudget := c.compactionInputBudget
      turnOutputTokens := c.turnOutputTokens
      turnShouldCompact := c.turnShouldCompact
      turnCanDispatch := c.turnCanDispatch }

structure ObservedCount where
  provider : Nat
  estimate : Nat
  deriving Lean.ToJson

structure AnchorWire where
  inputTokens : Nat
  outputTokens : Nat
  estimatedInputTokens : Nat
  deriving Lean.ToJson

def AnchorWire.toModel (a : AnchorWire) : PromptAssembly.ContextCalibration.Anchor :=
  { input := a.inputTokens, output := a.outputTokens, estimated := a.estimatedInputTokens }

structure CompactionTriggerInput where
  name : String
  contextWindow : Nat
  maxOutputTokens : Nat
  thresholdBasisPoints : Nat
  priorBasisPoints : Nat
  seededObservations : List Nat
  observed : List ObservedCount
  anchor : Option AnchorWire
  invalidate : Bool
  rawInputTokens : Nat
  deriving Lean.ToJson

deriving instance Lean.ToJson for PromptAssembly.ContextCalibration.Admission

structure CompactionTriggerOutput where
  ratioBasisPoints : Nat
  calibratedTokens : Nat
  anchoredTokens : Option Nat
  contextTokens : Nat
  compactionInputBudget : Nat
  admission : Option PromptAssembly.ContextCalibration.Admission
  deriving Lean.ToJson

open PromptAssembly.ContextCalibration in
/-- The calibrated context size of a raw estimate, the compaction budget and
the trigger's admission: production `ContextCalibration::context`,
`compaction_input_budget` and `ReductionAdmission::for_context`. -/
@[conformance_family]
def promptAssemblyCompactionTrigger : Family CompactionTriggerInput CompactionTriggerOutput where
  inputs := promptAssemblyCompactionTriggerCases.map fun w =>
    { name := w.name, contextWindow := w.contextWindow, maxOutputTokens := w.maxOutputTokens
      thresholdBasisPoints := w.thresholdBasisPoints, priorBasisPoints := w.priorBasisPoints
      seededObservations := w.seededObservations
      observed := w.observed.map fun (provider, estimate) => { provider, estimate }
      anchor := w.anchor.map fun a =>
        { inputTokens := a.input, outputTokens := a.output, estimatedInputTokens := a.estimated }
      invalidate := w.invalidate, rawInputTokens := w.rawInputTokens }
  model := fun i =>
    let seeded := { seed i.priorBasisPoints i.seededObservations with
      anchor := i.anchor.map AnchorWire.toModel }
    let observedState := i.observed.foldl
      (fun state count => state.observe count.provider count.estimate) seeded
    let state := if i.invalidate then observedState.invalidate else observedState
    { ratioBasisPoints := state.ratio
      calibratedTokens := calibrated i.rawInputTokens state.ratio
      anchoredTokens := state.anchor.bind fun anchor =>
        anchored anchor i.rawInputTokens state.ratio
      contextTokens := state.tokens i.rawInputTokens
      compactionInputBudget := PromptAssembly.Budget.compactionInputBudget
        i.contextWindow i.thresholdBasisPoints i.maxOutputTokens
      admission := state.admission i.rawInputTokens i.contextWindow
        i.thresholdBasisPoints i.maxOutputTokens }

structure ClaudeBlockWire where
  kind : String
  id : Option Nat
  name : Option String
  deriving Lean.ToJson, BEq

def ClaudeBlockWire.ofBlock : PromptAssembly.ClaudeMap.Block → ClaudeBlockWire
  | .text => { kind := "text", id := none, name := none }
  | .toolUse id name => { kind := "toolUse", id := some id, name := some name }
  | .toolResult id => { kind := "toolResult", id := some id, name := none }

structure ClaudeMapInput where
  surface : List String
  blocks : List ClaudeBlockWire
  deriving Lean.ToJson, BEq

structure ClaudeMapOutput where
  outcome : String
  ids : List Nat
  deriving Lean.ToJson

def ClaudeMapInput.ofWitness (w : List String × List PromptAssembly.ClaudeMap.Block) :
    ClaudeMapInput :=
  { surface := w.1, blocks := w.2.map ClaudeBlockWire.ofBlock }

def claudeMapOutput (surface : List String) (blocks : List PromptAssembly.ClaudeMap.Block) :
    ClaudeMapOutput :=
  match PromptAssembly.ClaudeMap.mapTurn surface.toFinset blocks with
  | .ok _ => { outcome := "ok", ids := (PromptAssembly.ClaudeMap.toolUsePairs blocks).map (·.1) }
  | .error e => { outcome := PromptAssembly.ClaudeMap.errorName e, ids := [] }

@[conformance_family]
def claudeMap : Family ClaudeMapInput ClaudeMapOutput where
  inputs := promptAssemblyClaudeMapWitnesses.map ClaudeMapInput.ofWitness
  model := fun i =>
    match promptAssemblyClaudeMapWitnesses.find? (fun w => ClaudeMapInput.ofWitness w == i) with
    | some w => claudeMapOutput w.1 w.2
    | none => { outcome := "unknown", ids := [] }

structure ClaudeStreamEventWire where
  kind : String
  id : Option Nat
  name : Option String
  input : Option String
  fragment : Option String
  index : Option Nat
  value : Option String
  data : Option String
  deriving Lean.ToJson, BEq

def ClaudeStreamEventWire.ofEvent : PromptAssembly.ClaudeMap.StreamEvent → ClaudeStreamEventWire
  | .text value =>
      { kind := "text", id := none, name := none, input := none, fragment := none
        index := none, value := some value, data := none }
  | .start id name input =>
      { kind := "toolStart", id := some id, name := some name, input := input, fragment := none
        index := none, value := none, data := none }
  | .delta fragment =>
      { kind := "toolDelta", id := none, name := none, input := none, fragment := some fragment
        index := none, value := none, data := none }
  | .stop =>
      { kind := "toolStop", id := none, name := none, input := none, fragment := none
        index := none, value := none, data := none }
  | .thinkingStart index initialText =>
      { kind := "thinkingStart", id := none, name := none, input := none, fragment := none
        index := some index, value := some initialText, data := none }
  | .thinkingDelta index fragment =>
      { kind := "thinkingDelta", id := none, name := none, input := none
        fragment := some fragment, index := some index, value := none, data := none }
  | .signatureDelta index fragment =>
      { kind := "signatureDelta", id := none, name := none, input := none
        fragment := some fragment, index := some index, value := none, data := none }
  | .redactedStart index data =>
      { kind := "redactedStart", id := none, name := none, input := none, fragment := none
        index := some index, value := none, data := some data }
  | .contentStop index =>
      { kind := "contentStop", id := none, name := none, input := none, fragment := none
        index := some index, value := none, data := none }

structure ClaudeStreamInput where
  surface : List String
  events : List ClaudeStreamEventWire
  deriving Lean.ToJson, BEq

def ClaudeStreamInput.ofWitness (w : List String × List PromptAssembly.ClaudeMap.StreamEvent) :
    ClaudeStreamInput :=
  { surface := w.1, events := w.2.map ClaudeStreamEventWire.ofEvent }

structure ClaudeStreamCall where
  id : Nat
  arguments : String
  deriving Lean.ToJson

structure ClaudeStreamOutput where
  outcome : String
  calls : List ClaudeStreamCall
  deriving Lean.ToJson

def claudeStreamOutput (surface : List String)
    (events : List PromptAssembly.ClaudeMap.StreamEvent) : ClaudeStreamOutput :=
  match PromptAssembly.ClaudeMap.runStream surface.toFinset events with
  | .ok calls =>
      { outcome := "ok", calls := calls.map fun (id, arguments) => { id, arguments } }
  | .error e => { outcome := PromptAssembly.ClaudeMap.errorName e, calls := [] }

@[conformance_family]
def claudeStream : Family ClaudeStreamInput ClaudeStreamOutput where
  inputs := promptAssemblyClaudeStreamWitnesses.map ClaudeStreamInput.ofWitness
  model := fun i =>
    match promptAssemblyClaudeStreamWitnesses.find?
        (fun w => ClaudeStreamInput.ofWitness w == i) with
    | some w => claudeStreamOutput w.1 w.2
    | none => { outcome := "unknown", calls := [] }

private def claudeReasoningPartJson {α : Type} (payloadJson : α → String) :
    CanonicalOutput.ReasoningPart α → String
  | .text payload signature =>
      "{\"kind\":\"text\",\"payload\":" ++ payloadJson payload ++
        ",\"signature\":" ++ jsonOptionalString signature ++ "}"
  | .redacted payload =>
      "{\"kind\":\"redacted\",\"payload\":" ++ payloadJson payload ++ "}"
  | .encrypted payload =>
      "{\"kind\":\"encrypted\",\"payload\":" ++ payloadJson payload ++ "}"
  | .summary payload =>
      "{\"kind\":\"summary\",\"payload\":" ++ payloadJson payload ++ "}"

structure ClaudeReasoningPartWire where
  kind : String
  payload : String
  signature : Option String
  deriving Lean.ToJson

def ClaudeReasoningPartWire.ofPart : CanonicalOutput.ReasoningPart String → ClaudeReasoningPartWire
  | .text payload signature => { kind := "text", payload, signature }
  | .redacted payload => { kind := "redacted", payload, signature := none }
  | .encrypted payload => { kind := "encrypted", payload, signature := none }
  | .summary payload => { kind := "summary", payload, signature := none }

structure ClaudeStreamBlockWire where
  kind : String
  value : Option String
  parts : Option (List ClaudeReasoningPartWire)
  id : Option Nat
  name : Option String
  arguments : Option String
  deriving Lean.ToJson

def ClaudeStreamBlockWire.ofBlock : PromptAssembly.ClaudeMap.StreamBlock → ClaudeStreamBlockWire
  | .text value =>
      { kind := "text", value := some value, parts := none, id := none, name := none
        arguments := none }
  | .reasoning parts =>
      { kind := "reasoning", value := none, parts := some (parts.map ClaudeReasoningPartWire.ofPart)
        id := none, name := none, arguments := none }
  | .toolUse id name arguments =>
      { kind := "toolUse", value := none, parts := none, id := some id, name := some name
        arguments := some arguments }

structure ClaudeContentStepWire where
  provisionalThinking : Option String
  provisionalSignature : Option String
  provisionalRedacted : Option String
  sealed : List ClaudeStreamBlockWire
  deriving Lean.ToJson

def ClaudeContentStepWire.ofStep (step : PromptAssembly.ClaudeMap.ContentStep) :
    ClaudeContentStepWire :=
  { provisionalThinking := step.provisionalThinking
    provisionalSignature := step.provisionalSignature
    provisionalRedacted := step.provisionalRedacted
    sealed := step.sealed.map ClaudeStreamBlockWire.ofBlock }

structure ClaudeThinkingStreamOutput where
  outcome : String
  steps : List ClaudeContentStepWire
  content : List ClaudeStreamBlockWire
  deriving Lean.ToJson

def claudeThinkingStreamOutput (surface : List String)
    (events : List PromptAssembly.ClaudeMap.StreamEvent) : ClaudeThinkingStreamOutput :=
  match PromptAssembly.ClaudeMap.runContentTrace surface.toFinset events with
  | .ok (steps, content) =>
      { outcome := "ok", steps := steps.map ClaudeContentStepWire.ofStep
        content := content.map ClaudeStreamBlockWire.ofBlock }
  | .error error =>
      let steps := match PromptAssembly.ClaudeMap.runDecodedPrefixTrace surface.toFinset events with
        | .ok (steps, _) => steps
        | .error _ => []
      { outcome := PromptAssembly.ClaudeMap.errorName error
        steps := steps.map ClaudeContentStepWire.ofStep, content := [] }

def claudeThinkingStreamModel (i : ClaudeStreamInput) : ClaudeThinkingStreamOutput :=
  match promptAssemblyClaudeThinkingStreamWitnesses.find?
      (fun w => ClaudeStreamInput.ofWitness w == i) with
  | some w => claudeThinkingStreamOutput w.1 w.2
  | none => { outcome := "unknown", steps := [], content := [] }

@[conformance_family]
def claudeThinkingStream : Family ClaudeStreamInput ClaudeThinkingStreamOutput where
  inputs := promptAssemblyClaudeThinkingStreamWitnesses.map ClaudeStreamInput.ofWitness
  model := claudeThinkingStreamModel

/-- The streams that seal only reasoning and open a thinking block with initial
text: the canonical accumulator must seal that text with the streamed deltas. -/
@[conformance_family]
def claudeThinkingSeal : Family ClaudeStreamInput (List (List ClaudeReasoningPartWire)) where
  inputs := (promptAssemblyClaudeThinkingStreamWitnesses.filter fun w =>
      match PromptAssembly.ClaudeMap.runContentTrace w.1.toFinset w.2 with
      | .ok (_, content) =>
          !content.isEmpty &&
            content.all (fun block => match block with | .reasoning _ => true | _ => false) &&
            w.2.any (fun event => match event with
              | .thinkingStart _ text => text != ""
              | _ => false)
      | .error _ => false).map ClaudeStreamInput.ofWitness
  model := fun i =>
    (claudeThinkingStreamModel i).content.map fun block => block.parts.getD []

@[conformance_vocabulary]
def responsesStorageFamilyName : PromptAssembly.ResponsesStorage.Family → String
  | .openAiCompatible => "OpenAiCompatible"
  | .xaiGrokOAuth => "XaiGrokOAuth"
  | .chatGptCodex => "ChatGptCodex"
  | .openRouter => "OpenRouter"

deriving instance Lean.ToJson for PromptAssembly.ResponsesStorage.Family,
  PromptAssembly.ResponsesStorage.Wire

structure ResponsesStorageInput where
  name : String
  family : PromptAssembly.ResponsesStorage.Family
  wire : PromptAssembly.ResponsesStorage.Wire
  endpoint : String
  deriving Lean.ToJson

structure ResponsesStorageOutput where
  store : Option Bool
  encryptedInclude : Bool
  deriving Lean.ToJson

/-- Whether the projected Responses body asks not to be stored and requests
encrypted reasoning: production `loop_config` and its provider body projection. -/
@[conformance_family]
def promptAssemblyResponsesStorage : Family ResponsesStorageInput ResponsesStorageOutput where
  inputs := PromptAssembly.ResponsesStorage.cases.map fun c =>
    { name := c.name, family := c.family, wire := c.wire, endpoint := c.endpoint }
  model := fun i =>
    let storage := PromptAssembly.ResponsesStorage.storage i.family i.wire i.endpoint
    { store := storage.store
      encryptedInclude :=
        storage.includes.contains PromptAssembly.ResponsesStorage.encryptedReasoningInclude }

structure ResponsesEffortInput where
  name : String
  endpoint : String
  advertised : Option (List String)
  requested : Option String
  deriving Lean.ToJson

/-- The reasoning effort the projected Responses body sends: production
`loop_config` and its provider body projection. -/
@[conformance_family]
def promptAssemblyResponsesEffort : Family ResponsesEffortInput (Option String) where
  inputs := PromptAssembly.ResponsesStorage.effortCases.map fun c =>
    { name := c.name, endpoint := c.endpoint, advertised := c.advertised
      requested := c.requested }
  model := fun i =>
    PromptAssembly.ResponsesStorage.sentEffort i.endpoint i.advertised i.requested

def natsOfBytes (bytes : List UInt8) : List Nat := bytes.map UInt8.toNat

def bytesOfNats (values : List Nat) : List UInt8 := values.map Nat.toUInt8

structure ReplayPartWire where
  kind : String
  payload : List Nat
  signature : Option String
  deriving Lean.ToJson

def ReplayPartWire.ofModel : CanonicalOutput.ReasoningPart (List UInt8) → ReplayPartWire
  | .text payload signature => { kind := "text", payload := natsOfBytes payload, signature }
  | .redacted payload => { kind := "redacted", payload := natsOfBytes payload, signature := none }
  | .encrypted payload => { kind := "encrypted", payload := natsOfBytes payload, signature := none }
  | .summary payload => { kind := "summary", payload := natsOfBytes payload, signature := none }

def ReplayPartWire.toModel (part : ReplayPartWire) : CanonicalOutput.ReasoningPart (List UInt8) :=
  match part.kind with
  | "text" => .text (bytesOfNats part.payload) part.signature
  | "redacted" => .redacted (bytesOfNats part.payload)
  | "encrypted" => .encrypted (bytesOfNats part.payload)
  | _ => .summary (bytesOfNats part.payload)

/-- A canonical message block as replay input. A tool result and a media block
carry no fields here: the replay cases hold no tool result, and their media is
an image of unknown data. -/
structure ReplayInputBlockWire where
  kind : String
  payload : Option (List Nat)
  id : Option String
  parts : Option (List ReplayPartWire)
  docId : Option Nat
  callId : Option String
  name : Option String
  arguments : Option (List Nat)
  signature : Option String
  additionalParams : Option String
  deriving Lean.ToJson

def ReplayInputBlockWire.bare (kind : String) : ReplayInputBlockWire :=
  { kind, payload := none, id := none, parts := none, docId := none, callId := none
    name := none, arguments := none, signature := none, additionalParams := none }

def ReplayInputBlockWire.ofModel :
    CanonicalOutput.MessageBlock (List UInt8) → ReplayInputBlockWire
  | .text payload => { ReplayInputBlockWire.bare "text" with payload := some (natsOfBytes payload) }
  | .reasoning id parts =>
      { ReplayInputBlockWire.bare "reasoning" with
        id, parts := some (parts.map ReplayPartWire.ofModel) }
  | .toolCall docId id callId name arguments signature additionalParams =>
      { ReplayInputBlockWire.bare "toolCall" with
        docId := some docId, id := some id, callId, name := some name
        arguments := some (natsOfBytes arguments), signature, additionalParams }
  | .toolResult .. => ReplayInputBlockWire.bare "unsupportedToolResult"
  | .media .. => ReplayInputBlockWire.bare "unsupportedMedia"

def ReplayInputBlockWire.toModel (block : ReplayInputBlockWire) :
    CanonicalOutput.MessageBlock (List UInt8) :=
  match block.kind with
  | "text" => .text (bytesOfNats (block.payload.getD []))
  | "reasoning" => .reasoning block.id ((block.parts.getD []).map ReplayPartWire.toModel)
  | "toolCall" =>
      .toolCall (block.docId.getD 0) (block.id.getD "") block.callId (block.name.getD "")
        (bytesOfNats (block.arguments.getD [])) block.signature block.additionalParams
  | "unsupportedMedia" => .media { kind := .image, data := .unknown }
  | _ => .toolResult (block.docId.getD 0) (block.id.getD "") block.callId []

structure ReplayBlockWire where
  kind : String
  payload : Option (List Nat)
  signature : Option String
  id : Option String
  name : Option String
  arguments : Option (List Nat)
  deriving Lean.ToJson

def ReplayBlockWire.ofModel : PromptAssembly.ClaudeMap.ReplayBlock → ReplayBlockWire
  | .text payload =>
      { kind := "text", payload := some (natsOfBytes payload), signature := none, id := none
        name := none, arguments := none }
  | .signedThinking payload signature =>
      { kind := "signedThinking", payload := some (natsOfBytes payload), signature := some signature
        id := none, name := none, arguments := none }
  | .redactedThinking payload =>
      { kind := "redactedThinking", payload := some (natsOfBytes payload), signature := none
        id := none, name := none, arguments := none }
  | .toolUse id name arguments =>
      { kind := "toolUse", payload := none, signature := none, id := some id, name := some name
        arguments := some (natsOfBytes arguments) }

structure ClaudeReplayInput where
  name : String
  blocks : List ReplayInputBlockWire
  deriving Lean.ToJson

structure ClaudeReplayOutput where
  outcome : String
  replay : List ReplayBlockWire
  deriving Lean.ToJson

def ClaudeReplayInput.ofCase (c : PromptAssemblyClaudeReplayCase) : ClaudeReplayInput :=
  { name := c.name, blocks := c.blocks.map ReplayInputBlockWire.ofModel }

def ClaudeReplayInput.replayed (i : ClaudeReplayInput) : ClaudeReplayOutput :=
  match PromptAssembly.ClaudeMap.replayBlocks (i.blocks.map ReplayInputBlockWire.toModel) with
  | .ok replay => { outcome := "ok", replay := replay.map ReplayBlockWire.ofModel }
  | .error error => { outcome := PromptAssembly.ClaudeMap.errorName error, replay := [] }

/-- One assistant turn's canonical blocks replayed as a Messages body:
production `build_messages_body`. -/
@[conformance_family]
def promptAssemblyClaudeReplay : Family ClaudeReplayInput ClaudeReplayOutput where
  inputs := promptAssemblyClaudeReplayCases.map ClaudeReplayInput.ofCase
  model := ClaudeReplayInput.replayed

/-- The owned loop streams a Claude turn, persists it and replays it in the
next send. Only the streamed signed, redacted and tool case is built: a turn the
stream accepts is one the encoder can render, so the rejected cases never reach
a second send. -/
@[conformance_family]
def promptAssemblyClaudeReplayRoundTrip : Family ClaudeReplayInput (List ReplayBlockWire) where
  inputs := (promptAssemblyClaudeReplayCases.filter
    (·.name == "streamed-signed-redacted-tool")).map ClaudeReplayInput.ofCase
  model := fun i => i.replayed.replay

def auxiliaryKindName : CanonicalOutput.AuxiliaryKind → String
  | .compaction => "compaction"
  | .compactionFallback => "compaction_fallback"
  | .title => "title"
  | .accessReview => "access_review"

def auxiliaryKindOfName : Option String → CanonicalOutput.AuxiliaryKind
  | some "compaction_fallback" => .compactionFallback
  | some "title" => .title
  | some "access_review" => .accessReview
  | _ => .compaction

structure SourceWire where
  kind : String
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  auxiliaryKind : Option String
  call : Option Nat
  key : Option Nat
  deriving Lean.ToJson

def SourceWire.bare (kind : String) : SourceWire :=
  { kind, scope := none, turn := none, attempt := none, auxiliaryKind := none, call := none
    key := none }

def SourceWire.ofModel : CanonicalOutput.Source → SourceWire
  | .provider scope turn attempt =>
      { SourceWire.bare "provider" with
        scope := some scope, turn := some turn, attempt := some attempt }
  | .auxiliary kind scope turn attempt =>
      { SourceWire.bare "auxiliary" with
        auxiliaryKind := some (auxiliaryKindName kind), scope := some scope, turn := some turn
        attempt := some attempt }
  | .tool call => { SourceWire.bare "tool" with call := some call }
  | .toolEffectArguments call => { SourceWire.bare "tool_effect_arguments" with call := some call }
  | .authored key => { SourceWire.bare "authored" with key := some key }

def SourceWire.toModel (source : SourceWire) : CanonicalOutput.Source :=
  match source.kind with
  | "provider" => .provider (source.scope.getD 0) (source.turn.getD 0) (source.attempt.getD 0)
  | "auxiliary" =>
      .auxiliary (auxiliaryKindOfName source.auxiliaryKind) (source.scope.getD 0)
        (source.turn.getD 0) (source.attempt.getD 0)
  | "tool" => .tool (source.call.getD 0)
  | "tool_effect_arguments" => .toolEffectArguments (source.call.getD 0)
  | _ => .authored (source.key.getD 0)

structure ReplayTagWire where
  request : Nat
  source : SourceWire
  deriving Lean.ToJson

def ReplayTagWire.ofModel (tag : PromptAssembly.ClaudeMap.ReplayTag) : ReplayTagWire :=
  { request := tag.request, source := .ofModel tag.source }

def ReplayTagWire.toModel (tag : ReplayTagWire) : PromptAssembly.ClaudeMap.ReplayTag :=
  { request := tag.request, source := tag.source.toModel }

structure ReplayRowWire where
  source : Option ReplayTagWire
  physicalHeader : Option String
  blockIndices : List Nat
  blocks : List ReplayInputBlockWire
  deriving Lean.ToJson

def ReplayRowWire.ofModel (row : PromptAssembly.ClaudeMap.TaggedReplayRow) : ReplayRowWire :=
  { source := row.source.map ReplayTagWire.ofModel, physicalHeader := row.physicalHeader
    blockIndices := row.blockIndices, blocks := row.blocks.map ReplayInputBlockWire.ofModel }

def ReplayRowWire.toModel (row : ReplayRowWire) : PromptAssembly.ClaudeMap.TaggedReplayRow :=
  { source := row.source.map ReplayTagWire.toModel, physicalHeader := row.physicalHeader
    blockIndices := row.blockIndices, blocks := row.blocks.map ReplayInputBlockWire.toModel }

structure ReplayShapeInput where
  name : String
  source : ReplayRowWire
  retainedIndices : List Nat
  deriving Lean.ToJson

structure ReplayShapeOutput where
  outcome : String
  shaped : Option ReplayRowWire
  deriving Lean.ToJson

/-- One tagged assistant row narrowed to the requested source block indices:
production `select_tagged_assistant_blocks`. -/
@[conformance_family]
def promptAssemblyReplayShape : Family ReplayShapeInput ReplayShapeOutput where
  inputs := promptAssemblyReplayShapeCases.map fun c =>
    { name := c.name, source := .ofModel c.source, retainedIndices := c.retainedIndices }
  model := fun i =>
    match PromptAssembly.ClaudeMap.selectReplayBlocks
        (fun index _ => decide (index ∈ i.retainedIndices)) i.source.toModel with
    | .error error => { outcome := PromptAssembly.ClaudeMap.errorName error, shaped := none }
    | .ok shaped => { outcome := "ok", shaped := some (.ofModel shaped) }

inductive ReplayFlatKind where
  | ordinary
  | reasoning
  deriving Lean.ToJson

structure ReplayFlatItemWire where
  kind : ReplayFlatKind
  bytes : List Nat
  deriving Lean.ToJson

def ReplayFlatItemWire.ofModel : PromptAssembly.ClaudeMap.ReplayFlatItem → ReplayFlatItemWire
  | .ordinary bytes => { kind := .ordinary, bytes := bytes.map UInt8.toNat }
  | .reasoning bytes => { kind := .reasoning, bytes := bytes.map UInt8.toNat }

def ReplayFlatItemWire.toModel (item : ReplayFlatItemWire) : PromptAssembly.ClaudeMap.ReplayFlatItem :=
  match item.kind with
  | .ordinary => .ordinary (item.bytes.map Nat.toUInt8)
  | .reasoning => .reasoning (item.bytes.map Nat.toUInt8)

structure ReplayPrefixInput where
  name : String
  captured : List ReplayFlatItemWire
  current : List ReplayFlatItemWire
  deriving Lean.ToJson

structure ReplayPrefixOutput where
  ordinaryEqual : Bool
  reasoningSuffix : Bool
  leadingRemoval : Bool
  deriving Lean.ToJson

/-- The three acceptance checks a captured replay prefix passes against the
current flattened request: production `ords`, `anchored` and
`drop_leading_reasoning`. -/
@[conformance_family]
def promptAssemblyReplayPrefix : Family ReplayPrefixInput ReplayPrefixOutput where
  inputs := promptAssemblyReplayPrefixCases.map fun c =>
    { name := c.name, captured := c.captured.map .ofModel, current := c.current.map .ofModel }
  model := fun i =>
    let row := replayPrefixCase i.name (i.captured.map (·.toModel)) (i.current.map (·.toModel))
    { ordinaryEqual := row.ordinaryEqual
      reasoningSuffix := row.reasoningSuffix
      leadingRemoval := row.leadingRemoval }

def replayOriginName : PromptAssembly.ClaudeMap.ReplayOrigin → String
  | .claudeSubscription => "claudeSubscription"
  | .acceptedProvider => "acceptedProvider"
  | .recoveredProvider => "recoveredProvider"
  | .foreign => "foreign"
  | .missing => "missing"
  | .ambiguous => "ambiguous"

def replayOriginOfName : String → PromptAssembly.ClaudeMap.ReplayOrigin
  | "claudeSubscription" => .claudeSubscription
  | "acceptedProvider" => .acceptedProvider
  | "recoveredProvider" => .recoveredProvider
  | "foreign" => .foreign
  | "missing" => .missing
  | _ => .ambiguous

def replayWireName : PromptAssembly.ClaudeMap.ReplayWire → String
  | .claudeMessages => "claude_messages"
  | .responses => "responses"

def replayWireOfName : String → PromptAssembly.ClaudeMap.ReplayWire
  | "claude_messages" => .claudeMessages
  | _ => .responses

structure ReasoningWitnessWire where
  blockIndex : Nat
  parts : List ReplayPartWire
  deriving Lean.ToJson

structure ReplayResolutionWire where
  tag : ReplayTagWire
  origin : String
  issuerFamily : String
  issuerEndpoint : String
  wire : String
  physicalHeader : String
  complete : Bool
  captured : Option (List ReplayFlatItemWire)
  expectedReasoning : List ReasoningWitnessWire
  deriving Lean.ToJson

def ReplayResolutionWire.ofModel (r : PromptAssemblyClaudeCheckpointResolution) :
    ReplayResolutionWire :=
  { tag := .ofModel r.tag, origin := replayOriginName r.evidence.origin
    issuerFamily := r.evidence.issuer.family, issuerEndpoint := r.evidence.issuer.endpoint
    wire := replayWireName r.evidence.wire, physicalHeader := r.evidence.physicalHeader
    complete := r.evidence.complete
    captured := r.evidence.captured.map (·.map ReplayFlatItemWire.ofModel)
    expectedReasoning := r.evidence.reasoning.map fun (blockIndex, parts) =>
      { blockIndex, parts := parts.map ReplayPartWire.ofModel } }

def ReplayResolutionWire.toModel (r : ReplayResolutionWire) :
    PromptAssemblyClaudeCheckpointResolution :=
  { tag := r.tag.toModel
    evidence :=
      { origin := replayOriginOfName r.origin
        reasoning := r.expectedReasoning.map fun witness =>
          (witness.blockIndex, witness.parts.map ReplayPartWire.toModel)
        issuer := { family := r.issuerFamily, endpoint := r.issuerEndpoint }
        wire := replayWireOfName r.wire
        physicalHeader := r.physicalHeader
        complete := r.complete
        captured := r.captured.map (·.map ReplayFlatItemWire.toModel) } }

structure ClaudeCheckpointInput where
  name : String
  required : List ReplayTagWire
  rows : List ReplayRowWire
  split : Nat
  carrierIds : List String
  resolutions : List ReplayResolutionWire
  deriving Lean.ToJson

structure ClaudeCheckpointOutput where
  outcome : String
  prefixRows : List ReplayRowWire
  prefixCarriers : List String
  retained : List ReplayRowWire
  retainedCarriers : List String
  replay : List (List ReplayBlockWire)
  deriving Lean.ToJson

/-- A reduction checkpoint over tagged assistant rows and the contiguous replay
it restores: production `prepare_replay_checkpoint` and
`restore_contiguous_replay`. Each row travels with its message id; the
checkpoint keeps rows in order, so the prefix carries the first ids and the
retained rows the ones after them. -/
@[conformance_family]
def promptAssemblyClaudeCheckpoint : Family ClaudeCheckpointInput ClaudeCheckpointOutput where
  inputs := promptAssemblyClaudeCheckpointCases.map fun c =>
    { name := c.name, required := c.required.map .ofModel, rows := c.rows.map .ofModel
      split := c.split, carrierIds := c.carrierIds
      resolutions := c.resolutions.map ReplayResolutionWire.ofModel }
  model := fun i =>
    let carriers (rows retained : List PromptAssembly.ClaudeMap.TaggedReplayRow) :=
      (i.carrierIds.take rows.length, (i.carrierIds.drop rows.length).take retained.length)
    match PromptAssembly.ClaudeMap.prepareReplayCheckpoint (i.required.map (·.toModel))
        (i.rows.map (·.toModel)) i.split with
    | .error error =>
        { outcome := PromptAssembly.ClaudeMap.errorName error, prefixRows := []
          prefixCarriers := [], retained := [], retainedCarriers := [], replay := [] }
    | .ok checkpoint =>
        let (prefixCarriers, retainedCarriers) := carriers checkpoint.prefixRows checkpoint.retained
        let (outcome, replay) := match PromptAssembly.ClaudeMap.restoreContiguousReplay checkpoint with
          | .error error => (PromptAssembly.ClaudeMap.errorName error, [])
          | .ok replay => ("ok", replay.map (·.map ReplayBlockWire.ofModel))
        { outcome, prefixRows := checkpoint.prefixRows.map .ofModel, prefixCarriers
          retained := checkpoint.retained.map .ofModel, retainedCarriers, replay }

structure ReasoningSuffixInput where
  name : String
  issuerFamily : String
  issuerEndpoint : String
  wire : String
  context : List Nat
  rows : List ReplayRowWire
  resolutions : List ReplayResolutionWire
  /-- The flattened request the stage rows assemble into, which production
  compares a captured prefix against. -/
  assembled : List ReplayFlatItemWire
  deriving Lean.ToJson

structure ReasoningSuffixOutput where
  stage : List ReplayRowWire
  replay : List ReplayRowWire
  deriving Lean.ToJson

/-- The rows a request stages for reasoning replay and the rows it replays
after restoring the historical reasoning suffix: production
`project_recovered_prefix`, `replay_stage` and `select_replay`. -/
@[conformance_family]
def promptAssemblyReasoningSuffix : Family ReasoningSuffixInput ReasoningSuffixOutput where
  inputs := promptAssemblyReasoningSuffixCases.map fun c =>
    { name := c.name, issuerFamily := c.issuer.family, issuerEndpoint := c.issuer.endpoint
      wire := replayWireName c.wire, context := natsOfBytes c.context
      rows := c.rows.map .ofModel, resolutions := c.resolutions.map ReplayResolutionWire.ofModel
      assembled := c.assembled.map ReplayFlatItemWire.ofModel }
  model := fun i =>
    let issuer : PromptAssembly.ClaudeMap.ReplayIssuer :=
      { family := i.issuerFamily, endpoint := i.issuerEndpoint }
    let wire := replayWireOfName i.wire
    let resolve := resolveFrom (i.resolutions.map ReplayResolutionWire.toModel)
    let projected := (i.rows.map ReplayRowWire.toModel).map
      (PromptAssembly.ClaudeMap.projectRecoveredPrefix resolve)
    { stage := (PromptAssembly.ClaudeMap.replayStage projected issuer wire resolve).map .ofModel
      replay := (PromptAssembly.ClaudeMap.restoreHistoricalReasoningSuffix projected issuer wire
        resolve (replayCaseAssemble wire (bytesOfNats i.context))).map .ofModel }

example : promptAssemblyReasoningSuffixCases.any
    (fun c => (c.name.splitOn "/before-turn-").length > 1) = true := by native_decide

structure ProtectedReplayCompactionInput where
  name : String
  messageCount : Nat
  rawIndex : Nat
  required : List ReplayTagWire
  rows : List ReplayRowWire
  deriving Lean.ToJson

structure ProtectedReplayCompactionOutput where
  ceilingError : String
  ceiling : Option Nat
  selectedSplit : Option Nat
  outcome : String
  deriving Lean.ToJson

/-- The reduction ceiling a pending tool round sets, the pair-safe split under
it and the composed checkpoint: production `replay_compaction_prefix_bound`,
`protected_pair_safe_split_index` and `prepare_replay_checkpoint`. -/
@[conformance_family]
def protectedReplayCompaction :
    Family ProtectedReplayCompactionInput ProtectedReplayCompactionOutput where
  inputs := Compaction.protectedReplayCompactionCases.map fun c =>
    { name := c.name, messageCount := c.messageCount, rawIndex := c.rawIndex
      required := c.required.map .ofModel, rows := c.rows.map .ofModel }
  model := fun i =>
    let msgs := Compaction.caseFixture i.messageCount
    let required := i.required.map ReplayTagWire.toModel
    let rows := i.rows.map ReplayRowWire.toModel
    match Compaction.replayReductionCeiling required rows with
    | .error error =>
        { ceilingError := PromptAssembly.ClaudeMap.errorName error, ceiling := none
          selectedSplit := none, outcome := "ceiling_refused" }
    | .ok none => { ceilingError := "", ceiling := none, selectedSplit := none, outcome := "unbounded" }
    | .ok (some ceiling) =>
        { ceilingError := "", ceiling := some ceiling
          selectedSplit := Compaction.protectedPairSafeBoundary msgs i.rawIndex ceiling
          outcome := match Compaction.prepareProtectedReplayCheckpoint msgs i.rawIndex ceiling
              required rows with
            | .ok _ => "ok"
            | .error .cannotFit => "cannot_fit"
            | .error .misalignedAssistantProjection => "misaligned_assistant_projection"
            | .error (.replay error) => PromptAssembly.ClaudeMap.errorName error }

structure ServiceTierInput where
  family : String
  requested : Option String
  deriving Lean.ToJson, BEq

def ServiceTierInput.ofCase (c : PromptAssembly.ServiceTier.Case) : ServiceTierInput :=
  { family := c.family.wireName, requested := c.requested.map (·.configName) }

def ServiceTierInput.decision (i : ServiceTierInput) : PromptAssembly.ServiceTier.Decision :=
  match PromptAssembly.ServiceTier.cases.find? (fun c => ServiceTierInput.ofCase c == i) with
  | some c => PromptAssembly.ServiceTier.decide c.family c.requested
  | none => { allowed := false, wire := none }

structure ServiceTierAllowed where
  allowed : Bool
  deriving Lean.ToJson

structure ServiceTierWire where
  wire : Option String
  deriving Lean.ToJson

@[conformance_family]
def serviceTierValidation : Family ServiceTierInput ServiceTierAllowed where
  inputs := PromptAssembly.ServiceTier.cases.map ServiceTierInput.ofCase
  model := fun i => { allowed := i.decision.allowed }

structure ServiceTierLoopInput where
  family : String
  requested : Option String
  openaiWireApi : String
  deriving Lean.ToJson

/-- Every allowed selection on each OpenAI wire its family can carry: the ChatGPT
subscription speaks only the Codex Responses transport. -/
@[conformance_family]
def serviceTierLoopConfig : Family ServiceTierLoopInput ServiceTierWire where
  inputs := (PromptAssembly.ServiceTier.cases.filter fun c =>
      (PromptAssembly.ServiceTier.decide c.family c.requested).allowed).flatMap fun c =>
    ((if c.family = .chatGptCodex then ["responses"] else ["responses", "chat_completions"]).map
      fun wire =>
        { family := c.family.wireName, requested := c.requested.map (·.configName)
          openaiWireApi := wire })
  model := fun i =>
    { wire := (ServiceTierInput.decision { family := i.family, requested := i.requested }).wire }

/-- The families whose Responses transport carries the selection to the provider. -/
@[conformance_family]
def serviceTierTransport : Family ServiceTierInput ServiceTierWire where
  inputs := (PromptAssembly.ServiceTier.cases.filter fun c =>
      PromptAssembly.ServiceTier.supports c.family).map ServiceTierInput.ofCase
  model := fun i => { wire := i.decision.wire }

deriving instance Lean.ToJson for DiscoveryGuidanceCase

/-- Whether the prompt preamble carries the tool-discovery workflow for an
enablement and the tools actually presented: production `build_preamble`. -/
@[conformance_family]
def discoveryGuidancePresented : Family DiscoveryGuidanceCase Bool where
  inputs := discoveryGuidanceCases
  model := DiscoveryGuidanceCase.expected

end Conformance.Contracts
