import Proofs.P2P.PairingReconcile.DataPlane
import Proofs.Conformance.PairingOwnedDiff

namespace Conformance.PairingDataPlane

open Lean PairingReconcile.DataPlane
open PairingReconcile.OwnedDiff (Scope)

deriving instance ToJson for Row, Endpoint, Refusal, Desired

structure DesiredInput where
  name : String
  row : Row
  endpoint : Endpoint
  selfDid : String
  deriving ToJson

structure DesiredOutput where
  refusal : Option Refusal
  desired : Option Desired
  deriving ToJson

private def self : String := "did:key:self"

private def signed (nodeDid : String := "did:key:peer") (desiredId : String := "peer")
    (address : String := "signed-address") : Endpoint :=
  { desiredId, peerId := "peer", nodeDid, address
    requestDigest := "digest", sequence := 1, expiresAt := "2099-09-29T00:00:00Z" }

private def blank : Row :=
  { routeKey := none, nodeDid := none, addresses := [], template := none, source := none
    requestDigest := none, sequence := none, expiresAt := none }

private def machine : Row := { blank with template := some "machine" }

private def client (routeKey : Option String) (source : String) : Row :=
  { blank with template := some "client", routeKey, source := some source }

private def desiredCase (name : String) (row : Row) (endpoint : Endpoint := signed) :
    DesiredInput :=
  { name, row, endpoint, selfDid := self }

private def desiredInputs : List DesiredInput :=
  [ desiredCase "machine_row_scopes_the_owned_directory" machine
  , desiredCase "matching_row_address_changes_nothing"
      { machine with addresses := ["signed-address"] }
  , desiredCase "forged_row_address_is_replaced_by_the_signed_one"
      { machine with addresses := ["forged-address"] }
  , desiredCase "padded_signed_address_is_trimmed" machine (signed (address := " signed-address "))
  , desiredCase "blank_signed_address_yields_no_route" machine (signed (address := " "))
  , desiredCase "own_scope_is_accepted" { machine with nodeDid := some self }
  , desiredCase "padded_own_scope_is_accepted" { machine with nodeDid := some s!" {self} " }
  , desiredCase "blank_scope_is_unstated" { machine with nodeDid := some "  " }
  , desiredCase "foreign_scope_is_refused" { machine with nodeDid := some "did:key:someone-else" }
  , desiredCase "foreign_scope_is_refused_before_a_missing_template"
      { blank with nodeDid := some "did:key:someone-else" }
  , desiredCase "missing_template_is_refused" blank
  , desiredCase "blank_template_is_refused" { blank with template := some " " }
  , desiredCase "unknown_template_is_refused" { blank with template := some "not-a-template" }
  , desiredCase "padded_template_is_resolved" { blank with template := some " machine " }
  , desiredCase "machine_row_needs_no_signed_peer_did" machine (signed (nodeDid := ""))
  , desiredCase "client_to_runtime_key_scopes_the_local_client"
      (client (some "runtime:client-to-runtime") "enrollment")
  , desiredCase "runtime_to_client_key_scopes_the_local_runtime"
      (client (some "phone:runtime-to-client") "operator")
  , desiredCase "padded_route_key_is_directed"
      (client (some " phone:runtime-to-client ") "operator")
  , desiredCase "enrollment_base_key_runs_runtime_to_client" (client (some "peer") "enrollment")
  , desiredCase "undirected_local_key_is_refused" (client (some "peer") "operator")
  , desiredCase "direction_without_a_directory_is_refused"
      (client (some ":client-to-runtime") "operator")
  , desiredCase "blank_directory_before_a_direction_is_refused"
      (client (some "  :runtime-to-client") "operator")
  , desiredCase "enrollment_key_without_a_directory_runs_runtime_to_client"
      (client (some ":client-to-runtime") "enrollment")
  , desiredCase "client_row_without_a_key_is_refused" (client none "enrollment")
  , desiredCase "client_row_with_a_blank_key_is_refused" (client (some " ") "operator")
  , desiredCase "client_row_needs_a_signed_peer_did" (client (some "peer") "enrollment")
      (signed (nodeDid := " "))
  , desiredCase "undirected_key_is_refused_before_an_unsigned_peer"
      (client (some "peer") "operator") (signed (nodeDid := "")) ]

@[conformance_family]
def desired : Family DesiredInput DesiredOutput where
  inputs := desiredInputs
  model := fun i =>
    match desiredOf i.row i.endpoint i.selfDid with
    | .ok desired => { refusal := none, desired := some desired }
    | .error refusal => { refusal := some refusal, desired := none }

structure OwnershipInput where
  name : String
  entries : List Endpoint
  key : String
  selfDid : String
  row : Row
  deriving ToJson

structure OwnershipOutput where
  admitted : Option Endpoint
  base : Option Row
  overlay : Option Row
  deriving ToJson

private def witness : Row :=
  { blank with source := some "enrollment", requestDigest := some "digest", sequence := some 1
               expiresAt := some "2099-09-29T00:00:00Z" }

private def ownershipCase (name : String) (row : Row) (entries : List Endpoint := [signed])
    (key : String := "peer") : OwnershipInput :=
  { name, entries, key, selfDid := self, row }

private def ownershipInputs : List OwnershipInput :=
  [ ownershipCase "enrollment_owns_the_row_witnessing_its_generation" witness
  , ownershipCase "enrollment_row_is_rewritten_from_the_enrollment"
      { witness with nodeDid := some "did:key:attacker", template := some "machine"
                     addresses := ["attacker-address"], routeKey := some "attacker" }
  , ownershipCase "enrollment_row_without_a_generation_has_no_owner"
      { blank with source := some "enrollment" }
  , ownershipCase "enrollment_row_of_another_sequence_has_no_owner"
      { witness with sequence := some 2 }
  , ownershipCase "enrollment_row_of_another_digest_has_no_owner"
      { witness with requestDigest := some "other" }
  , ownershipCase "enrollment_row_of_another_expiry_has_no_owner"
      { witness with expiresAt := some "2099-09-30T00:00:00Z" }
  , ownershipCase "local_writer_owns_a_row_of_any_other_source"
      { blank with source := some "operator", template := some "machine" }
  , ownershipCase "local_row_keeps_its_template_and_nothing_else"
      { blank with source := some "operator", template := some "machine"
                   nodeDid := some "did:key:attacker", addresses := ["attacker-address"]
                   routeKey := some "attacker" }
  , ownershipCase "local_row_carrying_a_generation_is_still_local"
      { witness with source := some "operator" }
  , ownershipCase "row_without_a_source_has_no_owner" blank
  , ownershipCase "row_with_a_blank_source_has_no_owner" { blank with source := some " " }
  , ownershipCase "padded_enrollment_source_has_no_owner"
      { witness with source := some " enrollment " }
  , ownershipCase "self_endpoint_admits_nothing" witness [signed (nodeDid := self)]
  , ownershipCase "another_route_key_admits_nothing" witness [signed (desiredId := "other")]
  , ownershipCase "no_enrollment_admits_nothing" witness []
  , ownershipCase "self_endpoint_is_skipped_for_a_later_one" witness
      [signed (nodeDid := self), signed (address := "second-address")] ]

@[conformance_family]
def ownership : Family OwnershipInput OwnershipOutput where
  inputs := ownershipInputs
  model := fun i =>
    let entry := admitted i.entries i.key i.selfDid
    { admitted := entry
      base := entry.bind (enrollmentBase i.row · i.selfDid)
      overlay := entry.bind (localOverlay i.row · i.selfDid) }

structure MergeInput where
  name : String
  localDid : String
  peerDid : String
  base : Option Desired
  dataPlane : Option Desired
  deriving ToJson

structure MergeOutput where
  merged : Option Desired
  deriving ToJson

private def control : Desired :=
  { addresses := ["base-address"], collections := ["ControlB", "ControlA"]
    filter := [⟨"ControlA", "node_did", "did:key:base"⟩], templates := ["machine"] }

private def data : Desired :=
  { addresses := ["signed-address"], collections := ["AgentRequest"]
    filter := [⟨"AgentRequest", "node_did", "did:key:a"⟩], templates := ["client"] }

private def mergeCase (name : String) (base dataPlane : Option Desired)
    (peerDid : String := "did:key:peer") : MergeInput :=
  { name, localDid := self, peerDid, base, dataPlane }

private def mergeInputs : List MergeInput :=
  [ mergeCase "no_layer_is_no_wiring" none none
  , mergeCase "base_layer_alone" (some control) none
  , mergeCase "data_plane_layer_alone" none (some data)
  , mergeCase "layers_union_and_the_signed_address_wins" (some control) (some data)
  , mergeCase "data_plane_scope_replaces_the_base_scope_of_its_collection" (some control)
      (some { data with collections := ["ControlA"]
                        filter := [⟨"ControlA", "node_did", "did:key:a"⟩] })
  , mergeCase "addressless_data_plane_keeps_the_base_address" (some control)
      (some { data with addresses := [] })
  , mergeCase "shared_template_is_kept_once" (some control)
      (some { data with templates := ["machine"] })
  , mergeCase "self_peer_has_no_base_layer" (some control) none self
  , mergeCase "self_peer_keeps_only_its_data_plane_layer" (some control) (some data) self ]

@[conformance_family]
def merge : Family MergeInput MergeOutput where
  inputs := mergeInputs
  model := fun i => { merged := mergeLayered i.localDid i.peerDid i.base i.dataPlane }

end Conformance.PairingDataPlane
