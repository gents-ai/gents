import Lean
import Proofs.Identity.Household
import Proofs.Identity.Automation
import Proofs.Identity.PackPlacementReaders

namespace Identity.OwnerStaged

open Identity.Household

/-- Native DefraDB (defradb.rs #2770) refuses a relationship change by a node
administrator that does not manage the document on every public route; only
the trusted embedder's route still admits one. Gents never relies on that
admission. A configuration consequence that is an owner-managed relation is
staged by the document's registered owner: the actor itself when it owns the
document, otherwise the embedding runtime as that owner, in the same
configuration transaction, and only when `authorized` holds for the
requesting actor. An unauthorized request stages nothing, and the owner's own
`mayManage` still bounds every change. -/
def ownerStaged (state : State) (authorized : Bool) (owner : DID)
    (relation : Relation) (grant : Bool) : State :=
  if !authorized then state
  else apply state (if grant then .grantAs owner relation else .revokeAs owner relation)

theorem unauthorized_request_stages_nothing (state : State) (owner : DID)
    (relation : Relation) (grant : Bool) :
    ownerStaged state false owner relation grant = state := rfl

/-- A configuration actor requests an owner-staged change only for a document
it may itself update. -/
def mayRequest (state : State) (requester resource id : String) : Bool :=
  allowed 5 state requester resource id "update"

def emptyHome : State :=
  { objects := [⟨"nodes", "node", "user"⟩],
    relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩], enrolled := [] }

/-! ### Event cursor rebinding (`Automation.rebindEventCursor`)

A cursor seeded by the runtime is owned by the runtime. Its `writer` has no
manager relation, so only that creator may rebind it. -/

def runtimeCursor : State :=
  Automation.publishEventCursor emptyHome "runtime" "cursor" "node" "agent" true true

def cursorRebound (requester : String) : State :=
  let authorized := mayRequest runtimeCursor requester "configuration" "cursor"
  let revoked := ownerStaged runtimeCursor authorized "runtime"
    ⟨"configuration", "cursor", "writer", .actor "agent"⟩ false
  ownerStaged revoked authorized "runtime"
    ⟨"configuration", "cursor", "writer", .actor "sibling"⟩ true

def cursorWritable (state : State) (agent : String) : Bool :=
  allowed 5 state agent "configuration" "cursor" "update"

theorem node_owner_cannot_rebind_a_runtime_cursor_directly :
    let direct := Automation.rebindEventCursor runtimeCursor "user" "cursor" "agent" "sibling"
    [cursorWritable direct "agent", cursorWritable direct "sibling"] = [true, false] := by
  native_decide

theorem runtime_rebinds_its_cursor_for_the_configuring_user :
    [cursorWritable (cursorRebound "user") "agent",
     cursorWritable (cursorRebound "user") "sibling",
     cursorWritable (cursorRebound "outsider") "agent",
     cursorWritable (cursorRebound "outsider") "sibling"] = [false, true, true, false] := by
  native_decide

/-! ### Declared placement readers (`PackPlacementReaders`)

A placement an Agent created is owned by that Agent; the installing User may
update it through its Node parent but cannot manage its readers. -/

def agentPlacement : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "placement", "creator-agent"⟩],
    relations := [⟨"configuration", "placement", "parent", .object "nodes" "node"⟩],
    enrolled := [] }

def placementDeclared (requester : String) : State :=
  ownerStaged agentPlacement (mayRequest agentPlacement requester "configuration" "placement")
    "creator-agent" (DeclaredAccess.readerRelation "configuration" "placement" "writer") true

theorem user_cannot_declare_readers_of_an_agent_placement_directly :
    let direct := PackPlacementReaders.install agentPlacement "user" [] ["writer"]
    PackPlacementReaders.readable direct "writer" = false := by native_decide

theorem placement_owner_applies_the_declaration_for_the_user :
    [PackPlacementReaders.readable (placementDeclared "user") "writer",
     PackPlacementReaders.readable (placementDeclared "outsider") "writer"] = [true, false] := by
  native_decide

/-! ### Workspace selection (`WorkspaceAndFileRequests.callbackSelectedAgentFixture`)

`workspace_selected_by` on an Agent has no manager relation: an Agent the
runtime created is selected only by the runtime. -/

def runtimeAgent : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "callback", "user"⟩,
      ⟨"agents", "target", "runtime"⟩],
    relations := [⟨"agents", "target", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "callback", "reader", .actor "creator"⟩],
    enrolled := [] }

def selection : Relation :=
  ⟨"agents", "target", "workspace_selected_by", .entitySet "configuration" "callback" "reader"⟩

def agentSelected (requester : String) : State :=
  ownerStaged runtimeAgent (mayRequest runtimeAgent requester "agents" "target") "runtime"
    selection true

theorem user_cannot_select_a_runtime_agent_directly :
    allowed 8 (apply runtimeAgent (.grantAs "user" selection)) "creator" "agents" "target" "read"
      = false := by native_decide

theorem runtime_selects_its_agent_for_the_configuring_user :
    [allowed 8 (agentSelected "user") "creator" "agents" "target" "read",
     allowed 8 (agentSelected "outsider") "creator" "agents" "target" "read"] = [true, false] := by
  native_decide

/-! ### Pairing readers (`Operations.pairingReaderFixture`)

Before its Node parent link a pairing observation has no reader manager, and
an enrolled administrator is never one. The runtime owns the observation and
grants its readers for an actor that may update the Node, as other
client-requested runtime operations are authorized. -/

def unlinkedPairing : State :=
  { emptyHome with objects := ⟨"pairing_observations", "route", "runtime"⟩ :: emptyHome.objects }

def routeReader : Relation := ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩

def pairingGranted (requester : String) : State :=
  ownerStaged unlinkedPairing (allowed 5 unlinkedPairing requester "nodes" "node" "update")
    "runtime" routeReader true

def pairingReadable (state : State) : Bool :=
  allowed 5 state "agent" "pairing_observations" "route" "read"

theorem node_operators_cannot_grant_unlinked_pairing_readers_directly :
    [pairingReadable (apply unlinkedPairing (.grantAs "user" routeReader)),
     pairingReadable (apply unlinkedPairing (.grantAs "admin" routeReader))] = [false, false] := by
  native_decide

theorem runtime_grants_pairing_readers_for_node_operators :
    [pairingReadable (pairingGranted "user"), pairingReadable (pairingGranted "admin"),
     pairingReadable (pairingGranted "outsider")] = [true, true, false] := by native_decide

def caseJson (path requester : String) (direct staged : Bool) : Lean.Json :=
  Lean.Json.mkObj [("path", Lean.toJson path), ("requester", Lean.toJson requester),
    ("direct", Lean.toJson direct), ("staged", Lean.toJson staged)]

/-- `direct`: the requester's own relationship change takes effect.
`staged`: the owner-staged change made for the requester takes effect. -/
def casesJson : String :=
  let cursorDirect (requester : String) :=
    cursorWritable (Automation.rebindEventCursor runtimeCursor requester "cursor" "agent" "sibling") "sibling"
  let placementDirect (requester : String) :=
    PackPlacementReaders.readable (PackPlacementReaders.install agentPlacement requester [] ["writer"]) "writer"
  let selectionDirect (requester : String) :=
    allowed 8 (apply runtimeAgent (.grantAs requester selection)) "creator" "agents" "target" "read"
  let pairingDirect (requester : String) :=
    pairingReadable (apply unlinkedPairing (.grantAs requester routeReader))
  (Lean.toJson (
    (["user", "outsider"].map fun r =>
      caseJson "event_cursor_rebind" r (cursorDirect r) (cursorWritable (cursorRebound r) "sibling")) ++
    (["user", "outsider"].map fun r =>
      caseJson "declared_placement_readers" r (placementDirect r)
        (PackPlacementReaders.readable (placementDeclared r) "writer")) ++
    (["user", "outsider"].map fun r =>
      caseJson "workspace_selection" r (selectionDirect r)
        (allowed 8 (agentSelected r) "creator" "agents" "target" "read")) ++
    (["user", "admin", "outsider"].map fun r =>
      caseJson "pairing_reader" r (pairingDirect r) (pairingReadable (pairingGranted r))))).compress

end Identity.OwnerStaged
