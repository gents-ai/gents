import Proofs.AgentSession
import Proofs.Session.State
import Proofs.Enrollment.State
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

structure RequestInput where
  selectedSkillIds : List String := []
  cwd : Option String := none
  initialTitle : Option AgentSession.Title := none
  queue : Option RequestQueue := none
  goalContinuation : Option GoalContinuationInput := none
  deriving DecidableEq, Repr

/-- Runtime continuations remain in their parent's invocation directory. They
do not replay the parent's title intent, queue position, or goal issuance facts.
The executing host still validates the inherited directory against its policy. -/
def continuationInput (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput := none) : RequestInput :=
  { cwd := parent.cwd, queue := some queue, goalContinuation := goal }

theorem continuation_preserves_directory (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) :
    (continuationInput parent queue goal).cwd = parent.cwd := rfl

theorem continuation_does_not_replay_creation (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) :
    (continuationInput parent queue goal).initialTitle = none := rfl

theorem continuation_uses_current_control (parent : RequestInput) (queue : RequestQueue)
    (goal : Option GoalContinuationInput) :
    (continuationInput parent queue goal).queue = some queue ∧
    (continuationInput parent queue goal).goalContinuation = goal := ⟨rfl, rfl⟩

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

def requestInputFields (input : RequestInput) : CanonicalFields :=
  textFieldsToBytes <|
    [toString input.selectedSkillIds.length] ++ input.selectedSkillIds ++
    optionFields (fun s => [s]) input.cwd ++
    optionFields titleFields input.initialTitle ++ optionFields queueFields input.queue ++
    optionFields (fun goal => [toString goal.sequence, toString goal.wrapup]) input.goalContinuation

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
    (input.queue.map (fun q => queueSourceAllowed q.source)).getD true

theorem continuation_retains_host_policy_checks (parent : RequestInput)
    (queue : RequestQueue) (goal : Option GoalContinuationInput) (skills : List String)
    (cwdAllowed : String → Bool) (queueAllowed : SessionQueue.QueueSource → Bool) :
    inputWithinContext (continuationInput parent queue goal) skills cwdAllowed queueAllowed =
      ((parent.cwd.map cwdAllowed).getD true && queueAllowed queue.source) := by
  simp [continuationInput, inputWithinContext]

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
  simpa using h.1.1

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
