import Proofs.Identity.Household
import Lean

namespace Identity.SessionChild
open Household

/-- Conditional atomic-publication contract. The caller supplies the physical
session and authenticated creator, and the storage adapter must commit the
document and its parent together. ACP1 does not establish those premises for
arbitrary direct creates or detached relationship grants. -/
def commitChild (state : State) (childId creator sessionDoc : String) (commit : Bool) : State :=
  if commit then
    { state with
      objects := ⟨"children", childId, creator⟩ :: state.objects
      relations := ⟨"children", childId, "parent", .object "sessions" sessionDoc⟩ :: state.relations }
  else state

theorem aborted_child_is_unchanged (state : State) (child creator sessionDoc : String) :
    commitChild state child creator sessionDoc false = state := by rfl

theorem committed_child_has_physical_parent (state : State) (child creator sessionDoc : String) :
    (commitChild state child creator sessionDoc true).relations =
      ⟨"children", child, "parent", .object "sessions" sessionDoc⟩ :: state.relations := by rfl

/-- Trusted Goal publication conditioned on native observations of the
physical session and selected Agent. Session ownership does not imply current
Agent invoke permission. The native owner is the actor that creates the Goal;
ACP1 direct creation need not pass through this modeled publisher. -/
def commitGoalChild (state : State) (childId actor sessionDoc : String)
    (sessionAllowed agentInvoke commit : Bool) : State :=
  if sessionAllowed && agentInvoke then commitChild state childId actor sessionDoc commit else state

theorem goal_without_agent_invoke_is_unchanged (state : State)
    (child actor sessionDoc : String) (sessionAllowed commit : Bool) :
    commitGoalChild state child actor sessionDoc sessionAllowed false commit = state := by
  simp [commitGoalChild]

theorem permitted_goal_retains_creator (state : State) (child actor sessionDoc : String) :
    (commitGoalChild state child actor sessionDoc true true true).objects =
      ⟨"children", child, actor⟩ :: state.objects := by
  simp [commitGoalChild, commitChild]

theorem goal_without_session_permission_is_unchanged (state : State)
    (child actor sessionDoc : String) (agentInvoke commit : Bool) :
    commitGoalChild state child actor sessionDoc false agentInvoke commit = state := by
  simp [commitGoalChild]

/-- Trusted runtime publication conditioned on authenticating the selected
agent. ACP1 does not enforce this actor equality on arbitrary direct creates.
User-authored Goals and claims retain their actual creator as owner. -/
def commitAgentChild (state : State) (childId actor selectedAgent sessionDoc : String)
    (commit : Bool) : State :=
  if actor == selectedAgent then commitChild state childId actor sessionDoc commit else state

theorem foreign_actor_cannot_create_child (state : State)
    (child actor selectedAgent sessionDoc : String) (commit : Bool)
    (h : actor ≠ selectedAgent) :
    commitAgentChild state child actor selectedAgent sessionDoc commit = state := by
  simp [commitAgentChild, h]

theorem agent_child_is_owned_by_selected_agent (state : State)
    (child selectedAgent sessionDoc : String) :
    (commitAgentChild state child selectedAgent selectedAgent sessionDoc true).objects =
      ⟨"children", child, selectedAgent⟩ :: state.objects := by
  simp [commitAgentChild, commitChild]

theorem agent_child_has_physical_parent (state : State)
    (child selectedAgent sessionDoc : String) :
    (commitAgentChild state child selectedAgent selectedAgent sessionDoc true).relations =
      ⟨"children", child, "parent", .object "sessions" sessionDoc⟩ :: state.relations := by
  simp [commitAgentChild, commitChild]

/-- The child document precedes recovery. Its authenticated native creator
rechecks the pending marker and commits the parent grant and marker together in
a native transaction. A conflicting transaction retries from a fresh snapshot. -/
def recoverSessionChild (publication : ParentPublication)
    (nativeOwner claimedCreator authenticatedCreator : Option DID)
    (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  if publication.committed && !publication.parentLinked && nativeOwner.isSome &&
      nativeOwner == claimedCreator && nativeOwner == authenticatedCreator &&
      grantSucceeded && markSucceeded then
    { publication with parentGranted := true, parentLinked := true }
  else publication

theorem session_child_missing_owner_does_not_link :
    recoverSessionChild beginParentPublication none (some "agent") (some "agent") true true =
      beginParentPublication := by decide

theorem session_child_grant_failure_does_not_mark :
    recoverSessionChild beginParentPublication (some "agent") (some "agent")
      (some "agent") false true = beginParentPublication := by decide

theorem session_child_failed_marker_does_not_grant :
    recoverSessionChild beginParentPublication (some "agent") (some "agent")
      (some "agent") true false = beginParentPublication := by decide

theorem completed_child_cannot_restore_revoked_parent :
    recoverSessionChild ⟨true, false, true⟩ (some "agent") (some "agent")
      (some "agent") true true = ⟨true, false, true⟩ := by decide

structure SessionChildCase where
  name : String
  initial : ParentPublication
  owner : Option DID
  creator : Option DID
  actor : Option DID
  grant : Bool
  mark : Bool

def sessionChildCases : List SessionChildCase :=
  [⟨"complete", beginParentPublication, some "agent", some "agent", some "agent", true, true⟩,
   ⟨"grant_failed", beginParentPublication, some "agent", some "agent", some "agent", false, true⟩,
   ⟨"mark_failed", beginParentPublication, some "agent", some "agent", some "agent", true, false⟩,
   ⟨"wrong_owner", beginParentPublication, some "other", some "agent", some "agent", true, true⟩,
   ⟨"completed_revoked", ⟨true, false, true⟩, some "agent", some "agent", some "agent", true, true⟩]

def sessionChildCaseJson (c : SessionChildCase) : Lean.Json :=
  let result := recoverSessionChild c.initial c.owner c.creator c.actor c.grant c.mark
  Lean.Json.mkObj [("name", Lean.toJson c.name), ("committed", Lean.toJson c.initial.committed),
    ("parent_granted", Lean.toJson c.initial.parentGranted), ("parent_linked", Lean.toJson c.initial.parentLinked),
    ("owner", Lean.toJson c.owner), ("creator", Lean.toJson c.creator), ("actor", Lean.toJson c.actor),
    ("grant", Lean.toJson c.grant), ("mark", Lean.toJson c.mark),
    ("expected_granted", Lean.toJson result.parentGranted), ("expected_linked", Lean.toJson result.parentLinked)]

def sessionChildCasesJson : String :=
  (Lean.toJson (sessionChildCases.map sessionChildCaseJson)).compress

/-- Goal and claim creators grant the physical session parent and selected
working agent after commit. The marker is recorded only after both grants;
the native creator remains owner for both agent- and user-authored documents.
ACP1 cannot fence a detached grant against concurrent later revocation. -/
structure GoalLinks where
  parent : Bool := false
  worker : Bool := false
  linked : Bool := false
  deriving DecidableEq, Repr

def repairGoalLinks (state : GoalLinks) (parentOk workerOk markOk : Bool) : GoalLinks :=
  let parent := state.parent || parentOk
  let worker := state.worker || workerOk
  ⟨parent, worker, state.linked || (parent && worker && markOk)⟩

theorem incomplete_goal_grant_cannot_mark :
    (repairGoalLinks {} true false true).linked = false := by decide

theorem interrupted_goal_grant_can_resume :
    repairGoalLinks (repairGoalLinks {} true false false) false true true =
      ⟨true, true, true⟩ := by decide

/-- Local-self publication is performed by the selected agent itself. The
native actor, signed requester, signer and selected Agent must coincide before
the shared session and request publication owner may be used. -/
def localSelfPublicationAllowed (actor requester signer selectedAgent : String) : Bool :=
  actor == signer && signer == requester && requester == selectedAgent

theorem foreign_local_self_publisher_rejected :
    localSelfPublicationAllowed "foreign" "agent" "agent" "agent" = false := by decide

def localSelfPublicationCases : List (String × String × String × String) :=
  [("agent", "agent", "agent", "agent"),
   ("foreign", "agent", "agent", "agent"),
   ("agent", "foreign", "agent", "agent"),
   ("agent", "agent", "foreign", "agent"),
   ("agent", "agent", "agent", "foreign")]

def localSelfPublicationCasesJson : String :=
  (Lean.toJson (localSelfPublicationCases.map fun (actor, requester, signer, agent) =>
    Lean.Json.mkObj [("actor", Lean.toJson actor), ("requester", Lean.toJson requester),
      ("signer", Lean.toJson signer), ("agent", Lean.toJson agent),
      ("allowed", Lean.toJson (localSelfPublicationAllowed actor requester signer agent))])).compress

/-- An exact signed client request can be visible to its creator before its
detached ACP1 grants complete. Its durable pending marker authorizes a retry
only by the authenticated signer retaining that request. A cleared marker is
not proof against a later revocation; an in-flight retry can race revocation. -/
structure ClientRequestPublication where
  committed : Bool
  pending : Bool
  grants : Bool
  deriving Repr, DecidableEq

def recoverClientRequest (state : ClientRequestPublication)
    (matchingSignature sameSigner grantsOk markOk : Bool) : ClientRequestPublication :=
  if state.committed && state.pending && matchingSignature && sameSigner then
    let granted := state.grants || grantsOk
    ⟨true, !(granted && markOk), granted⟩
  else state

theorem visible_client_request_does_not_prove_publication :
    (⟨true, true, false⟩ : ClientRequestPublication).committed = true ∧
      (⟨true, true, false⟩ : ClientRequestPublication).pending = true := by decide

theorem wrong_signer_cannot_recover_client_request :
    recoverClientRequest ⟨true, true, false⟩ true false true true = ⟨true, true, false⟩ := by decide

theorem failed_client_grants_remain_pending :
    recoverClientRequest ⟨true, true, false⟩ true true false true = ⟨true, true, false⟩ := by decide

theorem completed_client_request_is_not_regranted :
    recoverClientRequest ⟨true, false, false⟩ true true true true = ⟨true, false, false⟩ := by decide

structure ClientRequestCase where
  name : String
  initial : ClientRequestPublication
  matchingSignature : Bool
  sameSigner : Bool
  grantsOk : Bool
  markOk : Bool

def clientRequestCases : List ClientRequestCase :=
  [⟨"grant_failed", ⟨true, true, false⟩, true, true, false, true⟩,
   ⟨"grant_succeeded", ⟨true, true, false⟩, true, true, true, true⟩,
   ⟨"wrong_signer", ⟨true, true, false⟩, true, false, true, true⟩,
   ⟨"wrong_signature", ⟨true, true, false⟩, false, true, true, true⟩,
   ⟨"already_completed_revoked", ⟨true, false, false⟩, true, true, true, true⟩]

def clientRequestCaseJson (c : ClientRequestCase) : Lean.Json :=
  let result := recoverClientRequest c.initial c.matchingSignature c.sameSigner c.grantsOk c.markOk
  Lean.Json.mkObj [("name", Lean.toJson c.name), ("committed", Lean.toJson c.initial.committed),
    ("pending", Lean.toJson c.initial.pending), ("matching_signature", Lean.toJson c.matchingSignature),
    ("same_signer", Lean.toJson c.sameSigner), ("grants_ok", Lean.toJson c.grantsOk),
    ("mark_ok", Lean.toJson c.markOk), ("expected_pending", Lean.toJson result.pending),
    ("expected_grants", Lean.toJson result.grants)]

def clientRequestPublicationCasesJson : String :=
  (Lean.toJson (clientRequestCases.map clientRequestCaseJson)).compress

structure GoalLinkStep where
  parentOk : Bool
  workerOk : Bool
  markOk : Bool
  deriving Repr

def goalLinkStateJson (state : GoalLinks) : Lean.Json := Lean.Json.mkObj
  [("parent", Lean.toJson state.parent), ("worker", Lean.toJson state.worker),
   ("linked", Lean.toJson state.linked)]

def goalLinkTraceJson : GoalLinks → List GoalLinkStep → List Lean.Json
  | _, [] => []
  | state, step :: rest =>
    let next := repairGoalLinks state step.parentOk step.workerOk step.markOk
    Lean.Json.mkObj [("parent_ok", Lean.toJson step.parentOk),
      ("worker_ok", Lean.toJson step.workerOk), ("mark_ok", Lean.toJson step.markOk),
      ("after", goalLinkStateJson next)] :: goalLinkTraceJson next rest

def goalLinkCaseJson (collection actor : String) (commit : Bool) : Lean.Json :=
  let empty : State := { objects := [], relations := [], enrolled := [] }
  let committed := if commit then directCreate empty actor "children" "goal" true true else empty
  let owner := (committed.objects.find? (fun object => object.id == "goal")).map (·.owner)
  let steps : List GoalLinkStep := if commit then [⟨true, false, false⟩, ⟨false, true, true⟩] else []
  Lean.Json.mkObj [("collection", Lean.toJson collection), ("actor", Lean.toJson actor),
    ("commit", Lean.toJson commit), ("owner", Lean.toJson owner),
    ("initial", goalLinkStateJson {}), ("steps", Lean.toJson (goalLinkTraceJson {} steps))]

def goalLinkCasesJson : String :=
  (Lean.toJson (["Goal", "GoalCreationClaim"].flatMap fun collection =>
    ["user", "agent"].flatMap fun actor =>
      [goalLinkCaseJson collection actor false, goalLinkCaseJson collection actor true])).compress

/-- Capture payload deduplication is limited to a physical session. The
content digest remains the manifest integrity input; the storage identity also
includes the session to prevent cross-session key collisions and reuse. This
key does not itself authorize reads or hide data from other node writers. -/
def blockStorageIdentity (sessionDoc contentDigest : String) : String × String :=
  (sessionDoc, contentDigest)

theorem block_identity_is_session_scoped
    (firstSession secondSession firstDigest secondDigest : String)
    (different : firstSession ≠ secondSession) :
    blockStorageIdentity firstSession firstDigest ≠
      blockStorageIdentity secondSession secondDigest := by
  intro equal
  apply different
  exact congrArg Prod.fst equal

structure BlockScopeCase where
  firstSession : String
  secondSession : String
  firstDigest : String
  secondDigest : String
  deriving Repr

def blockScopeCases : List BlockScopeCase :=
  [ ⟨"session-a", "session-a", "digest-a", "digest-a"⟩,
    ⟨"session-a", "session-b", "digest-a", "digest-a"⟩,
    ⟨"session-a", "session-a", "digest-a", "digest-b"⟩,
    ⟨"a:b", "a", "c", "b:c"⟩ ]

def blockScopeCaseJson (c : BlockScopeCase) : Lean.Json := Lean.Json.mkObj
  [("first_session", Lean.toJson c.firstSession),
   ("second_session", Lean.toJson c.secondSession),
   ("first_digest", Lean.toJson c.firstDigest),
   ("second_digest", Lean.toJson c.secondDigest),
   ("same_storage_identity", Lean.toJson
     (blockStorageIdentity c.firstSession c.firstDigest ==
       blockStorageIdentity c.secondSession c.secondDigest))]

def blockScopeCasesJson : String :=
  (Lean.toJson (blockScopeCases.map blockScopeCaseJson)).compress

end Identity.SessionChild
