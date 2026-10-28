import Proofs.Enrollment.DurableProjection
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

end Conformance.EnrollmentPendingRequests
