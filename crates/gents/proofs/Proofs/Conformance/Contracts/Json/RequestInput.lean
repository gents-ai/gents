import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RequestInput
import Proofs.Session.PromptAssembly.CapturedContext

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
  | .client => jsonString "client"
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
  ",\"expected_agent_new_local_cwd\":" ++ optionalString (agentNewCwd c.input true) ++
  ",\"expected_agent_new_remote_cwd\":" ++ optionalString (agentNewCwd c.input false) ++
  ",\"workspace\":" ++ workspaceJson c.workspace ++
  ",\"workspace_source\":" ++ workspaceJson c.workspaceSource ++
  ",\"workspace_source_authenticated\":" ++ boolString c.workspaceSourceAuthenticated ++
  ",\"expected_workspace_accepted\":" ++ boolString (requestWorkspaceWithinSource c.workspace c.workspaceSource c.workspaceSourceAuthenticated) ++
  ",\"admission_kind\":" ++ admissionKindJson c.admissionKind ++
  ",\"runtime_source\":" ++ runtimeSourceJson c.runtimeSource ++
  ",\"verified_goal_continuation\":" ++ goalJson c.verifiedGoalContinuation ++
  ",\"expected_client_claim_queue_candidate\":" ++ boolString (clientClaimQueueCandidate c.input false) ++
  ",\"expected_agent_message_claim_queue_candidate\":" ++ boolString (clientClaimQueueCandidate c.input true) ++
  ",\"client_goal_claim_binding_cases\":" ++ jsonArray
    (([false, true] : List Bool).flatMap fun visible =>
      ([false, true] : List Bool).flatMap fun goalMatches =>
      ([false, true] : List Bool).flatMap fun root =>
      ([false, true] : List Bool).map fun agent =>
        "{\"visible\":" ++ boolString visible ++ ",\"matches\":" ++ boolString goalMatches ++
        ",\"root\":" ++ boolString root ++ ",\"agent\":" ++ boolString agent ++ ",\"expected\":" ++
        boolString (clientGoalClaimBinding visible goalMatches root agent) ++ "}") ++
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

namespace Conformance.RequestInputContracts

open Lean Conformance.ContractCases Conformance.Contracts Enrollment

structure TitleWire where
  text : String
  source : String
  deriving ToJson

structure PositionWire where
  slotRequestDocId : String
  replacesRequestDocId : String
  deriving ToJson

structure QueueWire where
  source : String
  policy : String
  key : Option String
  queuedAfterRequestId : Option String
  interruptedRequestId : Option String
  backgroundCompletionWakeVersion : Option Nat
  delivery : String
  position : Option PositionWire
  deriving ToJson

structure GoalWire where
  sequence : Int
  wrapup : Bool
  deriving ToJson

structure CapturedPositionWire where
  line : Nat
  character : Nat
  deriving ToJson

structure CapturedRangeWire where
  start : CapturedPositionWire
  «end» : CapturedPositionWire
  deriving ToJson

structure CapturedWire where
  kind : String
  source : String
  capturedAt : String
  uri : Option String
  terminalName : Option String
  workspaceUri : Option String
  version : Option Nat
  unsaved : Bool
  range : Option CapturedRangeWire
  content : String
  deriving ToJson

structure AssignmentWire where
  goalId : String
  rootRequestDocId : Option String
  deriving ToJson

structure AttachmentWire where
  name : String
  mediaType : String
  data : String
  deriving ToJson

structure InputWire where
  selectedSkillIds : List String
  cwd : Option String
  initialTitle : Option TitleWire
  queue : Option QueueWire
  goalContinuation : Option GoalWire
  capturedContext : List CapturedWire
  goalAssignment : Option AssignmentWire
  attachments : List AttachmentWire
  deriving ToJson

structure WorkspaceWire where
  workspaceId : Option String
  workspaceOwnerNodeDid : Option String
  workspaceAuthority : Option String
  workspaceSealHash : Option String
  deriving ToJson

def QueueWire.ofModel (queue : RequestQueue) : QueueWire :=
  { source := queue.source.toDefraDB, policy := queue.policy.toDefraDB, key := queue.key
    queuedAfterRequestId := queue.queuedAfterRequestId
    interruptedRequestId := queue.interruptedRequestId
    backgroundCompletionWakeVersion := queue.backgroundCompletionWakeVersion
    delivery := queue.delivery.toDefraDB
    position := queue.position.map fun p => ⟨p.slotRequestDocId, p.replacesRequestDocId⟩ }

def GoalWire.ofModel (goal : GoalContinuationInput) : GoalWire := ⟨goal.sequence, goal.wrapup⟩

def CapturedWire.ofModel (context : CapturedContext) : CapturedWire :=
  { kind := context.kind.toWireName, source := context.source.toWireName
    capturedAt := context.capturedAt, uri := context.uri, terminalName := context.terminalName
    workspaceUri := context.workspaceUri, version := context.version, unsaved := context.unsaved
    range := context.range.map fun r =>
      ⟨⟨r.start.line, r.start.character⟩, ⟨r.end.line, r.end.character⟩⟩
    content := context.content }

def InputWire.ofModel (input : RequestInput) : InputWire :=
  { selectedSkillIds := input.selectedSkillIds, cwd := input.cwd
    initialTitle := input.initialTitle.map fun t => ⟨t.text, t.source.toWireName⟩
    queue := input.queue.map QueueWire.ofModel
    goalContinuation := input.goalContinuation.map GoalWire.ofModel
    capturedContext := input.capturedContext.map CapturedWire.ofModel
    goalAssignment := input.goalAssignment.map fun r => ⟨r.goalId, r.rootRequestDocId⟩
    attachments := input.attachments.map fun a => ⟨a.name, a.mediaType, a.data⟩ }

def WorkspaceWire.ofModel (workspace : RequestWorkspace) : WorkspaceWire :=
  ⟨workspace.workspaceId, workspace.ownerNodeDid,
    workspace.authority.map BindingAuthority.toDefraDB, workspace.sealHash⟩

/-- The physical document a runtime continuation, reissue or edit names as its
parent or source; the opening request's self reference resolves to it. -/
def parentDocId : String := "physical:parent"

def continuationQueue (c : RequestInputCase) : RequestQueue :=
  c.input.queue.getD { source := .backgroundCompletion, policy := .coalesce }

example : (requestInputCases.map (·.name)).Nodup := by native_decide

def caseNamed (name : String) : RequestInputCase :=
  (requestInputCases.find? (·.name == name)).getD { name, input := {}, contextSkillIds := [] }

private def parsed (text : String) : Json := (Json.parse text).toOption.getD Json.null

def nativeInput (c : RequestInputCase) : Json := parsed (capturedNativeMessageJson c)

def providerAccepted (c : RequestInputCase) : Bool :=
  attachmentsWellFormed c.input.attachments && c.input.capturedContext.all CapturedContext.wellFormed

structure DerivationInput where
  name : String
  input : InputWire
  authoredText : String
  runtimeInputVersion : Option Nat
  runtimeGoalVersion : Option Nat
  continuationParentDocId : String
  continuationQueue : QueueWire
  continuationGoal : Option GoalWire
  reissuedSourceDocId : String
  editQueue : QueueWire
  editAuthoredInput : InputWire
  deriving ToJson

structure Derived where
  capturedInputCompatible : Bool
  goalAssignmentCompatible : Bool
  capturesWellFormed : Bool
  attachmentsWellFormed : Bool
  hasAuthoredInput : Bool
  nativeInput : Json
  agentNewLocalCwd : Option String
  agentNewRemoteCwd : Option String
  continuation : InputWire
  freshInput : InputWire
  reissuedInput : InputWire
  editedInput : InputWire
  editedFromEmpty : InputWire
  deriving ToJson

@[conformance_family]
def derivation : Family DerivationInput Derived where
  inputs := requestInputCases.map fun c =>
    { name := c.name, input := .ofModel c.input, authoredText := c.authoredText
      runtimeInputVersion := c.runtimeInputVersion, runtimeGoalVersion := c.runtimeGoalVersion
      continuationParentDocId := parentDocId, continuationQueue := .ofModel (continuationQueue c)
      continuationGoal := c.input.goalContinuation.map GoalWire.ofModel
      reissuedSourceDocId := parentDocId, editQueue := .ofModel (continuationQueue c)
      editAuthoredInput := .ofModel {} }
  model := fun i =>
    let c := caseNamed i.name
    let queue := continuationQueue c
    { capturedInputCompatible := capturedInputCompatible c.input c.runtimeInputVersion
      goalAssignmentCompatible := goalAssignmentCompatible c.input c.runtimeGoalVersion
      capturesWellFormed := c.input.capturedContext.all CapturedContext.wellFormed
      attachmentsWellFormed := attachmentsWellFormed c.input.attachments
      hasAuthoredInput := hasAuthoredInput c.input c.authoredText
      nativeInput := nativeInput c
      agentNewLocalCwd := agentNewCwd c.input true
      agentNewRemoteCwd := agentNewCwd c.input false
      continuation := .ofModel (continuationInput c.input queue c.input.goalContinuation parentDocId)
      freshInput := .ofModel (freshSessionInput c.input)
      reissuedInput := .ofModel (reissuedInput c.input parentDocId)
      editedInput := .ofModel (editedInput c.input {} parentDocId queue)
      editedFromEmpty := .ofModel (editedInput {} c.input parentDocId queue) }

structure SigningInput where
  name : String
  input : InputWire
  deriving ToJson

structure Signed where
  canonicalInputFields : List (List Nat)
  deriving ToJson

@[conformance_family]
def signedFields : Family SigningInput Signed where
  inputs := requestInputCases.map fun c => { name := c.name, input := .ofModel c.input }
  model := fun i =>
    { canonicalInputFields :=
        (requestInputFields (caseNamed i.name).input).map fun field => field.map (·.toNat) }

structure ProviderInput where
  name : String
  input : InputWire
  authoredText : String
  deriving ToJson

structure ProviderParts where
  chatAttachmentsSupported : Bool
  providerParts : List String
  attachmentParts : Json
  deriving ToJson

/-- The provider boundary converts only inputs whose attachments and captures
are well formed; `providerRefusal` replays the rest. -/
@[conformance_family]
def provider : Family ProviderInput ProviderParts where
  inputs := (requestInputCases.filter providerAccepted).map fun c =>
    { name := c.name, input := .ofModel c.input, authoredText := c.authoredText }
  model := fun i =>
    let c := caseNamed i.name
    { chatAttachmentsSupported := c.input.attachments.all RequestAttachment.chatCompletionsSupported
      providerParts :=
        if c.authoredText.isEmpty && !c.input.attachments.isEmpty && c.input.capturedContext.isEmpty
        then [] else PromptAssembly.CapturedContext.providerParts c.authoredText c.input.capturedContext
      attachmentParts := parsed (jsonArray (c.input.attachments.map attachmentProviderJson)) }

structure ProviderRefused where
  chatAttachmentsSupported : Bool
  providerAccepted : Bool
  deriving ToJson

@[conformance_family]
def providerRefusal : Family ProviderInput ProviderRefused where
  inputs := (requestInputCases.filter (!providerAccepted ·)).map fun c =>
    { name := c.name, input := .ofModel c.input, authoredText := c.authoredText }
  model := fun i =>
    let c := caseNamed i.name
    { chatAttachmentsSupported := c.input.attachments.all RequestAttachment.chatCompletionsSupported
      providerAccepted := providerAccepted c }

structure BindingInput where
  visible : Bool
  «matches» : Bool
  root : Bool
  agent : Bool
  deriving ToJson

@[conformance_family]
def goalClaimBinding : Family BindingInput Bool where
  inputs := ([false, true] : List Bool).flatMap fun visible =>
    ([false, true] : List Bool).flatMap fun goalMatches =>
    ([false, true] : List Bool).flatMap fun root =>
    ([false, true] : List Bool).map fun agent => ⟨visible, goalMatches, root, agent⟩
  model := fun i => clientGoalClaimBinding i.visible i.«matches» i.root i.agent

structure CandidateInput where
  name : String
  input : InputWire
  deriving ToJson

structure Candidates where
  clientClaimQueueCandidate : Bool
  agentMessageClaimQueueCandidate : Bool
  deriving ToJson

@[conformance_family]
def claimQueueCandidate : Family CandidateInput Candidates where
  inputs := requestInputCases.map fun c => { name := c.name, input := .ofModel c.input }
  model := fun i =>
    let c := caseNamed i.name
    { clientClaimQueueCandidate := clientClaimQueueCandidate c.input false
      agentMessageClaimQueueCandidate := clientClaimQueueCandidate c.input true }

example : ["client-goal-original-edge-preserved", "goal-source-needs-owned-admission",
    "client-unrelated-runtime-queue-denied", "client-local-agent-message-steering-candidate"].all
    (fun name => requestInputCases.any (·.name == name)) := by native_decide

structure WorkspaceInput where
  name : String
  workspace : WorkspaceWire
  workspaceSource : WorkspaceWire
  workspaceSourceAuthenticated : Bool
  deriving ToJson

structure WorkspaceAdmission where
  workspaceAccepted : Bool
  deriving ToJson

@[conformance_family]
def workspaceSource : Family WorkspaceInput WorkspaceAdmission where
  inputs := requestInputCases.map fun c =>
    { name := c.name, workspace := .ofModel c.workspace, workspaceSource := .ofModel c.workspaceSource
      workspaceSourceAuthenticated := c.workspaceSourceAuthenticated }
  model := fun i =>
    let c := caseNamed i.name
    { workspaceAccepted :=
        requestWorkspaceWithinSource c.workspace c.workspaceSource c.workspaceSourceAuthenticated }

example : requestInputCases.any (·.name == "workspace-foreign-owner-same-label-denied") := by
  native_decide

structure EditInput where
  name : String
  input : InputWire
  editQueue : QueueWire
  editAuthoredInput : InputWire
  deriving ToJson

structure EditDirectory where
  editDirectorySame : Bool
  editDirectoryChanged : Bool
  deriving ToJson

@[conformance_family]
def editDirectory : Family EditInput EditDirectory where
  inputs := requestInputCases.map fun c =>
    { name := c.name, input := .ofModel c.input, editQueue := .ofModel (continuationQueue c)
      editAuthoredInput := .ofModel {} }
  model := fun i =>
    let c := caseNamed i.name
    { editDirectorySame :=
        editDirectoryCompatible c.input (editedInput c.input {} parentDocId (continuationQueue c))
      editDirectoryChanged :=
        editDirectoryCompatible c.input { c.input with cwd := some "/gents-edit-different" } }

structure ReplacementInput where
  name : String
  input : InputWire
  reissuedSourceDocId : String
  deriving ToJson

/-- A pending-message replacement reissues its source without the title
intent; the queue owner supplies its queue, which the scenario checks against
the replaced slot. -/
structure Replacement where
  replacementInput : InputWire
  deriving ToJson

@[conformance_family]
def replacement : Family ReplacementInput Replacement where
  inputs := requestInputCases.map fun c =>
    { name := c.name, input := .ofModel c.input, reissuedSourceDocId := parentDocId }
  model := fun i =>
    { replacementInput := .ofModel
        { reissuedInput (caseNamed i.name).input parentDocId with initialTitle := none, queue := none } }

/-- The served admission fixture signs a fresh local-self request in a session
whose Agent selects a context without skills or a directory policy, and then
tampers with the first stored capture; only captured inputs without a
directory can be built there. -/
def admittedCaptureCases : List RequestInputCase :=
  requestInputCases.filter fun c => !c.input.capturedContext.isEmpty && c.input.cwd.isNone

structure AdmissionInput where
  name : String
  input : InputWire
  deriving ToJson

structure Admission where
  accepted : Bool
  deriving ToJson

@[conformance_family]
def admittedCapture : Family AdmissionInput Admission where
  inputs := admittedCaptureCases.map fun c => { name := c.name, input := .ofModel c.input }
  model := fun i => { accepted := (caseNamed i.name).accepted }

/-- Publication stores the native authored message of each input the provider
boundary accepts and that carries captures or attachments; plain text has no
sealed document to reconstruct. -/
def publishedCaptureCases : List RequestInputCase :=
  requestInputCases.filter fun c =>
    (!c.input.capturedContext.isEmpty || !c.input.attachments.isEmpty) && providerAccepted c

structure Publication where
  nativeInput : Json
  deriving ToJson

@[conformance_family]
def publishedCapture : Family ProviderInput Publication where
  inputs := publishedCaptureCases.map fun c =>
    { name := c.name, input := .ofModel c.input, authoredText := c.authoredText }
  model := fun i => { nativeInput := nativeInput (caseNamed i.name) }

example : requestInputCases.any (·.input.cwd.isSome) ∧
    requestInputCases.any (·.name == "captured-selection") := by native_decide

end Conformance.RequestInputContracts
