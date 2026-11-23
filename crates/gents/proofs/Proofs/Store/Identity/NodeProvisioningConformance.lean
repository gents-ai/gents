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
   ("foreign-signer", { valid with signerMatches := false })]

def freshBootstrap : BootstrapObservation := ⟨true, true, true, true⟩

def bootstrapCases : List (String × BootstrapObservation) :=
  [("fresh", freshBootstrap),
   ("not-exclusive", { freshBootstrap with exclusive := false }),
   ("store-not-fresh", { freshBootstrap with storeEmptyObserved := false }),
   ("reservation-not-durable", { freshBootstrap with reservationDurable := false }),
   ("crash-after-reservation", bootstrapAfterCrash freshBootstrap)]

end Identity.NodeProvisioningConformance
