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
  claimCommit : NativeCommit

def grantedCommit : NativeCommit := ⟨true, true, true, true⟩
def deniedCommit : NativeCommit := ⟨true, false, true, true⟩
def good : Action := .commit true true true
def observed : Action := .observe true true
def grant : Action := .publish "parent-agent" true allGrants

def cases : List Case :=
  [ ⟨"published", true, binding, [good, observed, grant], grantedCommit⟩
  , ⟨"same-agent-published", false, sameAgentBinding, [good, observed, grant], deniedCommit⟩
  , ⟨"same-agent-partial-visible", false, sameAgentBinding,
      [good, observed, .publish "parent-agent" true visiblePartialGrants], deniedCommit⟩
  , ⟨"same-agent-marker-conflict", false, sameAgentBinding, [good, observed, grant],
      { deniedCommit with committed := false }⟩
  , ⟨"invoke-denied", false, binding, [good, observed, grant], deniedCommit⟩
  , ⟨"invoke-revoked-at-commit", true, binding, [good, observed, grant], deniedCommit⟩
  , ⟨"marker-commit-conflict", true, binding, [good, observed, grant], { grantedCommit with committed := false }⟩
  , ⟨"native-commit-failed", true, binding, [.commit true true false, observed, grant], grantedCommit⟩
  , ⟨"missing-starter", true, binding, [.commit false true true, observed, grant], grantedCommit⟩
  , ⟨"missing-creation-receipt", true, binding, [.commit true false true, observed, grant], grantedCommit⟩
  , ⟨"wrong-signer", true, { binding with signer := "user" }, [good, observed, grant], grantedCommit⟩
  , ⟨"wrong-physical-session", true, { binding with signedSession := "other" }, [good, observed, grant], grantedCommit⟩
  , ⟨"wrong-physical-agent", true, { binding with signedAgent := "other" }, [good, observed, grant], grantedCommit⟩
  , ⟨"commit-unconfirmed", true, binding, [good, grant], grantedCommit⟩
  , ⟨"wrong-receipt", true, binding, [good, .observe false true, grant], grantedCommit⟩
  , ⟨"wrong-owner", true, binding, [good, .observe true false, grant], grantedCommit⟩
  , ⟨"partial-grants", true, binding, [good, observed, .publish "parent-agent" true partialGrants], grantedCommit⟩
  , ⟨"visible-before-publication-complete", true, binding, [good, observed, .publish "parent-agent" true visiblePartialGrants], grantedCommit⟩
  , ⟨"partial-then-recover", true, binding, [good, observed, .publish "parent-agent" true partialGrants, grant], grantedCommit⟩
  , ⟨"foreign-recovery-actor", true, binding, [good, observed, .publish "user" true allGrants], grantedCommit⟩
  , ⟨"mismatched-recovery-binding", true, binding, [good, observed, .publish "parent-agent" false allGrants], grantedCommit⟩
  , ⟨"revoked-after-publication", true, binding, [good, observed, grant, .revokeSessionParent, grant], grantedCommit⟩ ]

theorem complete_publication_exposes_child_and_preserves_user_control :
    let s := [good, observed, grant].foldl step (initial true)
    claim s grantedCommit = .claimed ∧
      allowed 5 s.household "child-agent" "delivered_requests" "child-request" "read" = true ∧
      allowed 5 s.household "user" "sessions" "child-session" "update" = true ∧
      allowed 5 s.household "user" "sessions" "child-session" "delete" = true ∧
      allowed 5 s.household "other-user" "sessions" "child-session" "delete" = false := by
  native_decide

theorem denied_invoke_cannot_claim :
    let s := [good, observed, grant].foldl step (initial false)
    claim s deniedCommit = .denied := by native_decide

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
