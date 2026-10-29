import Proofs.Enrollment.Observation
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ClientObservationAdmission

open Enrollment Conformance.Contracts

structure Case where
  name : String
  actor : Option Did
  node : Did
  current : Option RequestAuthority

private def authority : RequestAuthority :=
  ⟨"network", "admin", "server", "client", "client-peer", "node", "client"⟩

def cases : List Case :=
  [ ⟨"current_client", some "client", "node", some authority⟩
  , ⟨"unauthenticated", none, "node", some authority⟩
  , ⟨"unknown_member", some "client", "node", none⟩
  , ⟨"expired_authorization", some "client", "node", none⟩
  , ⟨"revoked_authorization", some "client", "node", none⟩
  , ⟨"ambiguous_authorization", some "client", "node", none⟩
  , ⟨"other_actor", some "other", "node", some authority⟩
  , ⟨"other_node", some "client", "other", some authority⟩
  , ⟨"other_profile", some "client", "node", some {authority with profile := "peer"}⟩
  , ⟨"empty_actor", some "", "node", some {authority with candidateDid := ""}⟩
  , ⟨"empty_node", some "client", "", some {authority with ownerNode := ""}⟩
  , ⟨"node_operator", some "node", "node", none⟩
  , ⟨"foreign_operator", some "other-node", "node", none⟩ ]

def expected (value : Case) : Bool :=
  clientObservationAdmitted value.actor value.node value.current

example : cases.map expected = [true, false, false, false, false, false,
    false, false, false, false, false, false, false] := by native_decide

private def authorityJson (value : RequestAuthority) : String :=
  "{\"network_id\":" ++ jsonString value.networkId ++
  ",\"admin_did\":" ++ jsonString value.adminDid ++
  ",\"server_peer\":" ++ jsonString value.serverPeer ++
  ",\"candidate_did\":" ++ jsonString value.candidateDid ++
  ",\"candidate_peer\":" ++ jsonString value.candidatePeer ++
  ",\"owner_node\":" ++ jsonString value.ownerNode ++
  ",\"profile\":" ++ jsonString value.profile ++ "}"

private def caseJson (value : Case) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"actor\":" ++ (value.actor.map jsonString).getD "null" ++
  ",\"node\":" ++ jsonString value.node ++
  ",\"current\":" ++ (value.current.map authorityJson).getD "null" ++
  ",\"expected_admitted\":" ++ (if expected value then "true" else "false") ++
  ",\"expected_runtime_admitted\":" ++
    (if runtimeObservationAdmitted value.actor value.node value.current then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.ClientObservationAdmission
