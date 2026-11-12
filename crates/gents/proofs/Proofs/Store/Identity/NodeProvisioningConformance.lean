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

def agentCreatorCasesJson : String := (toJson (agentCreatorCases.map fun (name, o) =>
  Json.mkObj [("name", toJson name), ("runtime_matches", toJson o.runtimeMatches),
    ("signer_held", toJson o.signerHeld), ("signer_matches", toJson o.signerMatches),
    ("native_create", toJson o.nativeCreate), ("native_relation", toJson o.nativeRelation),
    ("allowed", toJson (agentCreatorMayPublish o))])).compress

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

def bootstrapCaseJson (name : String) (o : BootstrapObservation) : Json := Json.mkObj
  [("name", toJson name), ("exclusive", toJson o.exclusive),
   ("matching_runtime", toJson o.matchingRuntime), ("authenticated_client", toJson o.authenticatedClient),
   ("node_absent_observed", toJson o.nodeAbsentObserved), ("store_empty_observed", toJson o.storeEmptyObserved),
   ("reservation_durable", toJson o.reservationDurable), ("nac_enabled", toJson o.nacEnabled),
   ("fresh_evidence", toJson o.freshEvidence), ("configuration_published", toJson o.configurationPublished),
   ("expected_may_grant", toJson (bootstrapMayGrant o))]

def bootstrapCasesJson : String := (toJson (bootstrapCases.map fun (name, observation) =>
  bootstrapCaseJson name observation)).compress

def runtimeNacCasesJson : String := (toJson ([NacStatus.notConfigured, .enabled,
  .disabledTemporarily].map fun status =>
    Json.mkObj [("status", toJson status.name),
      ("may_start", toJson (runtimeMayStart status))])).compress

end Identity.NodeProvisioningConformance
