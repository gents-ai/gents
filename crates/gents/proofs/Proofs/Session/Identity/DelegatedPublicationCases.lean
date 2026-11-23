import Proofs.Session.Identity.DelegatedPublication

namespace Identity.DelegatedPublication.Cases

open Identity.Household Request.CrossActorAuthorization Lean

def binding : Binding :=
  ⟨"user", "parent-agent", "child-agent", "child-session", "child-config",
    "child-request", "child-session", "child-config", "parent-agent"⟩

def sameAgentBinding : Binding := { binding with targetAgent := "parent-agent" }

def initial (invokeGrant : Bool) (b : Binding := binding) : Publication :=
  let base : Identity.Household.State :=
    { objects := [⟨"nodes", "node", b.nodeOwner⟩, ⟨"agents", b.agentDoc, b.nodeOwner⟩],
      relations := [⟨"agents", b.agentDoc, "parent", .object "nodes" "node"⟩], enrolled := [] }
  let state := if invokeGrant then Identity.Household.apply base
    (.grantAs b.nodeOwner ⟨"agents", b.agentDoc, "session_writer", .actor b.parentAgent⟩) else base
  { household := state, binding := b }

def allGrants : GrantAttempt := ⟨true, true, true, true, true, true, true⟩
def partialGrants : GrantAttempt := ⟨true, false, true, false, false, true, true⟩
def visiblePartialGrants : GrantAttempt := ⟨true, true, true, true, false, true, true⟩

inductive Action where
  | commit (starter creationReceipt nativeCommit : Bool)
  | observe (exact owner : Bool)
  | publish (actor : String) (exact : Bool) (g : GrantAttempt)
  | revokeSessionParent
  deriving Repr

def step (s : Publication) : Action → Publication
  | .commit starter receipt native => DelegatedPublication.commit s starter receipt native
  | .observe exact owner => DelegatedPublication.observe s exact owner
  | .publish actor exact grants => DelegatedPublication.publish s actor exact grants
  | .revokeSessionParent =>
      let relation : Relation := ⟨"sessions", s.binding.sessionDoc, "parent", .object "agents" s.binding.agentDoc⟩
      let household := Identity.Household.apply s.household (.revokeAs s.binding.parentAgent relation)
      { s with household := household }

structure Case where
  name : String
  invokeGrant : Bool
  binding : Binding
  actions : List Action
  /-- The writes that land on the target's claim attempts, one per attempt. -/
  claimRaces : List ClaimRace := []

def grantedCommit : NativeCommit := ⟨true, true, true, true⟩
def deniedCommit : NativeCommit := ⟨true, false, true, true⟩
def good : Action := .commit true true true
def observed : Action := .observe true true
def grant : Action := .publish "parent-agent" true allGrants

def cases : List Case :=
  [ ⟨"published", true, binding, [good, observed, grant], []⟩
  , ⟨"same-agent-published", false, sameAgentBinding, [good, observed, grant], []⟩
  , ⟨"same-agent-partial-visible", false, sameAgentBinding,
      [good, observed, .publish "parent-agent" true visiblePartialGrants], []⟩
  , ⟨"same-agent-marker-conflict", false, sameAgentBinding, [good, observed, grant],
      [.rewriteMarkers]⟩
  , ⟨"invoke-denied", false, binding, [good, observed, grant], []⟩
  , ⟨"invoke-revoked-at-commit", true, binding, [good, observed, grant], [.revokeInvoke]⟩
  , ⟨"marker-commit-conflict", true, binding, [good, observed, grant], [.rewriteMarkers]⟩
  , ⟨"marker-reset-at-commit", true, binding, [good, observed, grant], [.resetRequestMarker]⟩
  , ⟨"native-commit-failed", true, binding, [.commit true true false, observed, grant], []⟩
  , ⟨"missing-starter", true, binding, [.commit false true true, observed, grant], []⟩
  , ⟨"missing-creation-receipt", true, binding, [.commit true false true, observed, grant], []⟩
  , ⟨"wrong-signer", true, { binding with signer := "user" }, [good, observed, grant], []⟩
  , ⟨"wrong-physical-session", true, { binding with signedSession := "other" }, [good, observed, grant], []⟩
  , ⟨"wrong-physical-agent", true, { binding with signedAgent := "other" }, [good, observed, grant], []⟩
  , ⟨"commit-unconfirmed", true, binding, [good, grant], []⟩
  , ⟨"wrong-receipt", true, binding, [good, .observe false true, grant], []⟩
  , ⟨"wrong-owner", true, binding, [good, .observe true false, grant], []⟩
  , ⟨"partial-grants", true, binding, [good, observed, .publish "parent-agent" true partialGrants], []⟩
  , ⟨"visible-before-publication-complete", true, binding, [good, observed, .publish "parent-agent" true visiblePartialGrants], []⟩
  , ⟨"partial-then-recover", true, binding, [good, observed, .publish "parent-agent" true partialGrants, grant], []⟩
  , ⟨"foreign-recovery-actor", true, binding, [good, observed, .publish "user" true allGrants], []⟩
  , ⟨"mismatched-recovery-binding", true, binding, [good, observed, .publish "parent-agent" false allGrants], []⟩
  , ⟨"revoked-after-publication", true, binding, [good, observed, grant, .revokeSessionParent, grant], []⟩ ]

/-- The User's revocation lands on the target's first claim attempt, which
conflicts; the rerun reads the revoked invoke and denies. -/
theorem invoke_revoked_at_commit_is_denied :
    claimRetrying ([good, observed, grant].foldl step (initial true)) [.revokeInvoke] = .denied := by
  native_decide

/-- A marker reset between the claim's check and its commit conflicts the
attempt; the rerun reads the pending marker and leaves the work unavailable. -/
theorem marker_reset_at_commit_is_unavailable :
    claimRetrying ([good, observed, grant].foldl step (initial true)) [.resetRequestMarker] =
      .unavailable := by
  native_decide

/-- Rewriting the markers to the value they hold conflicts the attempt and
changes nothing the rerun reads, so the rerun claims, for a cross-actor and a
same-Agent request alike. -/
theorem rewritten_markers_rerun_claims :
    claimRetrying ([good, observed, grant].foldl step (initial true)) [.rewriteMarkers] = .claimed ∧
    claimRetrying ([good, observed, grant].foldl step (initial false sameAgentBinding))
      [.rewriteMarkers] = .claimed := by
  native_decide

theorem complete_publication_exposes_child_and_preserves_user_control :
    let s := [good, observed, grant].foldl step (initial true)
    claim s grantedCommit = .claimed ∧
      allowed 5 s.household "child-agent" "delivered_requests" "child-request" "read" = true ∧
      allowed 5 s.household "user" "sessions" "child-session" "update" = true ∧
      allowed 5 s.household "user" "sessions" "child-session" "delete" = true ∧
      allowed 5 s.household "other-user" "sessions" "child-session" "delete" = false := by
  native_decide

theorem denied_invoke_cannot_commit :
    let s := [good, observed, grant].foldl step (initial false)
    s.committed = false ∧ s.toolRunning = false ∧ claim s deniedCommit = .unavailable := by
  native_decide

/-- The parent Agent is the native owner of the session it published and holds
every permission on it, so document ACP lets it delete the session's `parent`
relation: the User loses the session through its Agent, and a later recovery
does not restore the cleared relation. The child's request keeps its own
relations, so the claim still succeeds. The tool surface gives a managed Agent
no operation that uses this permission to detach or delete a published session
(`Tools.SessionCustody.relationChangeOffered`). -/
theorem revoked_after_publication_detaches_the_session :
    let s := [good, observed, grant, .revokeSessionParent, grant].foldl step (initial true)
    allowed 5 s.household "user" "sessions" "child-session" "read" = false ∧
      claim s grantedCommit = .claimed := by
  native_decide

theorem same_agent_uses_client_admission_but_waits_for_links :
    let complete := [good, observed, grant].foldl step (initial false sameAgentBinding)
    let incomplete := [good, observed, .publish "parent-agent" true visiblePartialGrants].foldl step (initial false sameAgentBinding)
    claim complete deniedCommit = .claimed ∧
      allowed 5 incomplete.household "parent-agent" "delivered_requests" "child-request" "read" = true ∧
      claim incomplete deniedCommit = .unavailable := by native_decide

theorem unknown_commit_cannot_publish_or_claim :
    let s := [good, grant].foldl step (initial true)
    s.committed = true ∧ s.observed = false ∧ s.requestPending = true ∧
      claim s grantedCommit = .unavailable := by native_decide

theorem visibility_does_not_complete_publication :
    let s := [good, observed, .publish "parent-agent" true visiblePartialGrants].foldl step (initial true)
    allowed 5 s.household "child-agent" "delivered_requests" "child-request" "read" = true ∧
      s.requestPending = true ∧ claim s grantedCommit = .unavailable := by native_decide

theorem missing_starter_aborts_atomic_publication :
    let s := [.commit false true true].foldl step (initial true)
    s.committed = false ∧ s.receipt = false ∧ s.toolRunning = false := by native_decide

/-- The caller is dropped at the committed acknowledgement. The native transaction
has committed the opening request, its session and the verified starter, but
the caller recorded no receipt: the publication is unobserved and both pending
markers remain. -/
def droppedAckState : Publication := [good].foldl step (initial true)

/-- The creator's actor watcher recovers the exact receipt, then publishes every
relation and clears both pending markers before any claim. -/
def watcherRecoveredState : Publication := [good, observed, grant].foldl step (initial true)

theorem cancelled_after_child_commit_recovers_relationships_in_the_actor_watcher :
    droppedAckState.committed = true ∧ droppedAckState.toolRunning = true ∧
    allowed 5 droppedAckState.household "child-agent" "delivered_requests" "child-request" "read" = false ∧
    watcherRecoveredState.requestPending = false ∧
    allowed 5 watcherRecoveredState.household "child-agent" "delivered_requests" "child-request" "read" = true ∧
    claim watcherRecoveredState grantedCommit = .claimed := by native_decide

end Identity.DelegatedPublication.Cases
