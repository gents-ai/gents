import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Core
import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

def agentRequestAdmissionCaseJson (c : AgentRequestAdmissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"observation_available\":" ++ boolJson c.observationAvailable ++ ","
    ++ "\"kind\":" ++ jsonString c.kind ++ ","
    ++ "\"signature_valid\":" ++ boolJson c.signatureValid ++ ","
    ++ "\"signed_fields_match\":" ++ boolJson c.signedFieldsMatch ++ ","
    ++ "\"branch_fields_exact\":" ++ boolJson c.branchFieldsExact ++ ","
    ++ "\"pending_deadline_absent\":" ++ boolJson c.pendingDeadlineAbsent ++ ","
    ++ "\"signer_matches_requester\":" ++ boolJson c.signerMatchesRequester ++ ","
    ++ "\"requester_matches_target\":" ++ boolJson c.requesterMatchesTarget ++ ","
    ++ "\"signer_matches_target\":" ++ boolJson c.signerMatchesTarget ++ ","
    ++ "\"signer_matches_issuer\":" ++ boolJson c.signerMatchesIssuer ++ ","
    ++ "\"requester_matches_session_scope\":" ++ boolJson c.requesterMatchesSessionScope ++ ","
    ++ "\"current_approval\":" ++ boolJson c.currentApproval ++ ","
    ++ "\"exact_generation\":" ++ boolJson c.exactGeneration ++ ","
    ++ "\"authorization_fresh\":" ++ boolJson c.authorizationFresh ++ ","
    ++ "\"runtime_evidence_present\":" ++ boolJson c.runtimeEvidencePresent ++ ","
    ++ "\"runtime_source_kind\":" ++ jsonString c.runtimeSourceKind ++ ","
    ++ "\"target_runtime_attestation_valid\":" ++
      boolJson c.targetRuntimeAttestationValid ++ ","
    ++ "\"source_binding_current\":" ++ boolJson c.sourceBindingCurrent ++ ","
    ++ "\"trigger_config_document_binding_current\":" ++
      boolJson c.triggerConfigDocumentBindingCurrent ++ ","
    ++ "\"source_document_binding_current\":" ++ boolJson c.sourceDocumentBindingCurrent ++ ","
    ++ "\"target_policy_allows\":" ++ boolJson c.targetPolicyAllows ++ ","
    ++ "\"native_create_authorized\":" ++ boolJson c.nativeCreateAuthorized ++ ","
    ++ "\"hop_within_bound\":" ++ boolJson c.hopWithinBound ++ ","
    ++ "\"expected_admitted\":" ++ boolJson c.expectedAdmitted ++ ","
    ++ "\"expected_disposition\":" ++ jsonString c.expectedDisposition
    ++ "}"

def agentRequestAdmissionCasesJson : String :=
  jsonArray (agentRequestAdmissionCases.map agentRequestAdmissionCaseJson)

private def titleFieldsHexJson (fields : Enrollment.CanonicalFields) : String :=
  jsonStringArray (fields.map Enrollment.utf8HexString)

private def titleRequestInputJson (input : Enrollment.RequestInput) : String :=
  "{" ++ "\"selected_skill_ids\":" ++ jsonStringArray input.selectedSkillIds ++ ","
    ++ "\"cwd\":" ++ jsonOptionalString input.cwd ++ ","
    ++ "\"initial_title\":" ++ (match input.initialTitle with
      | none => "null"
      | some title => "{\"text\":" ++ jsonString title.text ++
          ",\"source\":" ++ jsonString title.source.toWireName ++ "}") ++ ","
    ++ "\"queue\":" ++ (match input.queue with
      | none => "null"
      | some queue => "{\"source\":" ++ jsonString queue.source.toDefraDB ++
          ",\"policy\":" ++ jsonString queue.policy.toDefraDB ++
          ",\"key\":" ++ jsonOptionalString queue.key ++
          ",\"queued_after_request_id\":" ++
            jsonOptionalString queue.queuedAfterRequestId ++
          ",\"interrupted_request_id\":" ++
            jsonOptionalString queue.interruptedRequestId ++
          ",\"background_completion_wake_version\":" ++
            jsonOptionalNat queue.backgroundCompletionWakeVersion ++
          ",\"delivery\":" ++ jsonString queue.delivery.toDefraDB ++
          ",\"position\":" ++ (match queue.position with
            | none => "null"
            | some p => "{\"slot_request_doc_id\":" ++ jsonString p.slotRequestDocId ++
                ",\"replaces_request_doc_id\":" ++ jsonString p.replacesRequestDocId ++ "}") ++ "}") ++ ","
    ++ "\"goal_continuation\":" ++ (match input.goalContinuation with
      | none => "null"
      | some goal => "{\"sequence\":" ++ toString goal.sequence ++
          ",\"wrapup\":" ++ boolJson goal.wrapup ++ "}") ++ "}"

private def titleParentEvidenceJson (parent : Enrollment.AuxiliaryParentEvidence) : String :=
  "{" ++ "\"request_id\":" ++ jsonString parent.link.requestId ++ ","
    ++ "\"document_id\":" ++ jsonString parent.link.documentId ++ ","
    ++ "\"node_did\":" ++ jsonString parent.nodeDid ++ ","
    ++ "\"session_id\":" ++ jsonString parent.sessionId ++ ","
    ++ "\"agent_did\":" ++ jsonString parent.agentDid ++ ","
    ++ "\"logical_binding_current\":" ++ boolJson parent.logicalBindingCurrent ++ ","
    ++ "\"physical_binding_current\":" ++ boolJson parent.physicalBindingCurrent ++ "}"

private def titleRuntimeEvidenceJson (evidence : Enrollment.RuntimeInternalEvidence) : String :=
  "{" ++ "\"source_kind\":" ++ jsonString (match evidence.sourceKind with
      | .localControl => "local-control"
      | .automatedTrigger => "automated-trigger") ++ ","
    ++ "\"issuer_did\":" ++ jsonString evidence.issuerDid ++ ","
    ++ "\"source_request_id\":" ++ jsonString evidence.sourceRequestId ++ ","
    ++ "\"target_node\":" ++ jsonString evidence.targetNode ++ ","
    ++ "\"target_runtime_attestation_valid\":" ++ boolJson evidence.targetRuntimeAttestationValid ++ ","
    ++ "\"source_binding_current\":" ++ boolJson evidence.sourceBindingCurrent ++ ","
    ++ "\"source_document_binding_current\":" ++ boolJson evidence.sourceDocumentBindingCurrent ++ ","
    ++ "\"title_parent\":" ++
      (match evidence.auxiliaryParent with
      | none => "null"
      | some parent => titleParentEvidenceJson parent) ++ "}"

def titleRequestAdmissionCaseJson (case : TitleRequestAdmissionCase) : String :=
  let request := case.request
  let admission := case.admission
  "{" ++ "\"name\":" ++ jsonString case.name ++ ","
    ++ "\"parent_observed_state\":" ++ jsonString case.parentObservedState.toDefraDB ++ ","
    ++ "\"observation_available\":" ++ boolJson case.observationAvailable ++ ","
    ++ "\"branch_fields_exact\":" ++ boolJson case.branchFieldsExact ++ ","
    ++ "\"pending_deadline_absent\":" ++ boolJson case.pendingDeadlineAbsent ++ ","
    ++ "\"request\":{"
    ++ "\"hop\":" ++ toString request.hop ++ ","
    ++ "\"request_id\":" ++ jsonString request.requestId ++ ","
    ++ "\"purpose\":" ++ jsonString request.purpose.toWire ++ ","
    ++ "\"target_node\":" ++ jsonString request.targetNode ++ ","
    ++ "\"requester_did\":" ++ jsonString request.requesterDid ++ ","
    ++ "\"agent_did\":" ++ jsonString request.agentDid ++ ","
    ++ "\"session_id\":" ++ jsonString request.sessionId ++ ","
    ++ "\"content\":" ++ jsonString request.content ++ ","
    ++ "\"input\":" ++ titleRequestInputJson request.input ++ ","
    ++ "\"model_input_fields_hex\":" ++ titleFieldsHexJson (Enrollment.requestInputFields request.input) ++ ","
    ++ "\"created_at\":" ++ jsonString request.createdAt ++ ","
    ++ "\"trigger_config_document_id\":" ++ jsonString request.triggerConfigDocumentId ++ ","
    ++ "\"model_retry_fields_hex\":" ++ titleFieldsHexJson request.retryFields ++ ","
    ++ "\"model_trigger_fields_hex\":" ++ titleFieldsHexJson request.triggerFields ++ ","
    ++ "\"model_parent_fields_hex\":" ++ titleFieldsHexJson request.parentFields ++ ","
    ++ "\"model_workspace_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.requestWorkspaceFields request.workspace) ++ ","
    ++ "\"model_semantic_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.agentRequestSemanticFields request) ++ "},"
    ++ "\"admission\":{"
    ++ "\"kind\":" ++ jsonString (match admission.kind with
      | .enrollment => "enrollment"
      | .client => "client"
      | .localSelf => "local-self"
      | .runtimeInternal => "runtime-internal"
      | .peer => "peer") ++ ","
    ++ "\"signer_did\":" ++ jsonString admission.signerDid ++ ","
    ++ "\"issuer_did\":" ++ jsonString admission.issuerDid ++ ","
    ++ "\"source_request_id\":" ++ jsonString admission.sourceRequestId ++ ","
    ++ "\"runtime_source_kind\":" ++ jsonString (match admission.runtimeSourceKind with
      | .localControl => "local-control"
      | .automatedTrigger => "automated-trigger") ++ ","
    ++ "\"signature_valid\":" ++ boolJson admission.signatureValid ++ ","
    ++ "\"model_signed_fields_hex\":" ++ titleFieldsHexJson admission.signedFields ++ ","
    ++ "\"model_expected_fields_hex\":" ++
      titleFieldsHexJson (Enrollment.agentRequestAdmissionFields request admission) ++ "},"
    ++ "\"runtime_evidence\":" ++ (match case.runtimeEvidence with
      | none => "null"
      | some evidence => titleRuntimeEvidenceJson evidence) ++ ","
    ++ "\"session_agent\":" ++ jsonString case.sessionAgent ++ ","
    ++ "\"expected_admitted\":" ++ boolJson case.expectedAdmitted ++ ","
    ++ "\"expected_claimable\":" ++ boolJson case.expectedClaimable ++ ","
    ++ "\"expected_disposition\":" ++ jsonString (match case.expectedDisposition with
      | .admit => "admit" | .deny => "deny" | .retry => "retry") ++ ","
    ++ "\"expected_pending_state\":" ++
      jsonOptionalString (case.expectedPendingState.map (·.toDefraDB)) ++ "}"

def titleRequestAdmissionCasesJson : String :=
  jsonArray (titleRequestAdmissionCases.map titleRequestAdmissionCaseJson)

structure TitlePurposeWireInput where
  name : String
  wire : Option String
  deriving Lean.ToJson

deriving instance Lean.ToJson for RequestPurpose

attribute [conformance_vocabulary] RequestPurpose.toWire

@[conformance_family]
def titlePurposeWire : Conformance.Family TitlePurposeWireInput (Option RequestPurpose) where
  inputs :=
    [ { name := "missing-purpose-is-invalid", wire := none }
    , { name := "unknown-purpose-is-invalid", wire := some "titleAudit" }
    , { name := "normal-purpose-explicit", wire := some "normal" }
    , { name := "title-purpose-explicit", wire := some "title-audit" }
    , { name := "access-review-purpose-explicit", wire := some "access-review" }
    , { name := "access-review-camel-case-is-invalid", wire := some "accessReview" } ]
  model := fun i => i.wire.bind RequestPurpose.fromWire?

end Conformance.Contracts
