import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Conformance.ContractCases

namespace Identity.Conformance

def amyDid : String :=
  "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"

def ruminationDid : String :=
  "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"

def ghostDid : String :=
  "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH"

structure PrincipalCase where
  did     : String
  enabled : Bool
  deriving Repr

structure BehaviorCase where
  id        : String
  principal : String
  enabled   : Bool
  deriving Repr

structure PermissionGrantCase where
  principal  : String
  permission : String
  deriving Repr

structure IdentityStructuralCase where
  name        : String
  principals  : List PrincipalCase
  behaviors   : List BehaviorCase
  deriving Repr

def behaviorCaseToBehavior (c : BehaviorCase) : Behavior :=
  { id := c.id, principal := c.principal, displayName := none, enabled := c.enabled }

def IdentityStructuralCase.world (c : IdentityStructuralCase) : World :=
  { principals := (c.principals.map (fun p =>
      ({ did := p.did, displayName := none, enabled := p.enabled } : Principal))).toFinset,
    behaviors := (c.behaviors.map behaviorCaseToBehavior).toFinset }

def IdentityStructuralCase.wellFormed (c : IdentityStructuralCase) : Bool :=
  decide c.world.WellFormed

def structuralCases : List IdentityStructuralCase :=
  [ { name        := "amy_general_and_amy_code_share_principal"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := amyDid, enabled := true }
        , { id := "amy-code",    principal := amyDid, enabled := true } ]
    }
  , { name        := "amy_rumination_separate_principal"
    , principals  :=
        [ { did := amyDid,        enabled := true }
        , { did := ruminationDid, enabled := true } ]
    , behaviors   :=
        [ { id := "amy-general",     principal := amyDid,        enabled := true }
        , { id := "amy-rumination",  principal := ruminationDid, enabled := true } ]
    }
  , { name        := "dangling_behavior_fk_violates"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "orphan", principal := ghostDid, enabled := true } ]
    }
  , { name        := "duplicate_behavior_id_violates"
    , principals  := [{ did := amyDid, enabled := true }]
    , behaviors   :=
        [ { id := "amy-general", principal := amyDid, enabled := true }
        , { id := "amy-general", principal := amyDid, enabled := false } ]
    }
  , { name := "same_behavior_label_across_principals_allowed"
    , principals := [{ did := amyDid, enabled := true }, { did := ruminationDid, enabled := true }]
    , behaviors := [{ id := "coding", principal := amyDid, enabled := true },
                    { id := "coding", principal := ruminationDid, enabled := true }] }


  ]

structure IdentityPermissionCase where
  name                     : String
  principals               : List PrincipalCase
  behaviors                : List BehaviorCase
  grants                   : List PermissionGrantCase
  permission               : String
  rowOwner                 : String
  actorPrincipal           : String
  actorBehavior            : String
  peerPrincipal            : String
  peerBehavior             : String
  expectedActorPrincipal   : Option String
  expectedPeerPrincipal    : Option String
  expectedActorAllowed     : Bool
  expectedPeerAllowed      : Bool
  samePrincipal            : Bool
  expectedDecisionsEqual   : Bool
  deriving Repr


def grantStoreFromCases (grants : List PermissionGrantCase) : GrantStore String :=
  { granted := fun principal permission =>
      grants.any (fun grant =>
        grant.principal == principal && grant.permission == permission) }

def permissionDecideFromGrants (grants : List PermissionGrantCase) :
    Decide String :=
  canonicalDecide (grantStoreFromCases grants)

theorem permissionDecideFromGrants_respectsPrincipal
    (grants : List PermissionGrantCase) :
    RespectsPrincipal (permissionDecideFromGrants grants) :=
  canonicalDecide_respectsPrincipal (grantStoreFromCases grants)

def mkIdentityPermissionCase
    (name : String)
    (principals : List PrincipalCase)
    (behaviors : List BehaviorCase)
    (grants : List PermissionGrantCase)
    (permission rowOwner actorPrincipal actorBehavior peerPrincipal peerBehavior : String) :
    IdentityPermissionCase :=
  let actor := Identity.findBehavior? (behaviors.map behaviorCaseToBehavior) actorPrincipal actorBehavior
  let peer := Identity.findBehavior? (behaviors.map behaviorCaseToBehavior) peerPrincipal peerBehavior
  let actorAllowed := actor.any (fun behavior => permissionDecideFromGrants grants behavior permission)
  let peerAllowed := peer.any (fun behavior => permissionDecideFromGrants grants behavior permission)
  { name := name
  , principals := principals
  , behaviors := behaviors
  , grants := grants
  , permission := permission
  , rowOwner := rowOwner
  , actorPrincipal := actorPrincipal
  , actorBehavior := actorBehavior
  , peerPrincipal := peerPrincipal
  , peerBehavior := peerBehavior
  , expectedActorPrincipal := actor.map (·.principal)
  , expectedPeerPrincipal := peer.map (·.principal)
  , expectedActorAllowed := actorAllowed
  , expectedPeerAllowed := peerAllowed
  , samePrincipal := actor.any (fun a => peer.any (fun b => a.principal == b.principal))
  , expectedDecisionsEqual := actorAllowed == peerAllowed
  }

def amyPrincipal : PrincipalCase :=
  { did := amyDid, enabled := true }

def ruminationPrincipal : PrincipalCase :=
  { did := ruminationDid, enabled := true }

def amyGeneralBehavior : BehaviorCase :=
  { id := "amy-general", principal := amyDid, enabled := true }

def amyCodeBehavior : BehaviorCase :=
  { id := "amy-code", principal := amyDid, enabled := true }

def amyRuminationBehavior : BehaviorCase :=
  { id := "amy-rumination", principal := ruminationDid, enabled := true }

def amyRowReadPermission : String :=
  "row:" ++ amyDid ++ ":memory.read"

def ruminationRowReadPermission : String :=
  "row:" ++ ruminationDid ++ ":journal.read"

def grant (principal permission : String) : PermissionGrantCase :=
  { principal := principal, permission := permission }

def identityPermissionCases : List IdentityPermissionCase :=
  [ mkIdentityPermissionCase
      "same_principal_row_owner_grant_allows_shared_behaviors"
      [amyPrincipal]
      [amyGeneralBehavior, amyCodeBehavior]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      amyDid "amy-code"
  , mkIdentityPermissionCase
      "separate_principal_without_grant_blocks_peer"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyRuminationBehavior]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      ruminationDid "amy-rumination"
  , mkIdentityPermissionCase
      "separate_principal_with_grant_allows_peer"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyRuminationBehavior]
      [ grant amyDid amyRowReadPermission
      , grant ruminationDid amyRowReadPermission ]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      ruminationDid "amy-rumination"
  , mkIdentityPermissionCase
      "scoped_behavior_lookup_selects_declared_principal"
      [amyPrincipal, ruminationPrincipal]
      [amyGeneralBehavior, amyCodeBehavior, amyRuminationBehavior]
      [grant ruminationDid ruminationRowReadPermission]
      ruminationRowReadPermission
      ruminationDid
      amyDid "amy-code"
      ruminationDid "amy-rumination"
  ]

theorem structural_scope_cases_pinned : structuralCases.map (·.wellFormed) =
    [true, true, false, false, true] := by native_decide

/-- Same label never borrows the other principal's ACP grant, regardless of
query ordering. Ambiguous same-owner documents fail resolution. -/
def sharedLabelBehaviors : List BehaviorCase :=
  [{ id := "coding", principal := amyDid, enabled := true },
   { id := "coding", principal := ruminationDid, enabled := true }]

def scopedSelectionCases : List IdentityPermissionCase :=
  [ mkIdentityPermissionCase "same-label-scoped-selection-forward"
      [amyPrincipal, ruminationPrincipal] sharedLabelBehaviors
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "coding" ruminationDid "coding"
  , mkIdentityPermissionCase "same-label-scoped-selection-reverse"
      [amyPrincipal, ruminationPrincipal] sharedLabelBehaviors.reverse
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "coding" ruminationDid "coding"
  , mkIdentityPermissionCase "unknown-owner-does-not-select-other-principal"
      [amyPrincipal, ruminationPrincipal] sharedLabelBehaviors
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      ghostDid "coding" amyDid "coding"
  , mkIdentityPermissionCase "same-owner-collision-does-not-select-first-row"
      [amyPrincipal] [amyGeneralBehavior, { amyGeneralBehavior with enabled := false }]
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "amy-general" amyDid "amy-general" ]

theorem scoped_selection_permission_cases_pinned : scopedSelectionCases.map
    (fun c => (c.expectedActorPrincipal, c.expectedPeerPrincipal,
      c.expectedActorAllowed, c.expectedPeerAllowed)) =
    [(some amyDid, some ruminationDid, true, false),
     (some amyDid, some ruminationDid, true, false),
     (none, some amyDid, false, true), (none, none, false, false)] := by native_decide

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

def permissionGrantCaseJson (c : PermissionGrantCase) : String :=
  "{"
    ++ "\"principal\":" ++ jsonString c.principal ++ ","
    ++ "\"permission\":" ++ jsonString c.permission
    ++ "}"

def identityStructuralCaseJson (c : IdentityStructuralCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"well_formed\":" ++ boolString c.wellFormed
    ++ "}"

def structuralCasesJson : String :=
  jsonArray (structuralCases.map identityStructuralCaseJson)

def identityPermissionCaseJson (c : IdentityPermissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"principals\":" ++ jsonArray (c.principals.map principalCaseJson) ++ ","
    ++ "\"behaviors\":" ++ jsonArray (c.behaviors.map behaviorCaseJson) ++ ","
    ++ "\"grants\":" ++ jsonArray (c.grants.map permissionGrantCaseJson) ++ ","
    ++ "\"permission\":" ++ jsonString c.permission ++ ","
    ++ "\"row_owner\":" ++ jsonString c.rowOwner ++ ","
    ++ "\"actor_principal\":" ++ jsonString c.actorPrincipal ++ ","
    ++ "\"actor_behavior\":" ++ jsonString c.actorBehavior ++ ","
    ++ "\"peer_principal\":" ++ jsonString c.peerPrincipal ++ ","
    ++ "\"peer_behavior\":" ++ jsonString c.peerBehavior ++ ","
    ++ "\"expected_actor_principal\":"
      ++ (c.expectedActorPrincipal.map jsonString |>.getD "null") ++ ","
    ++ "\"expected_peer_principal\":"
      ++ (c.expectedPeerPrincipal.map jsonString |>.getD "null") ++ ","
    ++ "\"expected_actor_allowed\":"
      ++ boolString c.expectedActorAllowed ++ ","
    ++ "\"expected_peer_allowed\":"
      ++ boolString c.expectedPeerAllowed ++ ","
    ++ "\"same_principal\":" ++ boolString c.samePrincipal ++ ","
    ++ "\"expected_decisions_equal\":"
      ++ boolString c.expectedDecisionsEqual
    ++ "}"

def identityPermissionCasesJson : String :=
  jsonArray ((identityPermissionCases ++ scopedSelectionCases).map identityPermissionCaseJson)

structure IdentityContract where
  name      : String
  statement : String
  enforced  : Bool
  trackedBy : String
  deriving Repr

def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "Target contract: resolve behaviors by (agent_did, behavior_id). " ++
        "For any two AgentBehavior rows b1, b2 with " ++
        "b1.agent_did == b2.agent_did, the runtime supplies the same " ++
        "Identity::Authenticated(did) as the actor for any DefraDB ACP " ++
        "check, so any DID-keyed permission decision returns identical " ++
        "results."
    , enforced  := false
    , trackedBy := "#1436"
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
