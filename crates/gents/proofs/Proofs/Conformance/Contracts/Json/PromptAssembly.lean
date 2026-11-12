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
