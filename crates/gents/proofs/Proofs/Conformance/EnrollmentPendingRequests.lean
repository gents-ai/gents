import Proofs.Enrollment.DurableProjection
import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Helpers

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

private def authorityJson (value : RequestAuthority) : String :=
  "{\"network_id\":" ++ jsonString value.networkId ++
  ",\"admin_did\":" ++ jsonString value.adminDid ++
  ",\"server_peer\":" ++ jsonString value.serverPeer ++
  ",\"candidate_did\":" ++ jsonString value.candidateDid ++
  ",\"candidate_peer\":" ++ jsonString value.candidatePeer ++
  ",\"owner_node\":" ++ jsonString value.ownerNode ++
  ",\"profile\":" ++ jsonString value.profile ++ "}"

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def caseJson (value : Case) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"requested\":" ++ authorityJson value.requested ++
  ",\"current\":" ++ authorityJson value.current ++
  ",\"has_decision\":" ++ boolJson value.hasDecision ++
  ",\"current_approval\":" ++ boolJson value.currentApproval ++
  ",\"route_applied\":" ++ boolJson value.routeApplied ++
  ",\"request_fresh\":" ++ boolJson value.requestFresh ++
  ",\"expected_visible\":" ++ boolJson (visibilityExpected value) ++
  ",\"expected_satisfied\":" ++ boolJson (expected value) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

def absentPeerCasesJson : String :=
  jsonArray ([false, true].flatMap fun managed =>
    [false, true].map fun ready =>
      let expected := absentEnrollmentPeer managed
      "{\"managed\":" ++ boolJson managed ++
      ",\"initial_ready\":" ++ boolJson ready ++
      ",\"expected_present\":" ++ boolJson expected.1 ++
      ",\"expected_ready\":" ++ boolJson expected.2 ++ "}")

structure RouteCase where
  name : String
  enrollment : Bool := true
  routeApplied : Bool := true
  network : Bool := true
  digest : Bool := true
  sequenceFresh : Bool := true
  leaseFresh : Bool := true
  operatorEndpoint : Bool := false

def routeCases : List RouteCase :=
  [ { name := "applied_fresh_enrollment" }
  , { name := "managed_endpoint_without_route", routeApplied := false, operatorEndpoint := true }
  , { name := "managed_endpoint_with_route", operatorEndpoint := true }
  , { name := "not_enrollment", enrollment := false, operatorEndpoint := true }
  , { name := "missing_network", network := false }
  , { name := "missing_digest", digest := false }
  , { name := "invalid_sequence", sequenceFresh := false }
  , { name := "expired_lease", leaseFresh := false, operatorEndpoint := true } ]

private def routeCaseJson (c : RouteCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"enrollment\":" ++ boolJson c.enrollment ++
  ",\"route_applied\":" ++ boolJson c.routeApplied ++
  ",\"network\":" ++ boolJson c.network ++
  ",\"digest\":" ++ boolJson c.digest ++
  ",\"sequence_fresh\":" ++ boolJson c.sequenceFresh ++
  ",\"lease_fresh\":" ++ boolJson c.leaseFresh ++
  ",\"operator_endpoint\":" ++ boolJson c.operatorEndpoint ++
  ",\"expected_ready\":" ++ boolJson (enrollmentRouteReady c.enrollment c.routeApplied
    c.network c.digest c.sequenceFresh c.leaseFresh c.operatorEndpoint) ++ "}"

def routeCasesJson : String := jsonArray (routeCases.map routeCaseJson)

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
