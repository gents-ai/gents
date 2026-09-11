import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RequestInput

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
      ",\"background_completion_wake_version\":" ++ jsonOptionalNat q.backgroundCompletionWakeVersion ++ "}"

private def goalJson : Option GoalContinuationInput → String
  | none => "null"
  | some goal => "{\"sequence\":" ++ toString goal.sequence ++
      ",\"wrapup\":" ++ boolString goal.wrapup ++ "}"

private def admissionKindJson : AgentRequestAdmissionKind → String
  | .enrollment => jsonString "enrollment"
  | .localSelf => jsonString "local-self"
  | .runtimeInternal => jsonString "runtime-internal"

private def runtimeSourceJson : RuntimeInternalSourceKind → String
  | .localChild => jsonString "local-child"
  | .crossPrincipalChild => jsonString "cross-principal-child"
  | .localControl => jsonString "local-control"
  | .automatedTrigger => jsonString "automated-trigger"

private def workspaceJson (w : RequestWorkspace) : String :=
  "{\"workspace_id\":" ++ optionalString w.workspaceId ++
  ",\"workspace_owner_agent_did\":" ++ optionalString w.ownerAgentDid ++
  ",\"workspace_authority\":" ++ optionalString (w.authority.map BindingAuthority.toDefraDB) ++
  ",\"workspace_seal_hash\":" ++ optionalString w.sealHash ++ "}"

private def requestInputCaseJson (c : RequestInputCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"input\":{\"selected_skill_ids\":" ++ jsonStringArray c.input.selectedSkillIds ++
  ",\"cwd\":" ++ optionalString c.input.cwd ++
  ",\"initial_title\":" ++ titleJson c.input.initialTitle ++
  ",\"queue\":" ++ queueJson c.input.queue ++
  ",\"goal_continuation\":" ++ goalJson c.input.goalContinuation ++ "}" ++
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
  ",\"behavior\":" ++ jsonString c.behavior ++
  ",\"session_behavior\":" ++ jsonString c.sessionBehavior ++
  ",\"session_exists\":" ++ boolString c.sessionExists ++
  ",\"current_title\":" ++ titleJson c.currentTitle ++
  ",\"canonical_input_fields\":" ++
    jsonArray ((requestInputFields c.input).map (fun field =>
      jsonArray (field.map (fun byte => toString byte.toNat)))) ++
  ",\"canonical_input_bytes\":" ++
    jsonArray ((serializeWireFields (requestInputFields c.input)).map (fun byte => toString byte.toNat)) ++
  ",\"expected_accepted\":" ++ boolString c.accepted ++
  ",\"expected_title\":" ++ titleJson (materializedTitle c.sessionExists c.currentTitle c.input) ++ "}"

def requestInputCasesJson : String := jsonArray (requestInputCases.map requestInputCaseJson)
end Conformance.Contracts
