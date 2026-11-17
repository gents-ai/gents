import Proofs.Store.Identity.Household
import Proofs.Store.Identity.ClientDelegation

namespace ClientRuntimeAccess

open Identity.Household Identity.ClientDelegation

inductive Phase where
  | disconnected | requesting | connected | unreachable | revoked
  deriving BEq, DecidableEq, Repr

structure Client where
  did : String
  runtime : Runtime
  phase : Phase := .disconnected
  queued : List String := []
  deriving BEq, DecidableEq, Repr

/-- Runtime access is the Household Node relationship. There are no client
peer, enrollment, replicated route or local-journal grants in this decision. -/
def admitted (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) (nativeRead : Bool) : Bool :=
  nativeRead && authenticates client.did client.runtime now delegation &&
    allowed 5 state client.did "nodes" client.runtime.node "read"

def administrator (client : Client) : Relation :=
  ⟨"nodes", client.runtime.node, "administrator", .actor client.did⟩

/-- The runtime applies the authenticated approver's ordinary ACP operation.
The client request itself cannot write this relation or borrow the runtime key. -/
def approve (state : Identity.Household.State) (approver : String) (client : Client) :
    Identity.Household.State :=
  apply state (.grantAs approver (administrator client))

def revoke (state : Identity.Household.State) (approver : String) (client : Client) :
    Identity.Household.State :=
  apply state (.revokeAs approver (administrator client))

/-- Asking again is local intent, not an access grant. Retained submissions
keep their identity and order through denial, revocation and a new request. -/
def request (client : Client) : Client := { client with phase := .requesting }

/-- An authenticated response is an observation. Every subsequent runtime
operation still checks current ACP; a connected badge is never authority. -/
def observe (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) (nativeRead reachable : Bool) : Client :=
  { client with phase :=
      if !reachable then .unreachable
      else if admitted state client delegation now nativeRead then .connected
      else if client.phase == .connected || client.phase == .revoked then .revoked
      else .requesting }

theorem request_preserves_client_binding (client : Client) :
    (request client).did = client.did ∧
    (request client).runtime = client.runtime ∧
    (request client).queued = client.queued := by simp [request]

theorem observation_preserves_client_binding (state : Identity.Household.State)
    (client : Client) (delegation : Delegation) (now : Nat) (nativeRead reachable : Bool) :
    (observe state client delegation now nativeRead reachable).did = client.did ∧
    (observe state client delegation now nativeRead reachable).runtime = client.runtime ∧
    (observe state client delegation now nativeRead reachable).queued = client.queued := by
  simp [observe]

theorem requesting_does_not_grant (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) (nativeRead : Bool) :
    admitted state (request client) delegation now nativeRead = admitted state client delegation now nativeRead := rfl

theorem observation_does_not_grant (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) (nativeRead reachable : Bool) :
    admitted state (observe state client delegation now nativeRead reachable) delegation now nativeRead =
      admitted state client delegation now nativeRead := rfl

def initial : Identity.Household.State :=
  { objects := [⟨"nodes", "runtime", "owner"⟩,
      ⟨"nodes", "other-runtime", "other-owner"⟩], relations := [], enrolled := [] }

def desktop : Client :=
  { did := "client", runtime := ⟨"runtime", "worker", 1⟩,
    queued := ["chat", "goal"] }

def signed : Delegation := ⟨"client", "worker", 1, 20, true⟩

theorem valid_delegation_is_not_a_grant : admitted initial desktop signed 10 true = false := by
  native_decide

theorem client_cannot_approve_itself :
    admitted (approve initial desktop.did desktop) desktop signed 10 true = false := by native_decide

theorem approval_uses_existing_node_authority :
    admitted (approve initial "owner" desktop) desktop signed 10 true = true := by native_decide

theorem approval_does_not_reach_another_runtime :
    admitted (approve initial "owner" desktop)
      { desktop with runtime := ⟨"other-runtime", "worker", 1⟩ } signed 10 true = false := by
  native_decide

theorem revoked_client_can_ask_but_cannot_restore_its_grant :
    let granted := approve initial "owner" desktop
    let connected := observe granted desktop signed 10 true true
    let withdrawn := revoke granted "owner" desktop
    let refused := observe withdrawn connected signed 10 true true
    refused.phase = .revoked ∧ refused.queued = desktop.queued ∧
      admitted withdrawn (request refused) signed 10 true = false ∧
      admitted (approve withdrawn "owner" (request refused)) (request refused) signed 10 true = true := by
  native_decide

theorem unreachable_runtime_keeps_queued_intents :
    let pending := observe initial desktop signed 10 true false
    pending.phase = .unreachable ∧ pending.queued = desktop.queued ∧
      (request pending).queued = desktop.queued := by native_decide

inductive AccessRequestState where
  | pending | approved | rejected | revoked
  deriving BEq, DecidableEq, Repr

inductive AccessDecision where
  | approve | reject | revoke
  deriving BEq, DecidableEq, Repr

inductive RequestAdmission where
  | accepted | refused | expired
  deriving BEq, DecidableEq, Repr

/-- A signed request authenticates its exact caller, runtime and immutable
intent. A retained request remains observable after its signing window ends.
Only a verified, unretained request whose valid window has expired permits a
new signed request; neither that response nor renewal grants access. -/
def admitRequest (callerMatches nodeMatches signatureVerified retainedExists retainedSame fresh expired : Bool) : RequestAdmission :=
  if !callerMatches || !nodeMatches || !signatureVerified then .refused
  else if retainedExists then if retainedSame then .accepted else .refused
  else if fresh then .accepted
  else if expired then .expired
  else .refused

theorem expired_unretained_request_can_be_renewed :
    admitRequest true true true false false false true = .expired := rfl

theorem retained_request_is_observed_after_expiry :
    admitRequest true true true true true false true = .accepted := rfl

theorem another_caller_cannot_renew (node signature retained same fresh expired : Bool) :
    admitRequest false node signature retained same fresh expired = .refused := by
  simp [admitRequest]

theorem changed_retained_intent_cannot_renew (fresh expired : Bool) :
    admitRequest true true true true false fresh expired = .refused := rfl

/-- The native grant owner records provenance before granting OperatorUser
and Node administration. A decision reports success only after those owners
complete; no request status itself grants authority. Rejected or revoked
identities require a new request, never a replay of the old request. -/
def settleRequest (before : AccessRequestState) (decision : AccessDecision)
    (nativeAuthority effectsComplete : Bool) : AccessRequestState :=
  if !nativeAuthority || !effectsComplete then before
  else match before, decision with
    | .pending, .approve => .approved
    | .pending, .reject => .rejected
    | .pending, .revoke => .revoked
    | .approved, .revoke => .revoked
    | state, _ => state

def requestStateToken : AccessRequestState → String
  | .pending => "pending" | .approved => "approved"
  | .rejected => "rejected" | .revoked => "revoked"

theorem replay_cannot_restore_revoked_access (decision : AccessDecision)
    (authority completed : Bool) :
    settleRequest .revoked decision authority completed = .revoked := by
  cases decision <;> simp [settleRequest]

theorem failed_effects_never_report_approval (state : AccessRequestState)
    (decision : AccessDecision) (authority : Bool) :
    settleRequest state decision authority false = state := by simp [settleRequest]

end ClientRuntimeAccess
