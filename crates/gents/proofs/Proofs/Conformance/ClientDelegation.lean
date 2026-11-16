import Proofs.Store.Identity.ClientDelegation
import Proofs.Conformance.Family

namespace Conformance.ClientDelegation

open Lean Identity.ClientDelegation

structure Input where
  issuerMatches : Bool
  workerMatches : Bool
  deploymentMatches : Bool
  fresh : Bool
  verified : Bool
  deriving ToJson

@[conformance_family]
def authentication : Family Input Bool where
  inputs := [false, true].flatMap fun issuerMatches =>
    [false, true].flatMap fun workerMatches =>
      [false, true].flatMap fun deploymentMatches =>
        [false, true].flatMap fun fresh =>
          [false, true].map fun verified =>
            { issuerMatches, workerMatches, deploymentMatches, fresh, verified }
  model := fun i => authenticates "client" ⟨"runtime", "worker", 1⟩ 10
    { issuer := if i.issuerMatches then "client" else "another-client"
      worker := if i.workerMatches then "worker" else "another-worker"
      deployment := if i.deploymentMatches then 1 else 2
      expires := if i.fresh then 20 else 10
      verified := i.verified }

structure CredentialInput where
  presented : Bool
  actorMatches : Bool
  ambient : Bool
  deriving ToJson

@[conformance_family]
def requestCredential : Family CredentialInput Bool where
  inputs := [false, true].flatMap fun presented =>
    [false, true].flatMap fun actorMatches =>
      [false, true].map fun ambient => { presented, actorMatches, ambient }
  model := fun i => credentialForActor i.presented i.actorMatches

end Conformance.ClientDelegation
