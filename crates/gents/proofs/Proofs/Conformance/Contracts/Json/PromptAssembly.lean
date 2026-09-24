import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.PromptAssembly

namespace Conformance.Contracts

open Conformance.ContractCases

def currentInputCaseJson (witness : CurrentInputCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
  ",\"current_request\":" ++ jsonString witness.currentRequest ++
  ",\"headers\":" ++ jsonArray (witness.headers.map fun row =>
    "{\"request\":" ++ jsonString row.request ++ ",\"kind\":" ++ jsonString row.kind ++ "}") ++
  ",\"retained_indices\":" ++ jsonArray (witness.retainedIndices.map toString) ++ "}"

def currentInputCasesJson : String := jsonArray (currentInputCases.map currentInputCaseJson)

def jsonNatArray (values : List Nat) : String :=
  jsonArray (values.map toString)

def promptAssemblyItemJson (item : PromptAssemblyItemCase) : String :=
  "{"
    ++ "\"item\":" ++ jsonString item.item ++ ","
    ++ "\"value\":" ++ toString item.value
    ++ "}"

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

def promptAssemblyClaudeBodyCaseJson (witness : PromptAssemblyClaudeBodyCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"preamble\":" ++ jsonOptionalString witness.preamble ++ ","
    ++ "\"rows\":" ++ jsonStringArray witness.rows ++ ","
    ++ "\"tools\":" ++ jsonStringArray witness.tools ++ ","
    ++ "\"system\":" ++ jsonStringArray witness.system ++ ","
    ++ "\"tools_present\":" ++ boolString witness.toolsPresent
    ++ ",\"supported_efforts\":" ++ jsonOptionalStringArray witness.supportedEfforts
    ++ ",\"effort\":" ++ jsonOptionalString witness.effort
    ++ ",\"selected_effort\":" ++ jsonOptionalString witness.selectedEffort
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

end Conformance.Contracts
