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
agent. This includes provider captures, their session-scoped content blocks,
provider reductions, compaction entries and goal observations. The native
parent is the physical session, including for every block in a capture. ACP1
does not enforce this actor equality on arbitrary direct creates. User-authored
Goals and claims and user-created fork copies retain their actual creator as
owner. A provider operation without a physical session is not covered by this
contract. -/
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

/-- An auxiliary audit is signed by the selected Agent even when the ordinary
parent session belongs to a distinct requester. The physical child/session
link is authorized only by the immutable signed auxiliary and exact signed
normal parent. The binding never turns the auxiliary into user authority.
The source and collection are native observations, not caller-supplied labels. -/
structure ChildScopeBinding where
  collection : String := "AgentOutputSegment"
  source : String := "title"
  actor : String := "agent"
  childNode : String := "node"
  childSession : String := "session"
  childSessionDoc : String := "session-doc"
  childRequester : Option String := some "agent"
  childAgent : String := "agent"
  childRequestDoc : String := "aux-doc"
  sessionNode : String := "node"
  sessionId : String := "session"
  sessionDoc : String := "session-doc"
  sessionRequester : Option String := some "user"
  sessionAgent : String := "agent"
  auxiliaryPresent : Bool := true
  auxiliarySignatureValid : Bool := true
  auxiliaryShapeValid : Bool := true
  auxiliaryPurpose : String := "title-audit"
  auxiliaryDoc : String := "aux-doc"
  auxiliaryId : String := "aux-id"
  auxiliaryNode : String := "node"
  auxiliarySession : String := "session"
  auxiliaryRequester : Option String := some "agent"
  auxiliaryAgent : String := "agent"
  sourceParentId : String := "parent-id"
  linkedParentId : String := "parent-id"
  linkedParentDoc : String := "parent-doc"
  parentPresent : Bool := true
  parentSignatureValid : Bool := true
  parentPurpose : String := "normal"
  parentDoc : String := "parent-doc"
  parentId : String := "parent-id"
  parentNode : String := "node"
  parentSession : String := "session"
  parentRequester : Option String := some "user"
  parentAgent : String := "agent"
  deriving Repr, DecidableEq, Lean.ToJson

def auxiliarySourceMatches (collection source purpose : String) : Bool :=
  (collection == "AgentOutputSegment" || collection == "RenderedRequest") &&
    ((source == "title" && purpose == "title-audit") ||
     (source == "access-review" && purpose == "access-review"))

def childScopeBindingAllowed (b : ChildScopeBinding) : Bool :=
  let common := b.sessionDoc != "" && b.sessionNode != "" && b.sessionId != "" &&
    b.sessionAgent != "" && b.childSessionDoc == b.sessionDoc &&
    b.childNode == b.sessionNode && b.childSession == b.sessionId &&
    b.childAgent == b.sessionAgent && b.actor == b.sessionAgent
  let ordinary := b.source == "ordinary" && b.childRequester == b.sessionRequester
  let auxiliary := auxiliarySourceMatches b.collection b.source b.auxiliaryPurpose &&
    b.auxiliaryPresent && b.auxiliarySignatureValid && b.auxiliaryShapeValid &&
    b.auxiliaryDoc != "" && b.auxiliaryId != "" &&
    b.childRequestDoc == b.auxiliaryDoc && b.auxiliaryDoc != b.parentDoc &&
    b.auxiliaryNode == b.sessionNode && b.auxiliarySession == b.sessionId &&
    b.auxiliaryAgent == b.sessionAgent && b.auxiliaryRequester == some b.sessionAgent &&
    b.childRequester == b.auxiliaryRequester &&
    b.parentPresent && b.parentSignatureValid && b.parentPurpose == "normal" &&
    b.parentDoc != "" && b.parentId != "" && b.parentId != b.auxiliaryId &&
    b.linkedParentDoc == b.parentDoc && b.linkedParentId == b.parentId &&
    b.sourceParentId == b.parentId && b.parentNode == b.sessionNode &&
    b.parentSession == b.sessionId && b.parentAgent == b.sessionAgent &&
    b.parentRequester == b.sessionRequester
  common && (ordinary || auxiliary)

/-- Detached repair observes the selected native owner and creator before
reusing the same physical and signed binding. The existing grant and marker
semantics retain the native ACP1 revocation race described by recoverParent. -/
def recoverBoundSessionChild (publication : ParentPublication) (b : ChildScopeBinding)
    (nativeOwner claimedCreator authenticatedCreator : Option DID)
    (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  if publication.committed && !publication.parentLinked &&
      b.collection != "RenderedRequest" && childScopeBindingAllowed b &&
      nativeOwner == some b.sessionAgent &&
      claimedCreator == some b.sessionAgent && authenticatedCreator == some b.sessionAgent then
    recoverSessionChild publication nativeOwner claimedCreator authenticatedCreator
      grantSucceeded markSucceeded
  else publication

theorem foreign_requester_rejected_for_ordinary_child :
    childScopeBindingAllowed { source := "ordinary" } = false := by decide

theorem valid_paired_title_child_is_bound :
    childScopeBindingAllowed {} = true := by decide

theorem invalid_auxiliary_signature_cannot_repair :
    recoverBoundSessionChild beginParentPublication { auxiliarySignatureValid := false }
      (some "agent") (some "agent") (some "agent") true true =
      beginParentPublication := by decide

theorem foreign_native_owner_cannot_repair :
    recoverBoundSessionChild beginParentPublication {}
      (some "other") (some "agent") (some "agent") true true =
      beginParentPublication := by decide

theorem completed_link_is_not_regranted :
    recoverBoundSessionChild ⟨true, false, true⟩ {}
      (some "agent") (some "agent") (some "agent") true true =
      ⟨true, false, true⟩ := by decide

structure ChildScopeCase where
  name : String
  binding : ChildScopeBinding
  nativeOwner : Option String := some "agent"
  creator : Option String := some "agent"
  authenticated : Option String := some "agent"
  grantOk : Bool := true
  markOk : Bool := true
  committed : Bool := true
  parentGranted : Bool := false
  parentLinked : Bool := false
  deriving Repr

def childScopeCases : List ChildScopeCase :=
  [ { name := "paired_title_segment", binding := {} },
    { name := "paired_title_capture", binding := { collection := "RenderedRequest" } },
    { name := "paired_review_segment", binding := { source := "access-review", auxiliaryPurpose := "access-review" } },
    { name := "paired_review_capture", binding := { collection := "RenderedRequest", source := "access-review", auxiliaryPurpose := "access-review" } },
    { name := "absent_session_requester", binding := { sessionRequester := none, parentRequester := none } },
    { name := "ordinary_same_requester", binding := { source := "ordinary", childRequester := some "user" } },
    { name := "ordinary_foreign_requester", binding := { source := "ordinary" } },
    { name := "ordinary_absent_requester", binding := { source := "ordinary", childRequester := none, sessionRequester := none } },
    { name := "source_kind_mismatch", binding := { source := "access-review" } },
    { name := "foreign_collection", binding := { collection := "AgentMessage" } },
    { name := "foreign_actor", binding := { actor := "other" } },
    { name := "foreign_child_node", binding := { childNode := "other" } },
    { name := "foreign_child_session", binding := { childSession := "other" } },
    { name := "foreign_physical_session", binding := { childSessionDoc := "other" } },
    { name := "empty_physical_session", binding := { childSessionDoc := "", sessionDoc := "" } },
    { name := "foreign_child_agent", binding := { childAgent := "other" } },
    { name := "missing_auxiliary", binding := { auxiliaryPresent := false } },
    { name := "invalid_auxiliary_signature", binding := { auxiliarySignatureValid := false } },
    { name := "invalid_auxiliary_shape", binding := { auxiliaryShapeValid := false } },
    { name := "foreign_auxiliary_document", binding := { childRequestDoc := "other" } },
    { name := "foreign_auxiliary_requester", binding := { auxiliaryRequester := some "user" } },
    { name := "foreign_child_requester", binding := { childRequester := some "other" } },
    { name := "foreign_auxiliary_node", binding := { auxiliaryNode := "other" } },
    { name := "foreign_auxiliary_session", binding := { auxiliarySession := "other" } },
    { name := "foreign_auxiliary_agent", binding := { auxiliaryAgent := "other" } },
    { name := "missing_parent", binding := { parentPresent := false } },
    { name := "invalid_parent_signature", binding := { parentSignatureValid := false } },
    { name := "parent_not_normal", binding := { parentPurpose := "title-audit" } },
    { name := "foreign_parent_document", binding := { linkedParentDoc := "other" } },
    { name := "foreign_parent_id", binding := { linkedParentId := "other" } },
    { name := "foreign_runtime_source", binding := { sourceParentId := "other" } },
    { name := "foreign_parent_node", binding := { parentNode := "other" } },
    { name := "foreign_parent_session", binding := { parentSession := "other" } },
    { name := "foreign_parent_agent", binding := { parentAgent := "other" } },
    { name := "foreign_parent_requester", binding := { parentRequester := some "other" } },
    { name := "absent_parent_requester", binding := { parentRequester := none } },
    { name := "missing_native_owner", binding := {}, nativeOwner := none },
    { name := "foreign_native_owner", binding := {}, nativeOwner := some "other" },
    { name := "foreign_creator", binding := {}, nativeOwner := some "agent", creator := some "other" },
    { name := "foreign_authenticated_creator", binding := {}, nativeOwner := some "agent", creator := some "agent", authenticated := some "other" },
    { name := "failed_grant", binding := {}, nativeOwner := some "agent", creator := some "agent", authenticated := some "agent", grantOk := false, markOk := true },
    { name := "failed_marker", binding := {}, nativeOwner := some "agent", creator := some "agent", authenticated := some "agent", grantOk := true, markOk := false },
    { name := "uncommitted", binding := {}, committed := false },
    { name := "completed_then_revoked", binding := {}, parentLinked := true } ]

def childScopeCaseJson (c : ChildScopeCase) : Lean.Json :=
  let initial : ParentPublication := ⟨c.committed, c.parentGranted, c.parentLinked⟩
  let result := recoverBoundSessionChild initial c.binding c.nativeOwner c.creator
    c.authenticated c.grantOk c.markOk
  Lean.Json.mkObj [("name", Lean.toJson c.name), ("binding", Lean.toJson c.binding),
    ("native_owner", Lean.toJson c.nativeOwner), ("creator", Lean.toJson c.creator),
    ("authenticated", Lean.toJson c.authenticated), ("grant_ok", Lean.toJson c.grantOk),
    ("mark_ok", Lean.toJson c.markOk), ("committed", Lean.toJson c.committed),
    ("initial_parent_granted", Lean.toJson c.parentGranted),
    ("initial_parent_linked", Lean.toJson c.parentLinked),
    ("allowed", Lean.toJson (childScopeBindingAllowed c.binding)),
    ("parent_granted", Lean.toJson result.parentGranted),
    ("parent_linked", Lean.toJson result.parentLinked)]

def childScopeCasesJson : String :=
  (Lean.toJson (childScopeCases.map childScopeCaseJson)).compress

/-- Fresh Goal and claim documents, their physical session and working-Agent
relationships, and the linked marker commit as one native transaction. The
same actor owns the document on both user- and agent-authored paths. Retained
incomplete publications repair within one transaction; a completed marker is
a fence against recreating subsequently revoked relationships. -/
structure GoalLinks where
  parent : Bool := false
  worker : Bool := false
  linked : Bool := false
  deriving DecidableEq, Repr

def publishGoalLinks (commit : Bool) : GoalLinks :=
  if commit then ⟨true, true, true⟩ else {}

def goalRepairAllowed (actor nativeOwner : String) (state : GoalLinks) : Bool :=
  actor == nativeOwner && !state.linked

def repairGoalLinks (state : GoalLinks) (parentOk workerOk markOk : Bool) : GoalLinks :=
  if state.linked then state
  else if parentOk && workerOk && markOk then ⟨true, true, true⟩
  else state

theorem fresh_goal_grants_commit_together :
    publishGoalLinks true = ⟨true, true, true⟩ ∧ publishGoalLinks false = {} := by decide

theorem failed_goal_repair_does_not_publish_partial_grants :
    repairGoalLinks {} true false true = {} := by decide

theorem foreign_goal_creator_cannot_repair :
    goalRepairAllowed "user" "agent" {} = false := by decide

theorem completed_goal_does_not_restore_revoked_parent :
    repairGoalLinks ⟨false, true, true⟩ true true true = ⟨false, true, true⟩ := by decide

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

/-- Fresh client request creation, its physical parents, and the selected
working Agent grant share one native transaction. Legacy pending publications
retain the separate creator-checked recovery protocol below. A duplicate does
not regrant a completed publication. -/
def commitFreshClientRequest (state : ClientRequestPublication)
    (fresh committed : Bool) : ClientRequestPublication :=
  if fresh && committed then ⟨true, false, true⟩ else state

theorem client_request_atomic_abort (state : ClientRequestPublication) (fresh : Bool) :
    commitFreshClientRequest state fresh false = state := by cases fresh <;> rfl

theorem client_request_duplicate_preserves_revocation
    (state : ClientRequestPublication) (committed : Bool) :
    commitFreshClientRequest state false committed = state := by rfl

def atomicClientRequestCasesJson : String :=
  let cases := [("aborted", ⟨false, false, false⟩, true, false),
    ("created", ⟨false, false, false⟩, true, true),
    ("duplicate-after-revocation", ⟨true, false, false⟩, false, true)]
  (Lean.toJson (cases.map fun (name, before, fresh, committed) =>
    let result := commitFreshClientRequest before fresh committed
    Lean.Json.mkObj [("name", Lean.toJson name), ("committed", Lean.toJson result.committed),
      ("pending", Lean.toJson result.pending), ("grants", Lean.toJson result.grants)])).compress

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

def goalLinkCaseJson (collection actor mode : String) (commit : Bool) : Lean.Json :=
  let empty : State := { objects := [], relations := [], enrolled := [] }
  let docExists := mode != "fresh" || commit
  let author := if mode == "foreign" then "agent" else actor
  let committed := if docExists then directCreate empty author "children" "goal" true true else empty
  let owner := (committed.objects.find? (fun object => object.id == "goal")).map (·.owner)
  let initial := if mode == "fresh" then publishGoalLinks commit
    else if mode == "partial" then ⟨true, false, false⟩ else {}
  let steps : List GoalLinkStep := if mode == "recovery" || mode == "partial" then
    [⟨true, false, true⟩, ⟨true, true, false⟩, ⟨true, true, true⟩] else []
  let revoked := repairGoalLinks ⟨false, true, true⟩ true true true
  Lean.Json.mkObj [("collection", Lean.toJson collection), ("actor", Lean.toJson actor),
    ("mode", Lean.toJson mode), ("commit", Lean.toJson commit), ("owner", Lean.toJson owner),
    ("initial", goalLinkStateJson initial), ("steps", Lean.toJson (goalLinkTraceJson initial steps)),
    ("after_revocation", goalLinkStateJson revoked),
    ("repair_allowed", Lean.toJson (goalRepairAllowed actor author initial))]

def goalLinkCasesJson : String :=
  (Lean.toJson (["Goal", "GoalCreationClaim"].flatMap fun collection =>
    (["user", "agent"].flatMap fun actor =>
      [goalLinkCaseJson collection actor "fresh" false,
       goalLinkCaseJson collection actor "fresh" true,
       goalLinkCaseJson collection actor "recovery" true,
       goalLinkCaseJson collection actor "partial" true]) ++
    [goalLinkCaseJson collection "user" "foreign" true])).compress

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
