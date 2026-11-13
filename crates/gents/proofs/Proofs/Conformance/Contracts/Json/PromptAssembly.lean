import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Conformance.Contracts.Json.FileInventoryClosure
import Proofs.Conformance.ContractCases.PromptAssembly
import Proofs.Session.Compaction.Executable
import Proofs.Session.PromptAssembly.ClaudeWire
import Proofs.Session.PromptAssembly.ResponsesStorage

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

def retryFrontierCasesJson : String := jsonArray (
  [("before-input", true, true, true, false, false, false),
   ("ready", true, true, true, false, false, true),
   ("live-parent", false, true, true, false, false, true),
   ("foreign-requester", true, true, false, false, false, true),
   ("unsettled", true, true, true, true, false, true),
   ("background", true, true, true, true, true, true),
   ("resend", true, false, true, false, false, true),
   ("resend-other-requester", true, false, false, false, false, true)].map
    fun (name, scopedTerminal, sameSession, sameRequester, running, background, published) =>
      "{\"name\":" ++ jsonString name ++
      ",\"scoped_terminal\":" ++ toString scopedTerminal ++
      ",\"same_session\":" ++ toString sameSession ++
      ",\"same_requester\":" ++ toString sameRequester ++
      ",\"running\":" ++ toString running ++
      ",\"background\":" ++ toString background ++
      ",\"published\":" ++ toString published ++ ",\"expected\":" ++
      (match PromptAssembly.CurrentInput.admitResume scopedTerminal sameSession sameRequester
        (PromptAssembly.CurrentInput.settledForResume running background) published with
       | some result => toString result
       | none => "null") ++ "}")

def retryEntryCasesJson : String := jsonArray (
  [ (false, ([] : List Nat), (none : Option Nat)), (false, [1, 2, 3], none),
    (true, [1], none), (true, [1, 2, 3], none), (true, [1, 2, 3, 4], none),
    (true, [1, 2, 3], some 11), (false, [1], some 11) ].map fun (resume, history, context) =>
    "{\"resume\":" ++ toString resume ++
    ",\"history\":" ++ jsonArray (history.map toString) ++
    ",\"context\":" ++ (match context with | some n => toString n | none => "null") ++
    ",\"authored\":9,\"expected\":" ++
      jsonArray ((PromptAssembly.CurrentInput.entryWithContext resume history 9 10 context
        (history.getLast? == some 4)).map toString) ++
    ",\"publish\":" ++ toString (PromptAssembly.CurrentInput.publishesAuthoredInput resume) ++ "}")

def currentInputCaseJson (witness : CurrentInputCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
  ",\"current_request\":" ++ jsonString witness.currentRequest ++
  ",\"headers\":" ++ jsonArray (witness.headers.map fun row =>
    "{\"request\":" ++ jsonString row.request ++ ",\"kind\":" ++ jsonString row.kind ++ "}") ++
  ",\"retained_indices\":" ++ jsonArray (witness.retainedIndices.map toString) ++ "}"

def currentInputCasesJson : String := jsonArray (currentInputCases.map currentInputCaseJson)

def wakeInputCasesJson : String := jsonArray (wakeInputCases.map fun witness =>
  "{\"name\":" ++ jsonString witness.name ++
  ",\"claimed\":" ++ jsonArray (witness.claimed.map jsonString) ++
  ",\"acknowledged\":" ++ jsonArray (witness.acknowledged.map jsonString) ++
  ",\"rows\":" ++ jsonArray (witness.history.map fun row =>
    "{\"header\":" ++ jsonString row.header ++
    ",\"notification_key\":" ++ jsonOptionalString row.notificationKey ++ "}") ++
  ",\"notifications\":" ++ jsonArray (witness.notifications.map fun row =>
    "{\"header\":" ++ jsonString row.header ++
    ",\"notification_key\":" ++ jsonOptionalString row.notificationKey ++ "}") ++
  ",\"current_keys\":" ++ jsonArray (witness.currentKeys.map jsonString) ++
  ",\"expected_order\":" ++ jsonArray (witness.expectedOrder.map jsonString) ++ "}")

def jsonNatArray (values : List Nat) : String :=
  jsonArray (values.map toString)

def promptAssemblyItemJson (item : PromptAssemblyItemCase) : String :=
  "{"
    ++ "\"item\":" ++ jsonString item.item ++ ","
    ++ "\"value\":" ++ toString item.value
    ++ "}"

def promptAssemblyAssistantOrderCaseJson
    (witness : PromptAssemblyAssistantOrderCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"order_mode\":" ++ jsonString witness.orderMode ++
    ",\"input\":" ++ jsonArray (witness.input.map promptAssemblyItemJson) ++
    ",\"expected\":" ++ jsonArray (witness.expected.map promptAssemblyItemJson) ++
    ",\"expected_twice\":" ++
      jsonArray (witness.expectedTwice.map promptAssemblyItemJson) ++ "}"

def promptAssemblyAssistantOrderCasesJson : String :=
  jsonArray (promptAssemblyAssistantOrderCases.map promptAssemblyAssistantOrderCaseJson)

def promptAssemblyRowJson (row : PromptAssemblyRowCase) : String :=
  "{"
    ++ "\"role\":" ++ jsonString row.role ++ ","
    ++ "\"kind\":" ++ jsonString row.kind ++ ","
    ++ "\"call_ids\":" ++ jsonNatArray row.callIds ++ ","
    ++ "\"content\":" ++ jsonArray (row.content.map promptAssemblyItemJson)
    ++ "}"

def promptAssemblyRowsJson (rows : List PromptAssemblyRowCase) : String :=
  jsonArray (rows.map promptAssemblyRowJson)

def promptAssemblySplitJson (split : PromptAssemblySplitCase) : String :=
  "{"
    ++ "\"index\":" ++ toString split.index ++ ","
    ++ "\"expected\":" ++ promptAssemblyRowsJson split.expected
    ++ "}"

def promptAssemblySanitizeCaseJson (witness : PromptAssemblySanitizeCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"input\":" ++ promptAssemblyRowsJson witness.input ++ ","
    ++ "\"expected\":" ++ promptAssemblyRowsJson witness.expected ++ ","
    ++ "\"expected_twice\":" ++ promptAssemblyRowsJson witness.expectedTwice ++ ","
    ++ "\"splits\":" ++ jsonArray (witness.splits.map promptAssemblySplitJson)
    ++ "}"

def promptAssemblySanitizeCasesJson : String :=
  jsonArray (promptAssemblySanitizeCases.map promptAssemblySanitizeCaseJson)

def promptAssemblyModeSanitizeCaseJson
    (witness : PromptAssemblyModeSanitizeCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"order_mode\":" ++ jsonString witness.orderMode ++
    ",\"input\":" ++ promptAssemblyRowsJson witness.input ++
    ",\"expected\":" ++ promptAssemblyRowsJson witness.expected ++
    ",\"expected_twice\":" ++ promptAssemblyRowsJson witness.expectedTwice ++
    ",\"association\":" ++ promptAssemblyRowsJson witness.association ++
    ",\"association_indices\":" ++
      jsonArray (witness.associationIndices.map fun indices =>
        jsonArray (indices.map toString)) ++ "}"

def promptAssemblyModeSanitizeCasesJson : String :=
  jsonArray (promptAssemblyModeSanitizeCases.map promptAssemblyModeSanitizeCaseJson)

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

def promptAssemblyRepairCaseJson (witness : PromptAssemblyRepairCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"input\":" ++ jsonString witness.input ++ ","
    ++ "\"expected\":" ++ jsonString witness.expected ++ ","
    ++ "\"expected_twice\":" ++ jsonString witness.expectedTwice ++ ","
    ++ "\"payload_only\":" ++ boolString witness.payloadOnly
    ++ "}"

def promptAssemblyRepairCasesJson : String :=
  jsonArray (promptAssemblyRepairCases.map promptAssemblyRepairCaseJson)

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

private def natOptionJson : Option Nat → String
  | some value => toString value
  | none => "null"

def promptAssemblyCompactionTriggerCaseJson
    (witness : PromptAssemblyCompactionTriggerCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"context_window\":" ++ toString witness.contextWindow ++ ","
    ++ "\"max_output_tokens\":" ++ toString witness.maxOutputTokens ++ ","
    ++ "\"threshold_basis_points\":" ++ toString witness.thresholdBasisPoints ++ ","
    ++ "\"prior_basis_points\":" ++ toString witness.priorBasisPoints ++ ","
    ++ "\"seeded_observations\":"
      ++ jsonArray (witness.seededObservations.map toString) ++ ","
    ++ "\"observed\":" ++ jsonArray (witness.observed.map fun (provider, estimate) =>
      "[" ++ toString provider ++ "," ++ toString estimate ++ "]") ++ ","
    ++ "\"anchor\":" ++ (match witness.anchor with
      | some anchor => "{\"input_tokens\":" ++ toString anchor.input
          ++ ",\"output_tokens\":" ++ toString anchor.output
          ++ ",\"estimated_input_tokens\":" ++ toString anchor.estimated ++ "}"
      | none => "null") ++ ","
    ++ "\"invalidate\":" ++ boolString witness.invalidate ++ ","
    ++ "\"raw_input_tokens\":" ++ toString witness.rawInputTokens ++ ","
    ++ "\"ratio_basis_points\":" ++ toString witness.ratioBasisPoints ++ ","
    ++ "\"calibrated_tokens\":" ++ toString witness.calibratedTokens ++ ","
    ++ "\"anchored_tokens\":" ++ natOptionJson witness.anchoredTokens ++ ","
    ++ "\"context_tokens\":" ++ toString witness.contextTokens ++ ","
    ++ "\"compaction_input_budget\":" ++ toString witness.compactionInputBudget ++ ","
    ++ "\"admission\":" ++ (match witness.admission with
      | some admission => "{\"input_tokens\":" ++ toString admission.inputTokens
          ++ ",\"input_budget\":" ++ toString admission.inputBudget ++ "}"
      | none => "null")
    ++ "}"

def promptAssemblyCompactionTriggerCasesJson : String :=
  jsonArray (promptAssemblyCompactionTriggerCases.map promptAssemblyCompactionTriggerCaseJson)

def promptAssemblyRetentionCaseJson
    (witness : PromptAssemblyRetentionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"configured_keep_recent\":" ++ toString witness.configuredKeepRecent ++ ","
    ++ "\"effective_input_budget\":" ++ toString witness.effectiveInputBudget ++ ","
    ++ "\"fixed_input\":" ++ toString witness.fixedInput ++ ","
    ++ "\"available_input\":" ++ toString witness.availableInput ++ ","
    ++ "\"retention_target\":" ++ toString witness.retentionTarget ++ ","
    ++ "\"summary_max_output\":" ++ toString witness.summaryMaxOutput ++ ","
    ++ "\"effective_summary_output\":"
      ++ toString witness.effectiveSummaryOutput ++ ","
    ++ "\"rolling_summary_input_budget\":"
      ++ toString witness.rollingSummaryInputBudget
    ++ "}"

def promptAssemblyRetentionCasesJson : String :=
  jsonArray (promptAssemblyRetentionCases.map promptAssemblyRetentionCaseJson)

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

private def claudeBytesJson (bytes : List UInt8) : String :=
  jsonArray (bytes.map fun byte => toString byte.toNat)

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

private def responsesStorageFamilyJson :
    PromptAssembly.ResponsesStorage.Family → String
  | .openAiCompatible => "OpenAiCompatible"
  | .xaiGrokOAuth => "XaiGrokOAuth"
  | .chatGptCodex => "ChatGptCodex"
  | .openRouter => "OpenRouter"

private def responsesStorageWireJson :
    PromptAssembly.ResponsesStorage.Wire → String
  | .responses => "responses"
  | .chatCompletions => "chat_completions"

def promptAssemblyResponsesStorageCaseJson
    (witness : PromptAssembly.ResponsesStorage.Case) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"family\":" ++ jsonString (responsesStorageFamilyJson witness.family) ++
    ",\"wire\":" ++ jsonString (responsesStorageWireJson witness.wire) ++
    ",\"endpoint\":" ++ jsonString witness.endpoint ++
    ",\"store\":" ++ (match witness.expected.store with
      | none => "null"
      | some value => boolString value) ++
    ",\"encrypted_include\":" ++ boolString
      (witness.expected.includes.contains
        PromptAssembly.ResponsesStorage.encryptedReasoningInclude) ++ "}"

def promptAssemblyResponsesStorageCasesJson : String :=
  jsonArray (PromptAssembly.ResponsesStorage.cases.map
    promptAssemblyResponsesStorageCaseJson)

def promptAssemblyResponsesEffortCaseJson
    (witness : PromptAssembly.ResponsesStorage.EffortCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"endpoint\":" ++ jsonString witness.endpoint ++
    ",\"advertised\":" ++ jsonOptionalStringArray witness.advertised ++
    ",\"requested\":" ++ jsonOptionalString witness.requested ++
    ",\"expected\":" ++ jsonOptionalString witness.expected ++ "}"

def promptAssemblyResponsesEffortCasesJson : String :=
  jsonArray (PromptAssembly.ResponsesStorage.effortCases.map
    promptAssemblyResponsesEffortCaseJson)

private def claudeReplayInputBlockJson :
    CanonicalOutput.MessageBlock (List UInt8) → String
  | .text payload =>
      "{\"kind\":\"text\",\"payload\":" ++ claudeBytesJson payload ++ "}"
  | .reasoning id parts =>
      "{\"kind\":\"reasoning\",\"id\":" ++ jsonOptionalString id ++
      ",\"parts\":" ++
      jsonArray (parts.map (claudeReasoningPartJson claudeBytesJson)) ++ "}"
  | .toolCall docId id callId name arguments signature additionalParams =>
      "{\"kind\":\"toolCall\",\"doc_id\":" ++ toString docId ++
      ",\"id\":" ++ jsonString id ++
      ",\"call_id\":" ++ jsonOptionalString callId ++
      ",\"name\":" ++ jsonString name ++
      ",\"arguments\":" ++ claudeBytesJson arguments ++
      ",\"signature\":" ++ jsonOptionalString signature ++
      ",\"additional_params\":" ++ jsonOptionalString additionalParams ++ "}"
  | .toolResult .. => "{\"kind\":\"unsupportedToolResult\"}"
  | .media .. => "{\"kind\":\"unsupportedMedia\"}"

private def claudeReplayBlockJson : PromptAssembly.ClaudeMap.ReplayBlock → String
  | .text payload =>
      "{\"kind\":\"text\",\"payload\":" ++ claudeBytesJson payload ++ "}"
  | .signedThinking payload signature =>
      "{\"kind\":\"signedThinking\",\"payload\":" ++ claudeBytesJson payload ++
      ",\"signature\":" ++ jsonString signature ++ "}"
  | .redactedThinking payload =>
      "{\"kind\":\"redactedThinking\",\"payload\":" ++ claudeBytesJson payload ++ "}"
  | .toolUse id name arguments =>
      "{\"kind\":\"toolUse\",\"id\":" ++ jsonString id ++
      ",\"name\":" ++ jsonString name ++
      ",\"arguments\":" ++ claudeBytesJson arguments ++ "}"

def promptAssemblyClaudeReplayCaseJson (witness : PromptAssemblyClaudeReplayCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"blocks\":" ++ jsonArray (witness.blocks.map claudeReplayInputBlockJson) ++
    ",\"outcome\":" ++ jsonString witness.outcome ++
    ",\"replay\":" ++ jsonArray (witness.replay.map claudeReplayBlockJson) ++ "}"

def promptAssemblyClaudeReplayCasesJson : String :=
  jsonArray (promptAssemblyClaudeReplayCases.map promptAssemblyClaudeReplayCaseJson)

private def claudeReplayOriginJson : PromptAssembly.ClaudeMap.ReplayOrigin → String
  | .claudeSubscription => jsonString "claudeSubscription"
  | .acceptedProvider => jsonString "acceptedProvider"
  | .recoveredProvider => jsonString "recoveredProvider"
  | .foreign => jsonString "foreign"
  | .missing => jsonString "missing"
  | .ambiguous => jsonString "ambiguous"

private def claudeReasoningWitnessJson
    (witness : PromptAssembly.ClaudeMap.ReasoningWitness) : String :=
  jsonArray (witness.map fun (index, parts) =>
    "{\"block_index\":" ++ toString index ++ ",\"parts\":" ++
      jsonArray (parts.map (claudeReasoningPartJson claudeBytesJson)) ++ "}")

def claudeReplayTagJson (tag : PromptAssembly.ClaudeMap.ReplayTag) : String :=
  "{\"request\":" ++ toString tag.request ++
    ",\"source\":" ++ sourceJson tag.source ++ "}"

def claudeTaggedReplayRowJson
    (row : PromptAssembly.ClaudeMap.TaggedReplayRow) : String :=
  "{\"source\":" ++
    (match row.source with
     | none => "null"
     | some tag => claudeReplayTagJson tag) ++
    ",\"physical_header\":" ++
    (match row.physicalHeader with
     | none => "null"
     | some header => jsonString header) ++
    ",\"block_indices\":" ++ jsonArray (row.blockIndices.map toString) ++
    ",\"blocks\":" ++ jsonArray (row.blocks.map claudeReplayInputBlockJson) ++ "}"

private def replayFlatItemsJson (items : List PromptAssembly.ClaudeMap.ReplayFlatItem) : String :=
  jsonArray (items.map fun item =>
    match item with
    | .ordinary bytes => "{\"kind\":\"ordinary\",\"bytes\":" ++ claudeBytesJson bytes ++ "}"
    | .reasoning bytes => "{\"kind\":\"reasoning\",\"bytes\":" ++ claudeBytesJson bytes ++ "}")

private def claudeCheckpointResolutionJson
    (resolution : PromptAssemblyClaudeCheckpointResolution) : String :=
  "{\"tag\":" ++ claudeReplayTagJson resolution.tag ++
    ",\"origin\":" ++ claudeReplayOriginJson resolution.evidence.origin ++
    ",\"issuer_family\":" ++ jsonString resolution.evidence.issuer.family ++
    ",\"issuer_endpoint\":" ++ jsonString resolution.evidence.issuer.endpoint ++
    ",\"wire\":" ++
      (match resolution.evidence.wire with
       | .claudeMessages => jsonString "claude_messages"
       | .responses => jsonString "responses") ++
    ",\"physical_header\":" ++ jsonString resolution.evidence.physicalHeader ++
    ",\"complete\":" ++ (if resolution.evidence.complete then "true" else "false") ++
    ",\"captured\":" ++
      (match resolution.evidence.captured with
       | none => "null"
       | some items => replayFlatItemsJson items) ++
    ",\"expected_reasoning\":" ++
      claudeReasoningWitnessJson resolution.evidence.reasoning ++ "}"

def promptAssemblyClaudeCheckpointCaseJson
    (witness : PromptAssemblyClaudeCheckpointCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"required\":" ++ jsonArray (witness.required.map claudeReplayTagJson) ++
    ",\"rows\":" ++ jsonArray (witness.rows.map claudeTaggedReplayRowJson) ++
    ",\"split\":" ++ toString witness.split ++
    ",\"carrier_ids\":" ++ jsonStringArray witness.carrierIds ++
    ",\"resolutions\":" ++
      jsonArray (witness.resolutions.map claudeCheckpointResolutionJson) ++
    ",\"outcome\":" ++ jsonString witness.outcome ++
    ",\"prefix_rows\":" ++
      jsonArray (witness.prefixRows.map claudeTaggedReplayRowJson) ++
    ",\"retained\":" ++
      jsonArray (witness.retained.map claudeTaggedReplayRowJson) ++
    ",\"replay\":" ++ jsonArray (witness.replay.map fun row =>
      jsonArray (row.map claudeReplayBlockJson)) ++ "}"

def promptAssemblyClaudeCheckpointCasesJson : String :=
  jsonArray (promptAssemblyClaudeCheckpointCases.map promptAssemblyClaudeCheckpointCaseJson)



def promptAssemblyReasoningSuffixCaseJson
    (witness : PromptAssemblyReasoningSuffixCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"issuer_family\":" ++ jsonString witness.issuer.family ++
    ",\"issuer_endpoint\":" ++ jsonString witness.issuer.endpoint ++
    ",\"wire\":" ++
      (match witness.wire with
       | .claudeMessages => jsonString "claude_messages"
       | .responses => jsonString "responses") ++
    ",\"rows\":" ++ jsonArray (witness.rows.map claudeTaggedReplayRowJson) ++
    ",\"resolutions\":" ++
      jsonArray (witness.resolutions.map claudeCheckpointResolutionJson) ++
    ",\"stage\":" ++ jsonArray (witness.stage.map claudeTaggedReplayRowJson) ++
    ",\"assembled\":" ++ replayFlatItemsJson witness.assembled ++
    ",\"replay\":" ++ jsonArray (witness.replay.map claudeTaggedReplayRowJson) ++ "}"

def promptAssemblyReasoningSuffixCasesJson : String :=
  jsonArray (promptAssemblyReasoningSuffixCases.map promptAssemblyReasoningSuffixCaseJson)

def promptAssemblyReplayShapeCaseJson (witness : PromptAssemblyReplayShapeCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"source\":" ++ claudeTaggedReplayRowJson witness.source ++
    ",\"retained_indices\":" ++ jsonArray (witness.retainedIndices.map toString) ++
    ",\"outcome\":" ++ jsonString witness.outcome ++
    ",\"shaped\":" ++
      (match witness.shaped with
       | none => "null"
       | some row => claudeTaggedReplayRowJson row) ++ "}"

def promptAssemblyReplayShapeCasesJson : String :=
  jsonArray (promptAssemblyReplayShapeCases.map promptAssemblyReplayShapeCaseJson)

def promptAssemblyReplayPrefixCaseJson
    (witness : PromptAssemblyReplayPrefixCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"captured\":" ++ replayFlatItemsJson witness.captured ++
    ",\"current\":" ++ replayFlatItemsJson witness.current ++
    ",\"ordinary_equal\":" ++ (if witness.ordinaryEqual then "true" else "false") ++
    ",\"reasoning_suffix\":" ++ (if witness.reasoningSuffix then "true" else "false") ++
    ",\"leading_removal\":" ++ (if witness.leadingRemoval then "true" else "false") ++ "}"

def promptAssemblyReplayPrefixCasesJson : String :=
  jsonArray (promptAssemblyReplayPrefixCases.map promptAssemblyReplayPrefixCaseJson)

def protectedReplayCompactionCaseJson
    (witness : Compaction.ProtectedReplayCompactionCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"message_count\":" ++ toString witness.messageCount ++
    ",\"raw_index\":" ++ toString witness.rawIndex ++
    ",\"required\":" ++ jsonArray (witness.required.map claudeReplayTagJson) ++
    ",\"rows\":" ++ jsonArray (witness.rows.map claudeTaggedReplayRowJson) ++
    ",\"ceiling_error\":" ++ jsonString witness.ceilingError ++
    ",\"ceiling\":" ++ jsonOptionalNat witness.ceiling ++
    ",\"selected_split\":" ++ jsonOptionalNat witness.selectedSplit ++
    ",\"outcome\":" ++ jsonString witness.outcome ++ "}"

def protectedReplayCompactionCasesJson : String :=
  jsonArray (Compaction.protectedReplayCompactionCases.map protectedReplayCompactionCaseJson)

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

def providerToolIdentityJson (identity : PromptAssembly.ToolCallIdentity.Identity) : String :=
  "{\"id\":" ++ jsonString identity.nativeId ++
    ",\"call_id\":" ++ jsonOptionalString identity.callId ++
    ",\"openai_item_id\":" ++ jsonOptionalString
      (PromptAssembly.ToolCallIdentity.openAiItemId identity.nativeId) ++ "}"

def providerToolIdentityCasesJson : String :=
  jsonArray (providerToolIdentityCases.map fun row =>
    "{\"name\":" ++ jsonString row.name ++
    ",\"responses\":" ++ toString row.responses ++
    ",\"input\":" ++ jsonArray (row.input.map providerToolIdentityJson) ++
    ",\"expected\":" ++ jsonArray (row.expected.map providerToolIdentityJson) ++ "}")

def discoveryGuidanceCasesJson : String :=
  jsonArray (discoveryGuidanceCases.map fun row =>
    "{\"name\":" ++ jsonString row.name ++
    ",\"enabled\":" ++ toString row.enabled ++
    ",\"presented\":" ++ jsonArray (row.presented.map jsonString) ++
    ",\"expected\":" ++ toString row.expected ++ "}")

end Conformance.Contracts
