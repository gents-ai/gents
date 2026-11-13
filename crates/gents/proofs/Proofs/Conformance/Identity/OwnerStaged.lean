import Proofs.Store.Identity.OwnerStaged
import Proofs.Conformance.Family

namespace Conformance.OwnerStaged

open Lean Identity.Household Identity.OwnerStaged

structure RelationInput where
  path : String
  requester : String
  deriving ToJson, FromJson

/-- `direct`: the requester's own relationship change takes effect. `staged`:
the owner-staged change made for the requester takes effect. `outcome`: how
the owner-staged request is decided. -/
structure RelationOutput where
  direct : Bool
  staged : Bool
  outcome : String
  deriving ToJson

private def cursorDirect (requester : String) : Bool :=
  cursorWritable (Identity.Automation.rebindEventCursor runtimeCursor requester "cursor" "agent" "sibling")
    "sibling"

private def placementDirect (requester : String) : Bool :=
  Identity.PackPlacementReaders.readable
    (Identity.PackPlacementReaders.install agentPlacement requester [] ["writer"]) "writer"

private def selectionDirect (requester : String) : Bool :=
  allowed 8 (apply runtimeAgent (.grantAs requester selection)) "creator" "agents" "target" "read"

private def cursorGrant (requester target : String) : Outcome :=
  decision runtimeCursor "runtime" agentsOf requester cursorAuthority
    ⟨"configuration", "cursor", "writer", .actor target⟩ true

private def placementGrant (requester reader : String) : Outcome :=
  decision agentPlacement "runtime" agentsOf requester placementAuthority
    (Identity.DeclaredAccess.readerRelation "configuration" "placement" reader) true

private def selectionGrant (requester callback : String) : Outcome :=
  decision runtimeAgent "runtime" agentsOf requester selectionAuthority
    ⟨"agents", "target", "workspace_selected_by", .entitySet "configuration" callback "reader"⟩ true

private def ownerCase (owner : String) (proof : Bool) : RelationOutput :=
  let result := ownerRowOutcome owner proof
  { direct := false
    staged := match result with
      | .staged _ => true
      | _ => false
    outcome := result.name }

/-- One owner-staged request per operation and requester. The Agent-row paths
name the registered owner the runtime would stage as: a forged row, a genuine
nonce under a foreign DID, a duplicated row, no proof, and a genuine Agent. -/
def relation (i : RelationInput) : RelationOutput :=
  let r := i.requester
  match i.path with
  | "event_cursor_rebind" =>
      { direct := cursorDirect r, staged := cursorWritable (cursorRebound r) "sibling"
        outcome := (cursorGrant r "sibling").name }
  | "event_cursor_outsider_writer" =>
      { direct := false, staged := false, outcome := (cursorGrant r "outsider").name }
  | "declared_placement_readers" =>
      { direct := placementDirect r
        staged := Identity.PackPlacementReaders.readable (placementDeclared r) "writer"
        outcome := (placementGrant r "writer").name }
  | "placement_undeclared_reader" =>
      { direct := false, staged := false, outcome := (placementGrant r "outsider").name }
  | "forged_agent_row" => ownerCase "user" true
  | "genuine_nonce_foreign_did" => ownerCase "user2" true
  | "duplicate_agent_row" => ownerCase "dup-agent" true
  | "no_proof" => ownerCase "creator-agent" false
  | "derived_agent_row" => ownerCase "creator-agent" true
  | "workspace_selection" =>
      { direct := selectionDirect r
        staged := allowed 8 (agentSelected r) "creator" "agents" "target" "read"
        outcome := (selectionGrant r "callback").name }
  | "workspace_unbound_callback" =>
      { direct := false, staged := false, outcome := (selectionGrant r "unbound").name }
  | _ => { direct := false, staged := false, outcome := Outcome.denied.name }

@[conformance_family]
def relations : Family RelationInput RelationOutput where
  inputs :=
    (["user", "outsider", "agent"].map fun requester => { path := "event_cursor_rebind", requester }) ++
    [{ path := "event_cursor_outsider_writer", requester := "user" }] ++
    (["user", "outsider"].map fun requester => { path := "declared_placement_readers", requester }) ++
    [{ path := "placement_undeclared_reader", requester := "user" }] ++
    (["forged_agent_row", "genuine_nonce_foreign_did", "duplicate_agent_row", "no_proof",
      "derived_agent_row"].map fun path => { path, requester := "admin" }) ++
    (["user", "outsider"].map fun requester => { path := "workspace_selection", requester }) ++
    [{ path := "workspace_unbound_callback", requester := "user" }]
  model := relation

structure PairingInput where
  requester : String
  document : String
  deriving ToJson, FromJson

/-- `document`: `route` is runtime-owned, `foreign` is owned by another actor,
`missing` does not exist. `http` is how the runtime's owner-staged route
answers the same requester's admitted command. `direct` stays the native
public route's own answer, which no Gents pairing path relies on. -/
structure PairingOutput where
  direct : Bool
  staged : Bool
  outcome : String
  http : String
  deriving ToJson

def pairingCase (i : PairingInput) : PairingOutput :=
  let relation := readerOn i.document
  let granted := request unlinkedPairing "runtime" agentsOf i.requester runtimeNode relation true
  { direct := mayManage unlinkedPairing i.requester relation
    staged := allowed 5 granted "agent" "pairing_observations" i.document "read" &&
      !allowed 5 unlinkedPairing "agent" "pairing_observations" i.document "read"
    outcome := (pairingOutcome i.requester i.document).name
    http := (pairingHttp i.requester i.document).name }

@[conformance_family]
def pairing : Family PairingInput PairingOutput where
  inputs := pairingDocuments.flatMap fun document =>
    pairingRequesters.map fun requester => { requester, document }
  model := pairingCase

structure CommandInput where
  authorityMatches : Bool
  /-- `signer`: the bearer is the signer; `other`: the bearer is the Node
  owner but not the signer; `anonymous`: no bearer. -/
  caller : String
  signatureValid : Bool
  deriving ToJson, FromJson

structure CommandOutput where
  decision : String
  deriving ToJson

private def callerFor (caller : String) (signer : String) : Option String :=
  match caller with
  | "signer" => some signer
  | "other" => some "admin"
  | _ => none

/-- Route admission of a signed owner-staged command for the pairing reader. -/
@[conformance_family]
def routeCommands : Family CommandInput CommandOutput where
  inputs := [true, false].flatMap fun authorityMatches =>
    ["signer", "other", "anonymous"].flatMap fun caller =>
      [true, false].map fun signatureValid => { authorityMatches, caller, signatureValid }
  model := fun i =>
    { decision := (overHttp i.authorityMatches (callerFor i.caller "user") "user" i.signatureValid
        unlinkedPairing "runtime" agentsOf runtimeNode routeReader).name }

structure ApplyInput where
  requester : String
  /-- `revoke` and `grant` apply one change of the route reader; `replan`
  revokes while a concurrent change makes WRITE derive another plan. -/
  direction : String
  deriving ToJson, FromJson

structure ApplyOutput where
  result : String
  committed : Bool
  readable : Bool
  deriving ToJson

/-- One HTTP apply per pairing requester and direction (PLAN, REVOKE, WRITE). -/
@[conformance_family]
def httpApply : Family ApplyInput ApplyOutput where
  inputs := ["revoke", "grant", "replan"].flatMap fun direction =>
    pairingRequesters.map fun requester => { requester, direction }
  model := fun i =>
    let applied := pairingApply i.requester i.direction
    { result := applied.result.name, committed := applied.committed
      readable := pairingReadable applied.state }

end Conformance.OwnerStaged
