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
    (delegation : Delegation) (now : Nat) : Bool :=
  authenticates client.did client.runtime now delegation &&
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
    (delegation : Delegation) (now : Nat) (reachable : Bool) : Client :=
  { client with phase :=
      if !reachable then .unreachable
      else if admitted state client delegation now then .connected
      else if client.phase == .connected || client.phase == .revoked then .revoked
      else .requesting }

theorem request_preserves_client_binding (client : Client) :
    (request client).did = client.did ∧
    (request client).runtime = client.runtime ∧
    (request client).queued = client.queued := by simp [request]

theorem observation_preserves_client_binding (state : Identity.Household.State)
    (client : Client) (delegation : Delegation) (now : Nat) (reachable : Bool) :
    (observe state client delegation now reachable).did = client.did ∧
    (observe state client delegation now reachable).runtime = client.runtime ∧
    (observe state client delegation now reachable).queued = client.queued := by
  simp [observe]

theorem requesting_does_not_grant (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) :
    admitted state (request client) delegation now = admitted state client delegation now := rfl

theorem observation_does_not_grant (state : Identity.Household.State) (client : Client)
    (delegation : Delegation) (now : Nat) (reachable : Bool) :
    admitted state (observe state client delegation now reachable) delegation now =
      admitted state client delegation now := rfl

def initial : Identity.Household.State :=
  { objects := [⟨"nodes", "runtime", "owner"⟩,
      ⟨"nodes", "other-runtime", "other-owner"⟩], relations := [], enrolled := [] }

def desktop : Client :=
  { did := "client", runtime := ⟨"runtime", "worker", 1⟩,
    queued := ["chat", "goal"] }

def signed : Delegation := ⟨"client", "worker", 1, 20, true⟩

theorem valid_delegation_is_not_a_grant : admitted initial desktop signed 10 = false := by
  native_decide

theorem client_cannot_approve_itself :
    admitted (approve initial desktop.did desktop) desktop signed 10 = false := by native_decide

theorem approval_uses_existing_node_authority :
    admitted (approve initial "owner" desktop) desktop signed 10 = true := by native_decide

theorem approval_does_not_reach_another_runtime :
    admitted (approve initial "owner" desktop)
      { desktop with runtime := ⟨"other-runtime", "worker", 1⟩ } signed 10 = false := by
  native_decide

theorem revoked_client_can_ask_but_cannot_restore_its_grant :
    let granted := approve initial "owner" desktop
    let connected := observe granted desktop signed 10 true
    let withdrawn := revoke granted "owner" desktop
    let refused := observe withdrawn connected signed 10 true
    refused.phase = .revoked ∧ refused.queued = desktop.queued ∧
      admitted withdrawn (request refused) signed 10 = false ∧
      admitted (approve withdrawn "owner" (request refused)) (request refused) signed 10 = true := by
  native_decide

theorem unreachable_runtime_keeps_queued_intents :
    let pending := observe initial desktop signed 10 false
    pending.phase = .unreachable ∧ pending.queued = desktop.queued ∧
      (request pending).queued = desktop.queued := by native_decide

end ClientRuntimeAccess
