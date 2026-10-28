import Proofs.AgentSession
import Proofs.Session.State
import Proofs.Enrollment.State
import Proofs.Enrollment.CapturedContext
import Proofs.Workspace.Types

namespace Enrollment

structure QueuePosition where
  slotRequestDocId : String
  replacesRequestDocId : String
  deriving DecidableEq, Repr

structure RequestQueue where
  source : SessionQueue.QueueSource
  policy : SessionQueue.QueuePolicy
  key : Option String := none
  queuedAfterRequestId : Option String := none
  interruptedRequestId : Option String := none
  backgroundCompletionWakeVersion : Option Nat := none
  delivery : SessionQueue.QueueDelivery := .queue
  position : Option QueuePosition := none
  deriving DecidableEq, Repr

structure GoalContinuationInput where
  sequence : Int
  wrapup : Bool
  deriving DecidableEq, Repr

/-- The opening request uses `none` for its own physical document identity.
Descendants replace that sentinel with the authenticated parent's physical ID.
The reference attributes work; existing admission and ACP retain authority. -/
structure GoalAssignmentInput where
  goalId : String
  rootRequestDocId : Option String := none
  deriving DecidableEq, Repr

/-- Attachment bytes travel in the signed request, never as paths to reopen.
Base64 validity and media decoding are wire-codec obligations. Bounds apply to
the encoded payload to bound document replication as well as provider input. -/
structure RequestAttachment where
  name : String
  mediaType : String
  data : String
  deriving DecidableEq, Repr

/-- OpenAI's chat transport requires explicit image detail even when automatic
resolution is desired. -/
def attachmentImageDetail : String := "auto"

/-- The chat-completions adapter cannot transport PDFs as documents. Reject
them rather than allowing its generic document conversion to emit base64 prose. -/
def RequestAttachment.chatCompletionsSupported (a : RequestAttachment) : Bool :=
  a.mediaType != "application/pdf"

/-- UTF-8 text attachments decode at the provider boundary; binary attachments
retain their native media kind. Base64 decoding remains a wire-codec obligation. -/
def RequestAttachment.providerKind (a : RequestAttachment) : String :=
  if a.mediaType == "text/plain" then "text"
  else if a.mediaType.startsWith "image/" then "image" else "document"

/-- The quoted name is supplied by the JSON string codec so filenames cannot
introduce new header lines. Keep attached file text distinct from authored prose. -/
def attachmentTextHeader (quotedName : String) : String :=
  "Attached file " ++ quotedName ++ ":\n\n"

def RequestAttachment.wellFormed (a : RequestAttachment) : Bool :=
  !a.name.trim.isEmpty && a.name.utf8ByteSize <= 1024 &&
    ["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf", "text/plain"].contains a.mediaType &&
    !a.data.isEmpty && a.data.utf8ByteSize <= 5592408

def attachmentsWellFormed (attachments : List RequestAttachment) : Bool :=
  attachments.length <= 4 && attachments.all RequestAttachment.wellFormed &&
    (attachments.map (fun a => a.data.utf8ByteSize)).sum <= 11184816

def attachmentFields (attachments : List RequestAttachment) : List String :=
  if attachments.isEmpty then [] else
    ["attachments", toString attachments.length] ++
      attachments.flatMap (fun a => [a.name, a.mediaType, a.data])

theorem attachment_singleton_binds_content (a b : RequestAttachment)
    (h : attachmentFields [a] = attachmentFields [b]) : a = b := by
  cases a
  cases b
  simp_all [attachmentFields]

theorem empty_attachments_keep_existing_framing : attachmentFields [] = [] := rfl

structure RequestInput where
  selectedSkillIds : List String := []
  cwd : Option String := none
  initialTitle : Option AgentSession.Title := none
  queue : Option RequestQueue := none
  goalContinuation : Option GoalContinuationInput := none
  capturedContext : List CapturedContext := []
  attachments : List RequestAttachment := []
  goalAssignment : Option GoalAssignmentInput := none
  deriving DecidableEq, Repr

/-- User submission and queue replacement may contain only typed captures.
This is content presence, not admission authority or a requirement on internal
runtime requests. -/
def hasAuthoredInput (input : RequestInput) (prose : String) : Bool :=
  !prose.trim.isEmpty || !input.capturedContext.isEmpty || !input.attachments.isEmpty

/-- This observation prevents sending typed captures to a process that cannot
decode them. It grants no authority; the selected runtime's identity, readiness
and request admission retain their existing owners. Plain input does not need
this new contract advertisement. -/
def capturedInputCompatible (input : RequestInput) (runtimeVersion : Option Nat) : Bool :=
  (input.attachments.isEmpty || runtimeVersion == some 2 || runtimeVersion == some 3) &&
    (input.capturedContext.isEmpty || runtimeVersion == some 1 || runtimeVersion == some 2 ||
      runtimeVersion == some 3) &&
    (input.capturedContext.all (fun context => context.source != .composer) ||
      runtimeVersion == some 3)

def goalAssignmentCompatible (input : RequestInput) (runtimeVersion : Option Nat) : Bool :=
  input.goalAssignment.isNone || runtimeVersion == some 3

/-- Retry and queue replacement publish a fresh physical request in the same
session. The opening request's self sentinel must still name its original
physical document; all other captured invocation facts retain their owner. -/
def reissuedInput (source : RequestInput) (sourceDocId : String) : RequestInput :=
  { source with goalAssignment := source.goalAssignment.map fun reference =>
      { reference with rootRequestDocId := some (reference.rootRequestDocId.getD sourceDocId) } }

theorem reissued_input_preserves_attachments (source : RequestInput) (sourceDocId : String) :
    (reissuedInput source sourceDocId).attachments = source.attachments := rfl

theorem reissued_input_preserves_captures (source : RequestInput) (sourceDocId : String) :
    (reissuedInput source sourceDocId).capturedContext = source.capturedContext := rfl

theorem reissued_input_preserves_known_root (source : RequestInput) (sourceDocId root : String)
    (reference : GoalAssignmentInput) (hasReference : source.goalAssignment = some reference)
    (hasRoot : reference.rootRequestDocId = some root) :
    (reissuedInput source sourceDocId).goalAssignment = some reference := by
  simp [reissuedInput, hasReference, hasRoot]
  cases reference
  simp_all

theorem reissued_input_preserves_opening_request (source : RequestInput) (sourceDocId : String)
    (reference : GoalAssignmentInput) (hasReference : source.goalAssignment = some reference)
    (selfRoot : reference.rootRequestDocId = none) :
    (reissuedInput source sourceDocId).goalAssignment =
      some { reference with rootRequestDocId := some sourceDocId } := by
  simp [reissuedInput, hasReference, selfRoot]

theorem reissued_input_does_not_join_an_assignment (source : RequestInput) (sourceDocId : String)
    (unassigned : source.goalAssignment = none) :
    (reissuedInput source sourceDocId).goalAssignment = none := by
  simp [reissuedInput, unassigned]

def freshSessionInput (source : RequestInput) : RequestInput :=
  {source with queue := none, goalContinuation := none, goalAssignment := none}

theorem fresh_session_preserves_attachments (source : RequestInput) :
    (freshSessionInput source).attachments = source.attachments := rfl

/-- Runtime continuations remain in their parent's invocation directory. They
do not replay the parent's title intent, queue position, captured context, or goal issuance facts.
The executing host still validates the inherited directory against its policy. -/
def continuationInput (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) (parentDocId : String) : RequestInput :=
  { cwd := parent.cwd, queue := some queue, goalContinuation := goal
    goalAssignment := parent.goalAssignment.map fun reference =>
      { reference with rootRequestDocId := some (reference.rootRequestDocId.getD parentDocId) } }

theorem continuation_preserves_directory (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) (parentDocId : String) :
    (continuationInput parent queue goal parentDocId).cwd = parent.cwd := rfl

theorem continuation_does_not_replay_creation (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) (parentDocId : String) :
    (continuationInput parent queue goal parentDocId).initialTitle = none := rfl

theorem continuation_does_not_replay_captured_context (parent : RequestInput)
    (queue : RequestQueue) (goal : Option GoalContinuationInput) (parentDocId : String) :
    (continuationInput parent queue goal parentDocId).capturedContext = [] := rfl

theorem continuation_does_not_replay_attachments (parent : RequestInput)
    (queue : RequestQueue) (goal : Option GoalContinuationInput) (parentDocId : String) :
    (continuationInput parent queue goal parentDocId).attachments = [] := rfl

theorem continuation_uses_current_control (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) (parentDocId : String) :
    (continuationInput parent queue goal parentDocId).queue = some queue ∧
    (continuationInput parent queue goal parentDocId).goalContinuation = goal := ⟨rfl, rfl⟩

theorem continuation_keeps_assignment (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) (parentDocId : String) (reference : GoalAssignmentInput)
    (h : parent.goalAssignment = some reference) :
    (continuationInput parent queue goal parentDocId).goalAssignment =
      some { reference with rootRequestDocId := some (reference.rootRequestDocId.getD parentDocId) } := by
  simp [continuationInput, h]

/-- The implementation must use its one canonical input encoder for signing and
verification, rather than concatenating unescaped JSON or arbitrary metadata. -/
private def optionFields {α : Type} (encode : α → List String) : Option α → List String
  | none => ["none"]
  | some value => "some" :: encode value

private def titleFields (title : AgentSession.Title) : List String :=
  [title.text, title.source.toWireName]

/-- Default queue input retains its existing signed bytes. -/
def queueExtensionFields (queue : RequestQueue) : List String :=
  if queue.delivery == .queue && queue.position.isNone then []
  else ["queue-v2", queue.delivery.toDefraDB] ++
    optionFields (fun p => [p.slotRequestDocId, p.replacesRequestDocId]) queue.position

private def queueFields (queue : RequestQueue) : List String :=
  [queue.source.toDefraDB, queue.policy.toDefraDB] ++
  optionFields (fun s => [s]) queue.key ++
  optionFields (fun s => [s]) queue.queuedAfterRequestId ++
  optionFields (fun s => [s]) queue.interruptedRequestId ++
  optionFields (fun n => [toString n]) queue.backgroundCompletionWakeVersion ++
  queueExtensionFields queue

/-- Absent references retain the existing signed request bytes. A present
reference signs both the Goal and opening request; the self sentinel is explicit. -/
def goalAssignmentFields : Option GoalAssignmentInput → List String
  | none => []
  | some reference => ["goal-assignment", reference.goalId] ++
      optionFields (fun s => [s]) reference.rootRequestDocId

def requestInputFields (input : RequestInput) : CanonicalFields :=
  textFieldsToBytes <|
    [toString input.selectedSkillIds.length] ++ input.selectedSkillIds ++
    optionFields (fun s => [s]) input.cwd ++
    optionFields titleFields input.initialTitle ++ optionFields queueFields input.queue ++
    optionFields (fun goal => [toString goal.sequence, toString goal.wrapup]) input.goalContinuation ++
    capturedContextExtensionFields input.capturedContext ++ goalAssignmentFields input.goalAssignment ++
    attachmentFields input.attachments

theorem default_queue_has_no_encoding_extension (queue : RequestQueue) :
    queueExtensionFields {queue with delivery := .queue, position := none} = [] := by
  simp [queueExtensionFields]

theorem queue_extension_encoding_is_injective (left right : RequestQueue)
    (h : queueExtensionFields left = queueExtensionFields right) :
    left.delivery = right.delivery ∧ left.position = right.position := by
  rcases left with ⟨_, _, _, _, _, _, ld, lp⟩
  rcases right with ⟨_, _, _, _, _, _, rd, rp⟩
  cases ld <;> cases rd <;> rcases lp with _ | ⟨ls, lr⟩ <;> rcases rp with _ | ⟨rs, rr⟩ <;>
    simp_all [queueExtensionFields, optionFields, SessionQueue.QueueDelivery.toDefraDB]

/-- Existing context allowlist is the sole skill grant. Invalid activation is
rejected rather than silently authorizing an extra node-wide skill. -/
def inputWithinContext (input : RequestInput) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) : Bool :=
  input.selectedSkillIds.all skillIds.contains &&
    (input.cwd.map cwdAllowed).getD true &&
    (input.queue.map (fun q => queueSourceAllowed q.source)).getD true &&
    input.capturedContext.all CapturedContext.wellFormed && attachmentsWellFormed input.attachments

theorem continuation_retains_host_policy_checks (parent : RequestInput)
    (queue : RequestQueue) (goal : Option GoalContinuationInput) (parentDocId : String) (skills : List String)
    (cwdAllowed : String → Bool) (queueAllowed : SessionQueue.QueueSource → Bool) :
    inputWithinContext (continuationInput parent queue goal parentDocId) skills cwdAllowed queueAllowed =
      ((parent.cwd.map cwdAllowed).getD true && queueAllowed queue.source) := by
  simp [continuationInput, inputWithinContext, attachmentsWellFormed]

/-- Signed source observations do not widen the existing skill, directory or
queue-source grants. Structural acceptance of a capture is not authorization. -/
theorem captures_do_not_widen_context (input : RequestInput) (skills : List String)
    (cwdAllowed : String → Bool) (queueAllowed : SessionQueue.QueueSource → Bool)
    (h : inputWithinContext input skills cwdAllowed queueAllowed = true) :
    inputWithinContext {input with capturedContext := []} skills cwdAllowed queueAllowed = true := by
  simp only [inputWithinContext, Bool.and_eq_true] at h ⊢
  exact ⟨⟨h.1.1, by simp⟩, h.2⟩

def agentMatchesSession (selected observed : String) : Bool :=
  !selected.trim.isEmpty && selected == observed

def materializedTitle (sessionExists : Bool) (current : Option AgentSession.Title)
    (input : RequestInput) : Option AgentSession.Title :=
  if sessionExists then current else input.initialTitle

theorem existing_title_owner_preserved (current : Option AgentSession.Title)
    (input : RequestInput) : materializedTitle true current input = current := by rfl

theorem activation_cannot_expand_allowlist (input : RequestInput) (skills : List String)
    (cwdAllowed : String → Bool) (queueAllowed : SessionQueue.QueueSource → Bool)
    (h : inputWithinContext input skills cwdAllowed queueAllowed = true) :
    ∀ skill ∈ input.selectedSkillIds, skill ∈ skills := by
  simp only [inputWithinContext, Bool.and_eq_true] at h
  simpa using h.1.1.1.1

theorem rejected_queue_source_cannot_grant (input : RequestInput)
    (queue : RequestQueue) (skills : List String) (cwdAllowed : String → Bool)
    (queueAllowed : SessionQueue.QueueSource → Bool)
    (hqueue : input.queue = some queue) (hdeny : queueAllowed queue.source = false) :
    inputWithinContext input skills cwdAllowed queueAllowed = false := by
  simp [inputWithinContext, hqueue, hdeny]

theorem rejected_cwd_cannot_override_host_policy (input : RequestInput)
    (cwd : String) (skills : List String) (cwdAllowed : String → Bool)
    (queueAllowed : SessionQueue.QueueSource → Bool)
    (hcwd : input.cwd = some cwd) (hdeny : cwdAllowed cwd = false) :
    inputWithinContext input skills cwdAllowed queueAllowed = false := by
  simp [inputWithinContext, hcwd, hdeny]

structure RequestWorkspace where
  workspaceId : Option String := none
  ownerNodeDid : Option String := none
  authority : Option BindingAuthority := none
  sealHash : Option String := none
  deriving DecidableEq, Repr

def requestWorkspaceFields (workspace : RequestWorkspace) : CanonicalFields :=
  textFieldsToBytes <|
    optionFields (fun s => [s]) workspace.workspaceId ++
    optionFields (fun s => [s]) workspace.ownerNodeDid ++
    optionFields (fun a => [a.toDefraDB]) workspace.authority ++
    optionFields (fun s => [s]) workspace.sealHash

/-- Workspace ownership/grants/seal validity are resolved by the existing
admission and workspace adapters, never granted by this structural check. -/
def requestWorkspaceWellFormed (workspace : RequestWorkspace) : Bool :=
  match workspace.workspaceId, workspace.ownerNodeDid, workspace.authority with
  | none, none, none => workspace.sealHash.isNone
  | some id, some owner, some _ =>
      !id.trim.isEmpty && id == id.trim &&
      !owner.trim.isEmpty && owner == owner.trim
  | _, _, _ => false

private def authorityRank : BindingAuthority → Nat
  | .readOnly => 0
  | .integrate => 1
  | .readWrite => 2

/-- The source is the existing authenticated exact bridge/entry observation.
No parent replication premise: cross-node bridges retain opaque parent IDs.
The executing node is deliberately not substituted for the workspace owner. -/
def requestWorkspaceWithinSource (request source : RequestWorkspace)
    (sourceAuthenticated : Bool) : Bool :=
  sourceAuthenticated && requestWorkspaceWellFormed request &&
    requestWorkspaceWellFormed source &&
    request.workspaceId == source.workspaceId &&
    request.ownerNodeDid == source.ownerNodeDid &&
    request.sealHash == source.sealHash &&
    match request.authority, source.authority with
    | none, none => true
    | some requested, some granted => authorityRank requested <= authorityRank granted
    | _, _ => false

theorem unauthenticated_workspace_source_denied (request source : RequestWorkspace) :
    requestWorkspaceWithinSource request source false = false := by
  simp [requestWorkspaceWithinSource]

theorem workspace_source_preserves_owner (request source : RequestWorkspace)
    (h : requestWorkspaceWithinSource request source true = true) :
    request.ownerNodeDid = source.ownerNodeDid := by
  simp only [requestWorkspaceWithinSource, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.1.2

theorem workspace_source_preserves_identity (request source : RequestWorkspace)
    (h : requestWorkspaceWithinSource request source true = true) :
    request.workspaceId = source.workspaceId := by
  simp only [requestWorkspaceWithinSource, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.1.1.2

/-- The source adapter authenticates every edge independently. -/
theorem nested_workspace_owner_preserved (root child grandchild : RequestWorkspace)
    (hc : requestWorkspaceWithinSource child root true = true)
    (hg : requestWorkspaceWithinSource grandchild child true = true) :
    grandchild.ownerNodeDid = root.ownerNodeDid := by
  exact (workspace_source_preserves_owner grandchild child hg).trans
    (workspace_source_preserves_owner child root hc)

end Enrollment
