import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.FileInventoryClosure
import Proofs.Conformance.ContractCases.PromptAssembly
import Proofs.Session.Compaction.Executable
import Proofs.Session.PromptAssembly.ClaudeWire
import Proofs.Session.PromptAssembly.ResponsesStorage
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def routingAffinityCasesJson : String := jsonArray (routingAffinityCases.map fun row =>
  "{\"name\":" ++ jsonString row.name ++ ",\"codex\":" ++ toString row.codex ++
  ",\"main\":" ++ toString row.main ++ ",\"session\":" ++ jsonOptionalString row.session ++
  ",\"responses\":" ++ jsonArray (row.responses.map (jsonOptionalString)) ++
  ",\"expected_session\":" ++ jsonOptionalString row.expected.session ++
  ",\"expected_token\":" ++ jsonOptionalString row.expected.token ++ "}")

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

def currentInputCaseJson (witness : CurrentInputCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
  ",\"current_request\":" ++ jsonString witness.currentRequest ++
  ",\"headers\":" ++ jsonArray (witness.headers.map fun row =>
    "{\"request\":" ++ jsonString row.request ++ ",\"kind\":" ++ jsonString row.kind ++ "}") ++
  ",\"retained_indices\":" ++ jsonArray (witness.retainedIndices.map toString) ++ "}"

def currentInputCasesJson : String := jsonArray (currentInputCases.map currentInputCaseJson)

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

def promptAssemblyLayerCaseJson (witness : PromptAssemblyLayerCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"skill_count\":" ++ toString witness.skillCount ++ ","
    ++ "\"summary_count\":" ++ toString witness.summaryCount ++ ","
    ++ "\"conversation_len\":" ++ toString witness.conversationLen ++ ","
    ++ "\"slots\":" ++ jsonStringArray witness.slots
    ++ "}"

def promptAssemblyLayerCasesJson : String :=
  jsonArray (promptAssemblyLayerCases.map promptAssemblyLayerCaseJson)

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

def promptAssemblyBudgetCaseJson (witness : PromptAssemblyBudgetCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"context_window\":" ++ toString witness.contextWindow ++ ","
    ++ "\"max_output_tokens\":" ++ toString witness.maxOutputTokens ++ ","
    ++ "\"threshold_basis_points\":" ++ toString witness.thresholdBasisPoints ++ ","
    ++ "\"configured_threshold_budget\":"
      ++ toString witness.configuredThresholdBudget ++ ","
    ++ "\"prompt_tokens\":" ++ toString witness.promptTokens ++ ","
    ++ "\"request_tokens\":" ++ toString witness.requestTokens ++ ","
    ++ "\"effective_input_budget\":" ++ toString witness.effectiveInputBudget ++ ","
    ++ "\"compaction_input_budget\":" ++ toString witness.compactionInputBudget ++ ","
    ++ "\"effective_output_tokens\":" ++ toString witness.effectiveOutputTokens ++ ","
    ++ "\"should_compact\":" ++ boolString witness.shouldCompact ++ ","
    ++ "\"provider_safe\":" ++ boolString witness.providerSafe ++ ","
    ++ "\"can_dispatch\":" ++ boolString witness.canDispatch
    ++ "}"

def promptAssemblyBudgetCasesJson : String :=
  jsonArray (promptAssemblyBudgetCases.map promptAssemblyBudgetCaseJson)

def promptAssemblyTurnBudgetCaseJson
    (witness : PromptAssemblyTurnBudgetCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"context_window\":" ++ toString witness.contextWindow ++ ","
    ++ "\"max_output_tokens\":" ++ toString witness.maxOutputTokens ++ ","
    ++ "\"threshold_basis_points\":" ++ toString witness.thresholdBasisPoints ++ ","
    ++ "\"configured_threshold_budget\":"
      ++ toString witness.configuredThresholdBudget ++ ","
    ++ "\"effective_input_budget\":" ++ toString witness.effectiveInputBudget ++ ","
    ++ "\"compaction_input_budget\":" ++ toString witness.compactionInputBudget ++ ","
    ++ "\"turn_input_tokens\":"
      ++ jsonArray (witness.turnInputTokens.map toString) ++ ","
    ++ "\"turn_output_tokens\":"
      ++ jsonArray (witness.turnOutputTokens.map toString) ++ ","
    ++ "\"turn_should_compact\":"
      ++ jsonArray (witness.turnShouldCompact.map boolString) ++ ","
    ++ "\"turn_can_dispatch\":"
      ++ jsonArray (witness.turnCanDispatch.map boolString)
    ++ "}"

def promptAssemblyTurnBudgetCasesJson : String :=
  jsonArray (promptAssemblyTurnBudgetCases.map promptAssemblyTurnBudgetCaseJson)

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

def promptAssemblyClaudeMapCaseJson
    (witness : PromptAssemblyClaudeMapCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"surface\":" ++ jsonStringArray witness.surface ++ ","
    ++ "\"blocks\":" ++ jsonStringArray witness.blocks ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"ids\":" ++ jsonNatArray witness.ids
    ++ "}"

def promptAssemblyClaudeMapCasesJson : String :=
  jsonArray (promptAssemblyClaudeMapCases.map promptAssemblyClaudeMapCaseJson)

def claudeFieldFactsJson (f : PromptAssembly.ClaudeBody.ModelFacts) : String :=
  "{"
    ++ "\"adaptive\":" ++ boolString f.adaptive
    ++ ",\"always_thinks\":" ++ boolString f.alwaysThinks
    ++ ",\"thinking_supported\":" ++ boolString f.thinkingSupported
    ++ ",\"rejects_disabled\":" ++ boolString f.rejectsDisabled
    ++ ",\"accepts_temperature\":" ++ boolString f.acceptsTemperature
    ++ ",\"effort_capable\":" ++ boolString f.effortCapable
    ++ ",\"max_effort\":" ++ boolString f.maxEffort
    ++ ",\"xhigh_effort\":" ++ boolString f.xhighEffort
    ++ ",\"caps_effort_when_disabled\":" ++ boolString f.capsEffortWhenDisabled
    ++ ",\"per_turn_effort\":" ++ boolString f.perTurnEffort
    ++ ",\"output_default\":" ++ toString f.outputDefault
    ++ ",\"output_upper\":" ++ toString f.outputUpper
    ++ ",\"default_effort\":" ++ jsonOptionalString f.defaultEffort
    ++ "}"

def claudeFieldThinkingConfigJson : PromptAssembly.ClaudeBody.ThinkingConfig → String
  | .adaptive => "{\"kind\":\"adaptive\",\"mechanical\":false,\"budget\":null}"
  | .disabled mechanical =>
      "{\"kind\":\"disabled\",\"mechanical\":" ++ boolString mechanical ++ ",\"budget\":null}"
  | .enabled budget =>
      "{\"kind\":\"enabled\",\"mechanical\":false,\"budget\":" ++ jsonOptionalNat budget ++ "}"

def claudeFieldRequestJson (r : PromptAssembly.ClaudeBody.Request) : String :=
  "{"
    ++ "\"thinking\":" ++ claudeFieldThinkingConfigJson r.thinking
    ++ ",\"max_tokens_override\":" ++ jsonOptionalNat r.maxTokensOverride
    ++ ",\"requested_effort\":" ++ jsonOptionalString r.requestedEffort
    ++ ",\"first_party\":" ++ boolString r.firstParty
    ++ ",\"first_party_default_url\":" ++ boolString r.firstPartyDefaultUrl
    ++ ",\"experimental_betas_disabled\":" ++ boolString r.experimentalBetasDisabled
    ++ ",\"agentic\":" ++ boolString r.agentic
    ++ ",\"fork\":" ++ boolString r.fork
    ++ ",\"display_updates\":" ++ boolString r.displayUpdates
    ++ ",\"context_management_beta\":" ++ boolString r.contextManagementBeta
    ++ ",\"per_turn_control_beta\":" ++ boolString r.perTurnControlBeta
    ++ ",\"previous_message_id\":" ++ jsonOptionalString r.previousMessageId
    ++ "}"

def claudeFieldThinkingJson : Option PromptAssembly.ClaudeBody.Thinking → String
  | none => "{\"kind\":\"none\",\"budget\":null,\"updates\":false}"
  | some .disabled => "{\"kind\":\"disabled\",\"budget\":null,\"updates\":false}"
  | some (.adaptive updates) =>
      "{\"kind\":\"adaptive\",\"budget\":null,\"updates\":" ++ boolString updates ++ "}"
  | some (.enabled budget updates) =>
      "{\"kind\":\"enabled\",\"budget\":" ++ toString budget ++ ",\"updates\":"
        ++ boolString updates ++ "}"

def claudeFieldDiagnosticsJson : Option (Option String) → String
  | none => "{\"present\":false,\"previous_message_id\":null}"
  | some previous => "{\"present\":true,\"previous_message_id\":" ++ jsonOptionalString previous ++ "}"

def promptAssemblyClaudeBodyCaseJson (witness : PromptAssemblyClaudeBodyCase) : String :=
  let d := witness.decision
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"model\":" ++ jsonString witness.model ++ ","
    ++ "\"first_user_text\":" ++ jsonString witness.firstUserText ++ ","
    ++ "\"fingerprint_placeholder\":" ++ jsonString claudeFingerprintPlaceholder ++ ","
    ++ "\"prompt_id\":" ++ jsonString witness.promptId ++ ","
    ++ "\"attribution\":" ++ jsonString witness.attribution ++ ","
    ++ "\"preamble\":" ++ jsonOptionalString witness.preamble ++ ","
    ++ "\"rows\":" ++ jsonStringArray witness.rows ++ ","
    ++ "\"tools\":" ++ jsonStringArray witness.tools ++ ","
    ++ "\"system\":" ++ jsonStringArray witness.system ++ ","
    ++ "\"system_marked\":" ++ jsonArray (witness.systemMarked.map boolString) ++ ","
    ++ "\"tools_present\":" ++ boolString witness.toolsPresent
    ++ ",\"supported_efforts\":" ++ jsonOptionalStringArray witness.supportedEfforts
    ++ ",\"effort\":" ++ jsonOptionalString witness.effort
    ++ ",\"selected_effort\":" ++ jsonOptionalString witness.selectedEffort
    ++ ",\"facts\":" ++ claudeFieldFactsJson witness.facts
    ++ ",\"request\":" ++ claudeFieldRequestJson witness.request
    ++ ",\"max_tokens\":" ++ toString d.maxTokens
    ++ ",\"thinking\":" ++ claudeFieldThinkingJson d.thinking
    ++ ",\"temperature\":" ++ jsonOptionalNat d.temperature
    ++ ",\"context_management\":" ++ boolString d.contextManagement
    ++ ",\"body_effort\":" ++ jsonOptionalString d.effort
    ++ ",\"diagnostics\":" ++ claudeFieldDiagnosticsJson d.diagnostics
    ++ ",\"per_turn_effort\":" ++ jsonOptionalString d.perTurnEffort
    ++ "}"

def promptAssemblyClaudeBodyCasesJson : String :=
  jsonArray (promptAssemblyClaudeBodyCases.map promptAssemblyClaudeBodyCaseJson)

def promptAssemblyClaudeStreamCaseJson (witness : PromptAssemblyClaudeStreamCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"surface\":" ++ jsonStringArray witness.surface ++ ","
    ++ "\"events\":" ++ jsonStringArray witness.events ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"calls\":" ++ jsonStringArray witness.calls
    ++ "}"

def promptAssemblyClaudeStreamCasesJson : String :=
  jsonArray (promptAssemblyClaudeStreamCases.map promptAssemblyClaudeStreamCaseJson)

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

private def claudeStreamEventJson : PromptAssembly.ClaudeMap.StreamEvent → String
  | .text value => "{\"kind\":\"text\",\"value\":" ++ jsonString value ++ "}"
  | .start id name input =>
      "{\"kind\":\"toolStart\",\"id\":" ++ toString id ++
      ",\"name\":" ++ jsonString name ++ ",\"input\":" ++ jsonOptionalString input ++ "}"
  | .delta fragment =>
      "{\"kind\":\"toolDelta\",\"fragment\":" ++ jsonString fragment ++ "}"
  | .stop => "{\"kind\":\"toolStop\"}"
  | .thinkingStart index initialText =>
      "{\"kind\":\"thinkingStart\",\"index\":" ++ toString index ++
      ",\"value\":" ++ jsonString initialText ++ "}"
  | .thinkingDelta index fragment =>
      "{\"kind\":\"thinkingDelta\",\"index\":" ++ toString index ++
      ",\"fragment\":" ++ jsonString fragment ++ "}"
  | .signatureDelta index fragment =>
      "{\"kind\":\"signatureDelta\",\"index\":" ++ toString index ++
      ",\"fragment\":" ++ jsonString fragment ++ "}"
  | .redactedStart index data =>
      "{\"kind\":\"redactedStart\",\"index\":" ++ toString index ++
      ",\"data\":" ++ jsonString data ++ "}"
  | .contentStop index =>
      "{\"kind\":\"contentStop\",\"index\":" ++ toString index ++ "}"

private def claudeStreamBlockJson : PromptAssembly.ClaudeMap.StreamBlock → String
  | .text value => "{\"kind\":\"text\",\"value\":" ++ jsonString value ++ "}"
  | .reasoning parts =>
      "{\"kind\":\"reasoning\",\"parts\":" ++
        jsonArray (parts.map (claudeReasoningPartJson jsonString)) ++ "}"
  | .toolUse id name arguments =>
      "{\"kind\":\"toolUse\",\"id\":" ++ toString id ++
      ",\"name\":" ++ jsonString name ++
      ",\"arguments\":" ++ jsonString arguments ++ "}"

private def claudeContentStepJson (step : PromptAssembly.ClaudeMap.ContentStep) : String :=
  "{\"provisional_thinking\":" ++ jsonOptionalString step.provisionalThinking ++
    ",\"provisional_signature\":" ++ jsonOptionalString step.provisionalSignature ++
    ",\"provisional_redacted\":" ++ jsonOptionalString step.provisionalRedacted ++
    ",\"sealed\":" ++ jsonArray (step.sealed.map claudeStreamBlockJson) ++ "}"

def promptAssemblyClaudeThinkingStreamCaseJson
    (witness : PromptAssemblyClaudeThinkingStreamCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"surface\":" ++ jsonStringArray witness.surface ++
    ",\"events\":" ++ jsonArray (witness.events.map claudeStreamEventJson) ++
    ",\"outcome\":" ++ jsonString witness.outcome ++
    ",\"steps\":" ++ jsonArray (witness.steps.map claudeContentStepJson) ++
    ",\"content\":" ++ jsonArray (witness.content.map claudeStreamBlockJson) ++ "}"

def promptAssemblyClaudeThinkingStreamCasesJson : String :=
  jsonArray (promptAssemblyClaudeThinkingStreamCases.map promptAssemblyClaudeThinkingStreamCaseJson)

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

def promptAssemblyServiceTierCasesJson : String :=
  jsonArray (PromptAssembly.ServiceTier.cases.map fun witness =>
    let expected := PromptAssembly.ServiceTier.decide witness.family witness.requested
    "{\"family\":" ++ jsonString witness.family.wireName ++
    ",\"requested\":" ++ jsonOptionalString (witness.requested.map (·.configName)) ++
    ",\"allowed\":" ++ boolString expected.allowed ++
    ",\"wire\":" ++ jsonOptionalString expected.wire ++ "}")


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

deriving instance Lean.ToJson for DiscoveryGuidanceCase

/-- Whether the prompt preamble carries the tool-discovery workflow for an
enablement and the tools actually presented: production `build_preamble`. -/
@[conformance_family]
def discoveryGuidancePresented : Family DiscoveryGuidanceCase Bool where
  inputs := discoveryGuidanceCases
  model := DiscoveryGuidanceCase.expected

end Conformance.Contracts
