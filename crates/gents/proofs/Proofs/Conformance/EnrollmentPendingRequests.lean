import Proofs.P2P.Enrollment.DurableProjection
import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.EnrollmentPendingRequests

open Enrollment Conformance.Contracts

structure Case where
  name : String
  requested : RequestAuthority
  current : RequestAuthority
  hasDecision : Bool := false
  currentApproval : Bool := true
  routeApplied : Bool := true
  requestFresh : Bool := true

private def authority : RequestAuthority :=
  ⟨"network", "admin", "server", "client", "client-peer", "node", "client"⟩

def satisfactionCases : List Case :=
  [ ⟨"reconnected_route_satisfies_pending_request", authority, authority, false, true, true, true⟩
  , { name := "decided_reenrollment_still_needs_its_own_route", requested := authority,
      current := authority, hasDecision := true }
  , { name := "revoked_or_expired_authorization", requested := authority,
      current := authority, currentApproval := false }
  , { name := "route_still_reconnecting", requested := authority,
      current := authority, routeApplied := false }
  , ⟨"different_network", authority, { authority with networkId := "other" }, false, true, true, true⟩
  , ⟨"different_admin", authority, { authority with adminDid := "other" }, false, true, true, true⟩
  , ⟨"different_server", authority, { authority with serverPeer := "other" }, false, true, true, true⟩
  , ⟨"different_client_did", authority, { authority with candidateDid := "other" }, false, true, true, true⟩
  , ⟨"different_client_peer", authority, { authority with candidatePeer := "other" }, false, true, true, true⟩
  , ⟨"different_target_node", authority, { authority with ownerNode := "other" }, false, true, true, true⟩
  , ⟨"different_requested_profile", { authority with profile := "operator" }, authority, false, true, true, true⟩ ]

def expected (value : Case) : Bool :=
  pendingRequestSatisfied value.requested value.current value.hasDecision
    value.currentApproval value.routeApplied

example : satisfactionCases.map expected = [true, false, false, false, false, false,
    false, false, false, false, false] := by native_decide

def visibilityExpected (value : Case) : Bool :=
  activeRequestVisible value.requestFresh value.hasDecision value.currentApproval value.routeApplied

private def boolName (value : Bool) : String := if value then "true" else "false"

def visibilityCases : List Case :=
  [false, true].flatMap fun fresh =>
    [false, true].flatMap fun decided =>
      [false, true].flatMap fun current =>
        [false, true].map fun applied =>
          { name := "visibility_" ++ boolName fresh ++ "_" ++ boolName decided ++ "_" ++
              boolName current ++ "_" ++ boolName applied,
            requested := authority, current := authority, hasDecision := decided,
            currentApproval := current, routeApplied := applied, requestFresh := fresh }

def cases : List Case := satisfactionCases ++ visibilityCases

private def boolJson (value : Bool) : String := if value then "true" else "false"

deriving instance Lean.ToJson for RequestAuthority

structure PendingInput where
  name : String
  requested : RequestAuthority
  current : RequestAuthority
  hasDecision : Bool
  currentApproval : Bool
  routeApplied : Bool
  requestFresh : Bool
  deriving Lean.ToJson

private def pendingInput (value : Case) : PendingInput :=
  { name := value.name, requested := value.requested, current := value.current,
    hasDecision := value.hasDecision, currentApproval := value.currentApproval,
    routeApplied := value.routeApplied, requestFresh := value.requestFresh }

structure SatisfactionOutput where
  satisfied : Bool
  deriving Lean.ToJson

@[conformance_family]
def satisfaction : Family PendingInput SatisfactionOutput where
  inputs := cases.map pendingInput
  model := fun i =>
    let satisfied := pendingRequestSatisfied i.requested i.current i.hasDecision
      i.currentApproval i.routeApplied
    { satisfied }

structure VisibilityOutput where
  visible : Bool
  deriving Lean.ToJson

@[conformance_family]
def visibility : Family PendingInput VisibilityOutput where
  inputs := cases.map pendingInput
  model := fun i =>
    let visible := activeRequestVisible i.requestFresh i.hasDecision i.currentApproval
      i.routeApplied
    { visible }

example : activeRequestVisible false true true false = true := rfl
example : activeRequestVisible false false false false = false := rfl
example : activeRequestVisible false true false false = false := rfl

structure ListingOutput where
  listed : Bool
  deriving Lean.ToJson

/-- The cases a served desktop reaches with one signed authority: its pending
request while the route reconnects, after the exact route applies, and after a
later revision revokes the authorization. The other cases change the requested
or current authority itself, which the admin's signed decision fixes, so a
served desktop cannot persist them. -/
def listingCases : List Case :=
  cases.filter fun c => c.name ∈ ["route_still_reconnecting",
    "reconnected_route_satisfies_pending_request", "revoked_or_expired_authorization"]

@[conformance_family]
def listing : Family PendingInput ListingOutput where
  inputs := listingCases.map pendingInput
  model := fun i =>
    let listed := !pendingRequestSatisfied i.requested i.current i.hasDecision
      i.currentApproval i.routeApplied
    { listed }

structure AbsentPeerInput where
  managed : Bool
  initialReady : Bool
  deriving Lean.ToJson

structure AbsentPeerOutput where
  present : Bool
  ready : Bool
  deriving Lean.ToJson

@[conformance_family]
def absentPeer : Family AbsentPeerInput AbsentPeerOutput where
  inputs := [false, true].flatMap fun managed =>
    [false, true].map fun initialReady => { managed, initialReady }
  model := fun i =>
    let expected := absentEnrollmentPeer i.managed
    { present := expected.1, ready := expected.2 }

structure RouteCase where
  name : String
  enrollment : Bool := true
  routeApplied : Bool := true
  network : Bool := true
  digest : Bool := true
  sequenceFresh : Bool := true
  leaseFresh : Bool := true
  operatorEndpoint : Bool := false
  deriving Lean.ToJson

def routeCases : List RouteCase :=
  [ { name := "applied_fresh_enrollment" }
  , { name := "managed_endpoint_without_route", routeApplied := false, operatorEndpoint := true }
  , { name := "managed_endpoint_with_route", operatorEndpoint := true }
  , { name := "not_enrollment", enrollment := false, operatorEndpoint := true }
  , { name := "missing_network", network := false }
  , { name := "missing_digest", digest := false }
  , { name := "invalid_sequence", sequenceFresh := false }
  , { name := "expired_lease", leaseFresh := false, operatorEndpoint := true } ]

structure RouteReadinessOutput where
  ready : Bool
  deriving Lean.ToJson

@[conformance_family]
def routeReadiness : Family RouteCase RouteReadinessOutput where
  inputs := routeCases
  model := fun c =>
    let ready := enrollmentRouteReady c.enrollment c.routeApplied c.network c.digest
      c.sequenceFresh c.leaseFresh c.operatorEndpoint
    { ready }

/-- The input is a fresh runtime with no request, decision, or authorization. -/
def freshStartupPreservesAuthority : Bool :=
  let before : Enrollment.State := {}
  let after := reconcileExisting before Conformance.ContractCases.enrollmentRequest
    Conformance.ContractCases.enrollmentDecision
  decide (after.acceptedRequests = before.acceptedRequests ∧
    after.decisions = before.decisions ∧ after.authorizations = before.authorizations)

def startupCasesJson : String :=
  "[{\"name\":\"fresh_startup\",\"expected_authority_unchanged\":" ++
    boolJson freshStartupPreservesAuthority ++ "}]"

end Conformance.EnrollmentPendingRequests
