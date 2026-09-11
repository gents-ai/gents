import Proofs.AgentSession
import Proofs.Session.State
import Proofs.Enrollment.State
import Proofs.Workspace.Types

/-! Invocation input is not configuration or admission authority. The existing
path/workspace owner supplies cwd evidence; the authenticated origin supplies
queue-source evidence. Neither is inferred from caller-authored input. -/
namespace Enrollment

structure RequestQueue where
  source : SessionQueue.QueueSource
  policy : SessionQueue.QueuePolicy
  key : Option String := none
  queuedAfterRequestId : Option String := none
  interruptedRequestId : Option String := none
  backgroundCompletionWakeVersion : Option Nat := none
  deriving DecidableEq, Repr

/-- Original signed facts, not the mutable current Goal state. Goal and parent
identities remain on existing request lineage. Positive sequence is validated. -/
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

/-- Structural fields, passed to the existing length-prefixed canonical encoder.
Explicit option tags and collection lengths preserve absent/empty distinctions.
The implementation must use its one canonical input encoder for signing and
verification, rather than concatenating unescaped JSON or arbitrary metadata. -/
private def optionFields {α : Type} (encode : α → List String) : Option α → List String
  | none => ["none"]
  | some value => "some" :: encode value

private def titleFields (title : AgentSession.Title) : List String :=
  [title.text, title.source.toWireName]

private def queueFields (queue : RequestQueue) : List String :=
  [queue.source.toDefraDB, queue.policy.toDefraDB] ++
  optionFields (fun s => [s]) queue.key ++
  optionFields (fun s => [s]) queue.queuedAfterRequestId ++
  optionFields (fun s => [s]) queue.interruptedRequestId ++
  optionFields (fun n => [toString n]) queue.backgroundCompletionWakeVersion

def requestInputFields (input : RequestInput) : CanonicalFields :=
  textFieldsToBytes <|
    [toString input.selectedSkillIds.length] ++ input.selectedSkillIds ++
    optionFields (fun s => [s]) input.cwd ++
    optionFields titleFields input.initialTitle ++ optionFields queueFields input.queue ++
    optionFields (fun goal => [toString goal.sequence, toString goal.wrapup]) input.goalContinuation

/-- Existing context allowlist is the sole skill grant. Invalid activation is
rejected rather than silently authorizing an extra principal-wide skill. -/
def inputWithinContext (input : RequestInput) (skillIds : List String)
    (cwdAllowed : String → Bool) (queueSourceAllowed : SessionQueue.QueueSource → Bool) : Bool :=
  input.selectedSkillIds.all skillIds.contains &&
    (input.cwd.map cwdAllowed).getD true &&
    (input.queue.map (fun q => queueSourceAllowed q.source)).getD true

/-- Behavior selection has one owner and is required even for new sessions. -/
def behaviorMatchesSession (selected observed : String) : Bool :=
  !selected.trim.isEmpty && selected == observed

/-- Request titles are consumed only by creation. Reuse, including a currently
untitled session, must never replay the original title intent. -/
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

/-- The retained signed tuple names a workspace and authority, never a host.
Workspace ownership/grants/seal validity are resolved by the existing workspace
admission owner, not established by these caller-authored fields. -/
structure RequestWorkspace where
  workspaceId : Option String := none
  /-- Exact owner scope retained from the authenticated source, never a host. -/
  ownerAgentDid : Option String := none
  authority : Option BindingAuthority := none
  sealHash : Option String := none
  deriving DecidableEq, Repr

def requestWorkspaceFields (workspace : RequestWorkspace) : CanonicalFields :=
  textFieldsToBytes <|
    optionFields (fun s => [s]) workspace.workspaceId ++
    optionFields (fun s => [s]) workspace.ownerAgentDid ++
    optionFields (fun a => [a.toDefraDB]) workspace.authority ++
    optionFields (fun s => [s]) workspace.sealHash

/-- A scoped workspace reference is complete or absent. This structural check
never grants access; ACP, writer/integrator, state and seal checks remain owned
by the existing admission and workspace adapters. -/
def requestWorkspaceWellFormed (workspace : RequestWorkspace) : Bool :=
  match workspace.workspaceId, workspace.ownerAgentDid, workspace.authority with
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
No parent replication premise: cross-principal bridges retain opaque parent IDs.
The executing principal is deliberately not substituted for the workspace owner. -/
def requestWorkspaceWithinSource (request source : RequestWorkspace)
    (sourceAuthenticated : Bool) : Bool :=
  sourceAuthenticated && requestWorkspaceWellFormed request &&
    requestWorkspaceWellFormed source &&
    request.workspaceId == source.workspaceId &&
    request.ownerAgentDid == source.ownerAgentDid &&
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
    request.ownerAgentDid = source.ownerAgentDid := by
  simp only [requestWorkspaceWithinSource, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.1.2

theorem workspace_source_preserves_identity (request source : RequestWorkspace)
    (h : requestWorkspaceWithinSource request source true = true) :
    request.workspaceId = source.workspaceId := by
  simp only [requestWorkspaceWithinSource, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.1.1.2

/-- Repeated delegation cannot rewrite the owner, even when each executor has
another DID. The source adapter authenticates every edge independently. -/
theorem nested_workspace_owner_preserved (root child grandchild : RequestWorkspace)
    (hc : requestWorkspaceWithinSource child root true = true)
    (hg : requestWorkspaceWithinSource grandchild child true = true) :
    grandchild.ownerAgentDid = root.ownerAgentDid := by
  exact (workspace_source_preserves_owner grandchild child hg).trans
    (workspace_source_preserves_owner child root hc)

end Enrollment
