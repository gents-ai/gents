import Lean

namespace Identity.ClientDelegation

structure Runtime where
  node : String
  worker : String
  deployment : Nat
  deriving BEq, DecidableEq, Repr

/-- DefraDB verifies the client's signature and the Vera delegation's worker,
deployment and expiry. This records that external authentication premise;
Gents neither signs as the client nor treats a delegation as an ACP grant. -/
structure Delegation where
  issuer : String
  worker : String
  deployment : Nat
  expires : Nat
  verified : Bool
  deriving BEq, DecidableEq, Repr

def authenticates (client : String) (runtime : Runtime) (now : Nat)
    (delegation : Delegation) : Bool :=
  delegation.verified && delegation.issuer == client &&
    delegation.worker == runtime.worker && delegation.deployment == runtime.deployment &&
    now < delegation.expires

/-- A request carries only its own caller's presented credential. Native
RequestContext's explicit empty bearer suppresses the process fallback. -/
def credentialForActor (presented actorMatches : Bool) : Bool :=
  presented && actorMatches

/-- A remote ACP provider needs the caller's live delegation before a
client-owned graph can be staged. Local ACP has no remote credential premise.
This is transport readiness; the native provider still decides authorization. -/
def clientGraphReady (remote presented actorMatches : Bool) : Bool :=
  !remote || credentialForActor presented actorMatches

theorem remote_graph_waits_without_credential (actorMatches : Bool) :
    clientGraphReady true false actorMatches = false := by
  simp [clientGraphReady, credentialForActor]

theorem no_presented_credential_never_borrows (actorMatches : Bool) :
    credentialForActor false actorMatches = false := by simp [credentialForActor]

theorem another_actor_never_borrows (presented : Bool) :
    credentialForActor presented false = false := by simp [credentialForActor]

theorem expired_delegation_is_refused (client : String) (runtime : Runtime)
    (delegation : Delegation) (now : Nat) (expired : delegation.expires ≤ now) :
    authenticates client runtime now delegation = false := by
  simp [authenticates, Nat.not_lt_of_ge expired]

theorem another_worker_is_refused (client : String) (runtime : Runtime)
    (delegation : Delegation) (now : Nat) (different : delegation.worker ≠ runtime.worker) :
    authenticates client runtime now delegation = false := by
  simp [authenticates, different]

theorem another_deployment_is_refused (client : String) (runtime : Runtime)
    (delegation : Delegation) (now : Nat)
    (different : delegation.deployment ≠ runtime.deployment) :
    authenticates client runtime now delegation = false := by
  simp [authenticates, different]

end Identity.ClientDelegation
