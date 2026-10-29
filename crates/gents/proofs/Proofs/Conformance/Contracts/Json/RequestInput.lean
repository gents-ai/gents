import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RequestInput
import Proofs.PromptAssembly.CapturedContext

namespace Conformance.Contracts
open Conformance.ContractCases
open Enrollment

private def optionalString : Option String → String
  | none => "null"
  | some s => jsonString s

private def titleJson : Option AgentSession.Title → String
  | none => "null"
  | some title =>
      let source := title.source.toWireName
      "{\"text\":" ++ jsonString title.text ++ ",\"source\":" ++ jsonString source ++ "}"

private def queueJson : Option RequestQueue → String
  | none => "null"
  | some q =>
      "{\"source\":" ++ jsonString q.source.toDefraDB ++
      ",\"policy\":" ++ jsonString q.policy.toDefraDB ++
      ",\"key\":" ++ optionalString q.key ++
      ",\"queued_after_request_id\":" ++ optionalString q.queuedAfterRequestId ++
      ",\"interrupted_request_id\":" ++ optionalString q.interruptedRequestId ++
      ",\"background_completion_wake_version\":" ++ jsonOptionalNat q.backgroundCompletionWakeVersion ++
      ",\"delivery\":" ++ jsonString q.delivery.toDefraDB ++
      ",\"position\":" ++ (match q.position with
        | none => "null"
        | some p => "{\"slot_request_doc_id\":" ++ jsonString p.slotRequestDocId ++
            ",\"replaces_request_doc_id\":" ++ jsonString p.replacesRequestDocId ++ "}") ++ "}"

private def goalJson : Option GoalContinuationInput → String
  | none => "null"
  | some goal => "{\"sequence\":" ++ toString goal.sequence ++
      ",\"wrapup\":" ++ boolString goal.wrapup ++ "}"

private def admissionKindJson : AgentRequestAdmissionKind → String
  | .enrollment => jsonString "enrollment"
  | .localSelf => jsonString "local-self"
  | .runtimeInternal => jsonString "runtime-internal"
  | .peer => jsonString "peer"

private def runtimeSourceJson : RuntimeInternalSourceKind → String
  | .localControl => jsonString "local-control"
  | .automatedTrigger => jsonString "automated-trigger"

private def workspaceJson (w : RequestWorkspace) : String :=
  "{\"workspace_id\":" ++ optionalString w.workspaceId ++
  ",\"workspace_owner_node_did\":" ++ optionalString w.ownerNodeDid ++
  ",\"workspace_authority\":" ++ optionalString (w.authority.map BindingAuthority.toDefraDB) ++
  ",\"workspace_seal_hash\":" ++ optionalString w.sealHash ++ "}"

private def capturedContextJson (context : CapturedContext) : String :=
  (PromptAssembly.CapturedContext.contextJson context).compress

private def capturedNativeBlockJson : CanonicalOutput.MessageBlock (List UInt8) → String
  | .text bytes =>
      "{\"type\":\"text\",\"text\":" ++ jsonString (String.fromUTF8! ⟨bytes.toArray⟩) ++ "}"
  | .media media =>
      let metadata := ((Lean.Json.parse (media.additionalParams.getD "{}")) >>= fun value =>
        value.getObjVal? "captured_context").toOption.getD Lean.Json.null
      let text := match media.data with
        | .string bytes => String.fromUTF8! ⟨bytes.toArray⟩
        | _ => panic! "captured context must reference native string bytes"
      "{\"type\":\"document\",\"data\":{\"type\":\"string\",\"value\":" ++ jsonString text ++
      "},\"media_type\":" ++ jsonString (media.mediaType.getD "") ++
      ",\"captured_context\":" ++ metadata.compress ++ "}"
  | _ => panic! "captured input must contain only prose and documents"

private def attachmentJson (a : RequestAttachment) : String :=
  "{\"name\":" ++ jsonString a.name ++ ",\"mediaType\":" ++ jsonString a.mediaType ++
    ",\"data\":" ++ jsonString a.data ++ "}"

private def attachmentNativeJson (a : RequestAttachment) (metadata : Bool) : String :=
  let image := a.mediaType.startsWith "image/"
  let media := if image then a.mediaType.drop 6 else if a.mediaType == "application/pdf" then "pdf" else "txt"
  "{\"type\":" ++ jsonString (if image then "image" else "document") ++
    ",\"data\":{\"type\":\"base64\",\"value\":" ++ jsonString a.data ++ "},\"media_type\":" ++ jsonString media ++
    (if image then ",\"detail\":" ++ jsonString attachmentImageDetail else "") ++
    (if metadata then ",\"gents_attachment\":" ++ jsonString a.name else "") ++ "}"

private def attachmentProviderJson (a : RequestAttachment) : String :=
  if a.providerKind == "text" then
    "{\"type\":\"text\",\"prefix\":" ++ jsonString (attachmentTextHeader (jsonString a.name)) ++
      ",\"encoded_text\":" ++ jsonString a.data ++ "}"
  else attachmentNativeJson a false

private def capturedNativeMessageJson (c : RequestInputCase) : String :=
  let message := PromptAssembly.CapturedContext.authoredMessage c.authoredText c.input.capturedContext
  let blocks := if c.authoredText.isEmpty && !c.input.attachments.isEmpty && c.input.capturedContext.isEmpty then [] else message.blocks.map capturedNativeBlockJson
  "{\"role\":\"user\",\"content\":" ++ jsonArray (blocks ++ c.input.attachments.map (fun a => attachmentNativeJson a true)) ++ "}"

private def assignmentJson : Option GoalAssignmentInput → String
  | none => "null"
  | some reference => "{\"goal_id\":" ++ jsonString reference.goalId ++
      ",\"root_request_doc_id\":" ++ optionalString reference.rootRequestDocId ++ "}"

private def requestInputJson (input : RequestInput) : String :=
  "{\"selected_skill_ids\":" ++ jsonStringArray input.selectedSkillIds ++
  ",\"cwd\":" ++ optionalString input.cwd ++
  ",\"initial_title\":" ++ titleJson input.initialTitle ++
  ",\"queue\":" ++ queueJson input.queue ++
  ",\"goal_continuation\":" ++ goalJson input.goalContinuation ++
  ",\"captured_context\":" ++ jsonArray (input.capturedContext.map capturedContextJson) ++
  ",\"goal_assignment\":" ++ assignmentJson input.goalAssignment ++
  ",\"attachments\":" ++ jsonArray (input.attachments.map attachmentJson) ++ "}"

private def requestInputCaseJson (c : RequestInputCase) : String :=
  let queue := c.input.queue.getD { source := .backgroundCompletion, policy := .coalesce }
  "{\"name\":" ++ jsonString c.name ++
  ",\"input\":" ++ requestInputJson c.input ++
  ",\"authored_text\":" ++ jsonString c.authoredText ++
  ",\"runtime_input_version\":" ++ jsonOptionalNat c.runtimeInputVersion ++
  ",\"runtime_goal_version\":" ++ jsonOptionalNat c.runtimeGoalVersion ++
  ",\"expected_goal_assignment_compatible\":" ++ boolString (goalAssignmentCompatible c.input c.runtimeGoalVersion) ++
  ",\"expected_captured_input_compatible\":" ++ boolString (capturedInputCompatible c.input c.runtimeInputVersion) ++
  ",\"expected_attachments_well_formed\":" ++ boolString (attachmentsWellFormed c.input.attachments) ++
  ",\"expected_attachment_parts\":" ++ jsonArray (c.input.attachments.map attachmentProviderJson) ++
  ",\"expected_chat_attachments_supported\":" ++ boolString (c.input.attachments.all RequestAttachment.chatCompletionsSupported) ++
  ",\"expected_has_authored_input\":" ++ boolString (hasAuthoredInput c.input c.authoredText) ++
  ",\"expected_native_input\":" ++ capturedNativeMessageJson c ++
  ",\"expected_provider_parts\":" ++ jsonOptionalStringArray (if c.authoredText.isEmpty && !c.input.attachments.isEmpty && c.input.capturedContext.isEmpty then some [] else PromptAssembly.CapturedContext.providerParts? c.authoredText c.input.capturedContext) ++
  ",\"continuation_parent_doc_id\":" ++ jsonString "physical:parent" ++
  ",\"continuation_queue\":" ++ queueJson (some queue) ++
  ",\"continuation_goal\":" ++ goalJson c.input.goalContinuation ++
  ",\"expected_continuation\":" ++ requestInputJson (continuationInput c.input queue c.input.goalContinuation "physical:parent") ++
  ",\"reissued_source_doc_id\":" ++ jsonString "physical:parent" ++
  ",\"expected_reissued_input\":" ++ requestInputJson (reissuedInput c.input "physical:parent") ++
  ",\"edit_queue\":" ++ queueJson (some queue) ++
  ",\"edit_authored_input\":" ++ requestInputJson ({} : RequestInput) ++
  ",\"expected_edited_input\":" ++ requestInputJson (editedInput c.input {} "physical:parent" queue) ++
  ",\"expected_edited_from_empty\":" ++ requestInputJson (editedInput {} c.input "physical:parent" queue) ++
  ",\"expected_edit_directory_same\":" ++ boolString (editDirectoryCompatible c.input (editedInput c.input {} "physical:parent" queue)) ++
  ",\"expected_edit_directory_changed\":" ++ boolString (editDirectoryCompatible c.input { c.input with cwd := some "/gents-edit-different" }) ++
  ",\"expected_fresh_input\":" ++ requestInputJson (freshSessionInput c.input) ++
  ",\"workspace\":" ++ workspaceJson c.workspace ++
  ",\"workspace_source\":" ++ workspaceJson c.workspaceSource ++
  ",\"workspace_source_authenticated\":" ++ boolString c.workspaceSourceAuthenticated ++
  ",\"expected_workspace_accepted\":" ++ boolString (requestWorkspaceWithinSource c.workspace c.workspaceSource c.workspaceSourceAuthenticated) ++
  ",\"admission_kind\":" ++ admissionKindJson c.admissionKind ++
  ",\"runtime_source\":" ++ runtimeSourceJson c.runtimeSource ++
  ",\"verified_goal_continuation\":" ++ goalJson c.verifiedGoalContinuation ++
  ",\"context_skill_ids\":" ++ jsonStringArray c.contextSkillIds ++
  ",\"cwd_allowed\":" ++ boolString c.cwdAllowed ++
  ",\"queue_source_allowed\":" ++ boolString c.queueSourceAllowed ++
  ",\"agent\":" ++ jsonString c.agent ++
  ",\"session_agent\":" ++ jsonString c.sessionAgent ++
  ",\"session_exists\":" ++ boolString c.sessionExists ++
  ",\"current_title\":" ++ titleJson c.currentTitle ++
  ",\"canonical_input_fields\":" ++
    jsonArray ((requestInputFields c.input).map (fun field =>
      jsonArray (field.map (fun byte => toString byte.toNat)))) ++
  ",\"canonical_input_bytes\":" ++
    jsonArray ((serializeWireFields (requestInputFields c.input)).map (fun byte => toString byte.toNat)) ++
  ",\"expected_captures_well_formed\":" ++ boolString (c.input.capturedContext.all CapturedContext.wellFormed) ++
  ",\"expected_accepted\":" ++ boolString c.accepted ++
  ",\"expected_title\":" ++ titleJson (materializedTitle c.sessionExists c.currentTitle c.input) ++ "}"

def requestInputCasesJson : String := jsonArray (requestInputCases.map requestInputCaseJson)
end Conformance.Contracts
