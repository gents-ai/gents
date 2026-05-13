import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Conformance.ContractCases

/-!
# Identity — Conformance

Structural witness cases and the deferred-enforcement contract for
the runtime permission decision engine (tracked in #193).
-/

namespace Identity.Conformance

/-- Flat principal payload for JSON emission. -/
structure PrincipalCase where
  did     : String
  enabled : Bool
  deriving Repr

/-- Flat behavior payload for JSON emission. -/
structure BehaviorCase where
  id        : String
  principal : String
  enabled   : Bool
  deriving Repr

/-- Flat deployment payload for JSON emission. -/
structure DeploymentCase where
  id        : String
  principal : String
  hostId    : String
  enabled   : Bool
  deriving Repr

/-- One named scenario: a snapshot of principals/behaviors/deployments
    plus the expected `WellFormed` verdict. -/
structure IdentityStructuralCase where
  name        : String
  principals  : List PrincipalCase
  behaviors   : List BehaviorCase
  deployments : List DeploymentCase
  wellFormed  : Bool
  deriving Repr

def structuralCases : List IdentityStructuralCase :=
  [ { name        := "amy_general_and_amy_code_share_principal"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := "did:agent:amy", enabled := true }
        , { id := "amy-code",    principal := "did:agent:amy", enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "amy_rumination_separate_principal"
    , principals  :=
        [ { did := "did:agent:amy",        enabled := true }
        , { did := "did:agent:rumination", enabled := true } ]
    , behaviors   :=
        [ { id := "amy-general",     principal := "did:agent:amy",        enabled := true }
        , { id := "amy-rumination",  principal := "did:agent:rumination", enabled := true } ]
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := "did:agent:rumination"
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  , { name        := "dangling_behavior_fk_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "orphan", principal := "did:agent:ghost", enabled := true } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "duplicate_behavior_id_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := "did:agent:amy", enabled := true }
        , { id := "amy-general", principal := "did:agent:amy", enabled := false } ]
    , deployments := []
    , wellFormed  := false
    }
  , { name        := "deployment_fk_violates"
    , principals  := [{ did := "did:agent:amy", enabled := true }]
    , behaviors   := []
    , deployments :=
        [ { id := "ghost-deploy"
          , principal := "did:agent:ghost"
          , hostId := "host-3.local"
          , enabled := true } ]
    , wellFormed  := false
    }
  , { name        := "two_deployments_different_principals_ok"
    , principals  :=
        [ { did := "did:agent:amy",        enabled := true }
        , { did := "did:agent:rumination", enabled := true } ]
    , behaviors   := []
    , deployments :=
        [ { id := "deploy-amy"
          , principal := "did:agent:amy"
          , hostId := "host-1.local"
          , enabled := true }
        , { id := "deploy-rumination"
          , principal := "did:agent:rumination"
          , hostId := "host-2.local"
          , enabled := true } ]
    , wellFormed  := true
    }
  ]

open Conformance.Contracts
open Conformance.ContractCases (boolString)

def principalCaseJson (c : PrincipalCase) : String :=
  "{"
    ++ "\"did\":" ++ jsonString c.did ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def behaviorCaseJson (c : BehaviorCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def deploymentCaseJson (c : DeploymentCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"host_id\":" ++ jsonString c.hostId ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def identityStructuralCaseJson (c : IdentityStructuralCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"deployments\":" ++ jsonArray (c.deployments.map deploymentCaseJson) ++ ","
    ++ "\"well_formed\":" ++ boolString c.wellFormed
    ++ "}"

def structuralCasesJson : String :=
  jsonArray (structuralCases.map identityStructuralCaseJson)

/-- A named property the runtime permission engine must satisfy. -/
structure IdentityContract where
  name      : String
  statement : String
  enforced  : Bool
  trackedBy : String
  deriving Repr

def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "For any two AgentBehavior rows b₁, b₂ with " ++
        "b₁.agent_did == b₂.agent_did, the runtime's permission " ++
        "decision function MUST return identical results for any " ++
        "permission."
    , enforced  := false
    , trackedBy := "#193"
    }
  ]

def identityContractJson (c : IdentityContract) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"statement\":" ++ jsonString c.statement ++ ","
    ++ "\"enforced\":" ++ boolString c.enforced ++ ","
    ++ "\"tracked_by\":" ++ jsonString c.trackedBy
    ++ "}"

def identityContractsJson : String :=
  jsonArray (identityContracts.map identityContractJson)

end Identity.Conformance
