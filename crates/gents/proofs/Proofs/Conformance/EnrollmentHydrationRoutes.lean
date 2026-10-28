import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.EnrollmentHydrationRoutes

open Enrollment Conformance.ContractCases Conformance.Contracts

private def approval : Decision :=
  { enrollmentDecision with
    authorizationSequence := 3
    authorizationExpiresAt := "2099-01-01T00:00:00Z" }

private def current : State :=
  ([.observe enrollmentOffer, .confirmPin enrollmentOffer,
    .accept enrollmentOffer enrollmentRequest, .decide enrollmentRequest approval,
    .membership enrollmentRequest approval] : List EnrollmentAction).foldl
      applyEnrollmentAction {}

structure Case where
  name : String
  request : Request := enrollmentRequest
  decision : Decision := approval
  materialized : Bool := true
  desiredCurrent : Bool := false

private def otherRequest : Request :=
  canonicalizeEnrollmentRequest { unsignedEnrollmentRequest with
    requestId := "other-request"
    clientNonce := "other-nonce" }

def cases : List Case :=
  [ { name := "current_generation" }
  , { name := "old_generation", decision := { approval with authorizationSequence := 1 } }
  , { name := "future_generation", decision := { approval with authorizationSequence := 4 } }
  , { name := "other_expiry", decision := { approval with authorizationExpiresAt := "2098-01-01T00:00:00Z" } }
  , { name := "other_request", request := otherRequest }
  , { name := "unbound_materialization", materialized := false }
  , { name := "new_desired_before_old_applied_teardown",
      decision := { approval with authorizationSequence := 1 }, desiredCurrent := true } ]

/-- A restored materialization is an observation, not a new approval. Its
admission is computed by the existing directional hydration projection. -/
def expected (value : Case) : Bool :=
  let request := hydrationRequestFor enrollmentRequest "session-1"
  let routes : Finset AppliedRoute :=
    if value.materialized then {clientToServerRoute value.request value.decision} else {}
  let state := { current with appliedRoutes := routes }
  decide (SessionHydration.admits
    (projectedClientToServerHydrationCatalog state enrollmentRequest.networkId
      {SessionHydration.ownedSession request}) request)

example : cases.map expected = [true, false, false, false, false, false, false] := by native_decide

private def authorityJson : String :=
  "{\"network_id\":" ++ jsonString enrollmentRequest.networkId ++
  ",\"request_id\":" ++ jsonString enrollmentRequest.requestId ++
  ",\"admin_did\":" ++ jsonString enrollmentRequest.adminDid ++
  ",\"member_did\":" ++ jsonString enrollmentRequest.candidateDid ++
  ",\"member_peer\":" ++ jsonString enrollmentRequest.candidatePeer ++
  ",\"member_ticket\":" ++ jsonString enrollmentRequest.candidatePeer ++
  ",\"owner_node\":" ++ jsonString enrollmentRequest.ownerNode ++
  ",\"profile\":" ++ jsonString enrollmentRequest.profile ++
  ",\"request_digest\":" ++ jsonString (renderDigestString enrollmentRequest.digest) ++
  ",\"authorization_sequence\":" ++ toString approval.authorizationSequence ++
  ",\"authorization_expires_at\":" ++ jsonString approval.authorizationExpiresAt ++ "}"

private def generationJson (request : Request) (decision : Decision) (present : Bool) : String :=
  "\"enrollment_request_digest\":" ++ (if present then
    jsonString (renderDigestString request.digest) else "null") ++
  ",\"enrollment_authorization_sequence\":" ++ (if present then
    toString decision.authorizationSequence else "null") ++
  ",\"enrollment_authorization_expires_at\":" ++ (if present then
    jsonString decision.authorizationExpiresAt else "null")

private def caseJson (value : Case) : String :=
  let desiredRequest := if value.desiredCurrent then enrollmentRequest else value.request
  let desiredDecision := if value.desiredCurrent then approval else value.decision
  "{\"name\":" ++ jsonString value.name ++
  ",\"authority\":" ++ authorityJson ++
  ",\"desired\":{\"peer_id\":" ++ jsonString enrollmentRequest.candidatePeer ++
  ",\"node_did\":" ++ jsonString enrollmentRequest.ownerNode ++
  "," ++ generationJson desiredRequest desiredDecision value.materialized ++ "}" ++
  ",\"applied\":{" ++ generationJson value.request value.decision value.materialized ++ "}" ++
  ",\"expected_admitted\":" ++ (if expected value then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.EnrollmentHydrationRoutes
