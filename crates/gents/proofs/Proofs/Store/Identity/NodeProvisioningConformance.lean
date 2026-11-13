import Proofs.Store.Identity.NodeProvisioning
import Proofs.Store.Identity.Household
import Lean

namespace Identity.NodeProvisioningConformance
open Lean Identity.NodeProvisioning

def agentCreatorCases : List (String × AgentCreatorObservation) :=
  let valid : AgentCreatorObservation := ⟨true, true, true, true, true⟩
  [("authorized-agent", valid),
   ("foreign-runtime", { valid with runtimeMatches := false }),
   ("missing-signer", { valid with signerHeld := false }),
   ("foreign-signer", { valid with signerMatches := false }),
   ("native-create-denied", { valid with nativeCreate := false }),
   ("native-relation-denied", { valid with nativeRelation := false })]

def freshBootstrap : BootstrapObservation := ⟨true, true, true, true, true, true, true, true, false⟩

def bootstrapCases : List (String × BootstrapObservation) :=
  [("fresh", freshBootstrap),
   ("not-exclusive", { freshBootstrap with exclusive := false }),
   ("foreign-runtime", { freshBootstrap with matchingRuntime := false }),
   ("unauthenticated-client", { freshBootstrap with authenticatedClient := false }),
   ("existing-or-unknown-node", { freshBootstrap with nodeAbsentObserved := false }),
   ("store-not-fresh", { freshBootstrap with storeEmptyObserved := false }),
   ("reservation-not-durable", { freshBootstrap with reservationDurable := false }),
   ("nac-disabled", { freshBootstrap with nacEnabled := false }),
   ("already-published", { freshBootstrap with configurationPublished := true }),
   ("crash-after-reservation", bootstrapAfterCrash freshBootstrap)]

end Identity.NodeProvisioningConformance
