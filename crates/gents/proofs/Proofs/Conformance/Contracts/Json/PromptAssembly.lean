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
