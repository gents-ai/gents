import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure BackendDestinationInput where
  name : String
  profileRequired : Bool
  profileVisible : Bool
  backendVisible : Bool
  runtimeActor : Bool
  secretReadable : Bool
  sameNode : Bool
  selectedBackend : Bool
  sameCredential : Bool
  destinationApproved : Bool
  credentialVersionMatches : Bool
  currentSecret : Bool
  historicalSecret : Bool
  deriving ToJson

/-- The client-signed backend scenario moves an authorized backend to an
endpoint its credential was not approved for. -/
@[conformance_family]
def backendDestination : Family BackendDestinationInput Bool where
  inputs := [⟨"unapproved-destination", true, true, true, true, true, true, true, true, false, true,
    false, false⟩]
  model := fun i => Configuration.runtimeMayUseBackendCredential
    ⟨i.profileRequired, i.profileVisible, i.backendVisible, i.runtimeActor, i.secretReadable,
      i.sameNode, i.selectedBackend, i.sameCredential, i.destinationApproved, i.credentialVersionMatches⟩
    i.currentSecret i.historicalSecret

structure BackendDiscoveryInput where
  authorized : Bool
  sameDoc : Bool
  sameConnection : Bool
  committed : Bool
  deriving ToJson

@[conformance_family]
def backendDiscovery : Family BackendDiscoveryInput Bool where
  inputs := [(true, true, true, true), (false, true, true, true), (true, false, true, true),
    (true, true, false, true), (true, true, true, false)].map
    fun (authorized, sameDoc, sameConnection, committed) => { authorized, sameDoc, sameConnection, committed }
  model := fun i => Configuration.backendDiscoveryMayPublish i.authorized i.sameDoc i.sameConnection i.committed

structure BackendProbeInput where
  sameDoc : Bool
  sameConnection : Bool
  committed : Bool
  deriving ToJson

@[conformance_family]
def backendProbe : Family BackendProbeInput Bool where
  inputs := [(true, true, true), (false, true, true), (true, false, true), (true, true, false)].map
    fun (sameDoc, sameConnection, committed) => { sameDoc, sameConnection, committed }
  model := fun i => Configuration.backendProbeMayPublish i.sameDoc i.sameConnection i.committed

structure BackendRemovalInput where
  authorized : Bool
  matching : Bool
  committed : Bool
  deriving ToJson

structure BackendRecords where
  metadata : Bool
  observation : Bool
  credential : Bool
  deriving ToJson

@[conformance_family]
def backendRemoval : Family BackendRemovalInput BackendRecords where
  inputs := [(true, true, true), (false, true, true), (true, false, true), (true, true, false)].map
    fun (authorized, matching, committed) => { authorized, matching, committed }
  model := fun i =>
    let (metadata, observation, credential) :=
      Configuration.removeBackendRecords i.authorized i.matching i.committed
    { metadata, observation, credential }

structure BackendGenesisInput where
  logical : String
  credential : String
  firstNonce : String
  secondNonce : String
  deriving ToJson

/-- Recreating a removed backend with the same logical identity and
credential draws a fresh creation nonce, so it is a distinct native document. -/
@[conformance_family]
def backendGenesis : Family BackendGenesisInput Bool where
  inputs := [⟨"backend", "credential", "nonce-a", "nonce-b"⟩]
  model := fun i => Configuration.backendGenesis i.logical i.credential i.firstNonce !=
    Configuration.backendGenesis i.logical i.credential i.secondNonce

end Conformance.Household
