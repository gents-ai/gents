import Lean
import Proofs.Identity.Household
import Proofs.Identity.Automation
import Proofs.Identity.PackPlacementReaders
import Proofs.RuntimeReconcile.HttpAccess

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

def pairingResource : String := "pairing_observations"

/-- What the requester must natively hold before the owner stages a change for
it. `documentUpdate`: `update` on the changed document itself. `pairingNode`:
`update` on the runtime's own Node row, and only for a relation on a pairing
observation; a client-requested runtime operation is authorized that way while
the observation has no reader manager. The row is the single Node the runtime
reads for its own `node_did` in the same transaction (`none` when it reads no
row or several); the requester's view never chooses it. Native DefraDB keeps
`node_did @index(unique: true)` against every live document on a local write,
whatever the writer may read, but a replicated merge can keep a second,
unindexed row, so a duplicate refuses rather than picks one. -/
inductive Authority where
  | documentUpdate
  | pairingNode (node : Option String)
  deriving Repr

def authorized (state : State) (requester : DID) (relation : Relation) : Authority → Bool
  | .documentUpdate => mayRequest state requester relation.resource relation.id
  | .pairingNode none => false
  | .pairingNode (some node) =>
      relation.resource == pairingResource && allowed 5 state requester "nodes" node "update"

/-- The registered native owner of a live document that `reader` may read in
this transaction; none for a missing document or one it cannot read (native
`DocumentOwnerObservation` reports an owner only for a visible document). -/
def observedOwner (state : State) (reader resource id : String) : Option DID :=
  match state.objects.find? (fun object => object.resource == resource && object.id == id) with
  | some object => if allowed 8 state reader resource id "read" then some object.owner else none
  | none => none

/-- The owner is found only after authorization: as the requester observes it,
else as the runtime observes it. Both observe the same registered owner when
they observe one; the runtime's view lets an authorized requester change a
runtime-owned document it cannot read yet. -/
def foundOwner (state : State) (runtime requester resource id : String) : Option DID :=
  match observedOwner state requester resource id with
  | some owner => some owner
  | none => observedOwner state runtime resource id

/-- The owners the runtime stages as. An embedded configuration transaction
acts as the runtime or one of its Agents, so it may sign only as those; the
pairing authority stages only for observations the runtime itself owns.
`agentsOf runtime` is the runtime's own view: the Agent rows configured on its
Node, read as the runtime in the same transaction, like the owner it falls
back to and its Node row. The requester's read access never decides whether
an owner is one of the runtime's Agents. -/
def stagesAs (runtime : DID) (agentsOf : DID → List DID) (owner : DID) : Authority → Bool
  | .documentUpdate => owner == runtime || (agentsOf runtime).contains owner
  | .pairingNode _ => owner == runtime

inductive Outcome where
  | denied
  | unmanaged
  | direct
  | staged (owner : DID)
  deriving BEq, DecidableEq, Repr

def Outcome.name : Outcome → String
  | .denied => "denied"
  | .unmanaged => "unmanaged"
  | .direct => "direct"
  | .staged _ => "staged"

/-- The owner-staged request: authority first, then the owner. A requester that
owns the document changes it itself and native ACP decides. -/
def outcome (state : State) (runtime : DID) (agentsOf : DID → List DID) (requester : DID)
    (authority : Authority) (relation : Relation) : Outcome :=
  if !authorized state requester relation authority then .denied
  else match foundOwner state runtime requester relation.resource relation.id with
    | none => .unmanaged
    | some owner =>
      if owner == requester then .direct
      else if stagesAs runtime agentsOf owner authority then .staged owner
      else .unmanaged

def request (state : State) (runtime : DID) (agentsOf : DID → List DID) (requester : DID)
    (authority : Authority) (relation : Relation) (grant : Bool) : State :=
  match outcome state runtime agentsOf requester authority relation with
  | .direct => apply state (if grant then .grantAs requester relation else .revokeAs requester relation)
  | .staged owner => ownerStaged state true owner relation grant
  | _ => state

theorem staged_only_when_authorized (state : State) (runtime : DID) (agentsOf : DID → List DID)
    (requester : DID) (authority : Authority) (relation : Relation) (owner : DID)
    (h : outcome state runtime agentsOf requester authority relation = .staged owner) :
    authorized state requester relation authority = true := by
  unfold outcome at h
  cases hAuth : authorized state requester relation authority <;> simp_all

theorem staged_owner_is_runtime_or_its_agent (state : State) (runtime : DID)
    (agentsOf : DID → List DID) (requester : DID) (authority : Authority) (relation : Relation)
    (owner : DID) (h : outcome state runtime agentsOf requester authority relation = .staged owner) :
    owner = runtime ∨ owner ∈ agentsOf runtime := by
  unfold outcome at h
  split at h
  · cases h
  · split at h
    · cases h
    · rename_i found _
      split at h
      · cases h
      · split at h
        · rename_i managed
          cases h
          cases authority <;> simp_all [stagesAs]
        · cases h

/-- The pairing authority stages only on a pairing observation the runtime owns,
through the runtime's own Node row. -/
theorem pairing_stages_only_runtime_owned_pairing_observations (state : State) (runtime : DID)
    (agentsOf : DID → List DID) (requester : DID) (node : Option String) (relation : Relation)
    (owner : DID)
    (h : outcome state runtime agentsOf requester (.pairingNode node) relation = .staged owner) :
    owner = runtime ∧ relation.resource = pairingResource ∧ node.isSome := by
  have hAuth := staged_only_when_authorized state runtime agentsOf requester _ relation owner h
  unfold outcome at h
  split at h
  · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · split at h
        · cases h
          cases node <;> simp_all [authorized, stagesAs]
        · cases h

/-- Non-interference: an unauthorized requester is refused before any owner is
found, so whether the document exists and who owns it change neither the
outcome nor the state. -/
theorem unauthorized_refusal_independent_of_document (state state' : State) (runtime : DID)
    (agentsOf : DID → List DID) (requester : DID) (authority : Authority) (relation : Relation)
    (grant : Bool)
    (refused : authorized state requester relation authority = false)
    (refused' : authorized state' requester relation authority = false) :
    outcome state runtime agentsOf requester authority relation =
        outcome state' runtime agentsOf requester authority relation ∧
      outcome state runtime agentsOf requester authority relation = .denied ∧
      request state runtime agentsOf requester authority relation grant = state := by
  simp [outcome, request, refused, refused']

/-- Agents configured on each runtime's Node in these fixtures. -/
def agentsOf : DID → List DID
  | "runtime" => ["creator-agent"]
  | _ => []

def emptyHome : State :=
  { objects := [⟨"nodes", "node", "user"⟩],
    relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩], enrolled := [] }

/-! ### Event cursor rebinding (`Automation.rebindEventCursor`)

A cursor seeded by the runtime is owned by the runtime. Its `writer` has no
manager relation, so only that creator may rebind it. -/

def runtimeCursor : State :=
  Automation.publishEventCursor emptyHome "runtime" "cursor" "node" "agent" true true

def cursorRebound (requester : String) : State :=
  let revoked := request runtimeCursor "runtime" agentsOf requester .documentUpdate
    ⟨"configuration", "cursor", "writer", .actor "agent"⟩ false
  request revoked "runtime" agentsOf requester .documentUpdate
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
  request agentPlacement "runtime" agentsOf requester .documentUpdate
    (DeclaredAccess.readerRelation "configuration" "placement" "writer") true

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
  request runtimeAgent "runtime" agentsOf requester .documentUpdate selection true

theorem user_cannot_select_a_runtime_agent_directly :
    allowed 8 (apply runtimeAgent (.grantAs "user" selection)) "creator" "agents" "target" "read"
      = false := by native_decide

theorem runtime_selects_its_agent_for_the_configuring_user :
    [allowed 8 (agentSelected "user") "creator" "agents" "target" "read",
     allowed 8 (agentSelected "outsider") "creator" "agents" "target" "read"] = [true, false] := by
  native_decide

/-- A document owned by neither the runtime nor one of its Agents is never
staged, even for a requester that may update it. -/
theorem unmanaged_owner_is_not_staged :
    let foreign : State := { agentPlacement with
      objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "placement", "stranger"⟩] }
    outcome foreign "runtime" agentsOf "user" .documentUpdate
      (DeclaredAccess.readerRelation "configuration" "placement" "writer") = .unmanaged := by
  native_decide

/-! ### Pairing readers (`Operations.pairingReaderFixture`)

Before its Node parent link a pairing observation has no reader manager, and
an enrolled administrator is never one. The runtime owns the observation and
grants its readers for an actor that may update the runtime's own Node, as
other client-requested runtime operations are authorized. That authority
reaches only pairing observations the runtime owns: a missing observation, one
another actor owns, an administrator of a different Node and any other
collection are refused. -/

def unlinkedPairing : State :=
  { objects := [⟨"pairing_observations", "route", "runtime"⟩,
      ⟨"pairing_observations", "foreign", "stranger"⟩,
      ⟨"nodes", "node", "user"⟩, ⟨"nodes", "other-node", "other-user"⟩],
    relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
      ⟨"nodes", "other-node", "administrator", .actor "other-admin"⟩,
      ⟨"pairing_observations", "foreign", "route_reader", .actor "runtime"⟩],
    enrolled := [] }

/-- The runtime's own Node row, as it reads it for its `node_did`. -/
def runtimeNode : Authority := .pairingNode (some "node")

def readerOn (document : String) : Relation :=
  ⟨"pairing_observations", document, "route_reader", .actor "agent"⟩

def routeReader : Relation := readerOn "route"

def pairingGranted (requester : String) : State :=
  request unlinkedPairing "runtime" agentsOf requester runtimeNode routeReader true

def pairingReadable (state : State) : Bool :=
  allowed 5 state "agent" "pairing_observations" "route" "read"

theorem node_operators_cannot_grant_unlinked_pairing_readers_directly :
    [pairingReadable (apply unlinkedPairing (.grantAs "user" routeReader)),
     pairingReadable (apply unlinkedPairing (.grantAs "admin" routeReader))] = [false, false] := by
  native_decide

theorem runtime_grants_pairing_readers_for_node_operators :
    [pairingReadable (pairingGranted "user"), pairingReadable (pairingGranted "admin"),
     pairingReadable (pairingGranted "outsider")] = [true, true, false] := by native_decide

def pairingOutcome (requester document : String) : Outcome :=
  outcome unlinkedPairing "runtime" agentsOf requester runtimeNode (readerOn document)

theorem pairing_authority_reaches_only_runtime_owned_observations :
    [pairingOutcome "admin" "missing", pairingOutcome "admin" "foreign",
     pairingOutcome "other-admin" "route",
     outcome unlinkedPairing "runtime" agentsOf "admin" runtimeNode
       ⟨"configuration", "route", "writer", .actor "agent"⟩,
     outcome unlinkedPairing "runtime" agentsOf "admin" (.pairingNode none) routeReader] =
    [.unmanaged, .unmanaged, .denied, .denied, .denied] := by native_decide

theorem pairing_refusal_ignores_the_observation :
    [pairingOutcome "outsider" "missing", pairingOutcome "outsider" "route",
     pairingOutcome "outsider" "foreign", pairingOutcome "other-admin" "missing",
     pairingOutcome "other-admin" "foreign"] =
    [.denied, .denied, .denied, .denied, .denied] := by native_decide

/-! ### Transport independence of pairing readers

A pairing reader is requested either in the requester's own configuration
transaction on the runtime's node (`embedded`) or through the runtime's signed
pairing-reader route (`RuntimeReconcile.HttpAccess.decideCommand`). The route
never sends the requester's own relationship change to native ACP; once it
admits the command it opens an embedded transaction as the signer and decides
it with the same `outcome`. So the result depends only on who requests what,
never on the transport. -/

inductive PairingResponse where
  | refused (decision : RuntimeReconcile.HttpAccess.CommandDecision)
  | decided (result : Outcome)
  deriving BEq, DecidableEq, Repr

def pairingEmbedded (state : State) (runtime : DID) (agentsOf : DID → List DID)
    (requester : DID) (node : Option String) (relation : Relation) : Outcome :=
  outcome state runtime agentsOf requester (.pairingNode node) relation

def pairingOverHttp (authorityMatches : Bool) (caller : Option DID) (signer : DID)
    (signatureValid : Bool) (state : State) (runtime : DID) (agentsOf : DID → List DID)
    (node : Option String) (relation : Relation) : PairingResponse :=
  match RuntimeReconcile.HttpAccess.decideCommand authorityMatches caller signer signatureValid with
  | .executes => .decided (pairingEmbedded state runtime agentsOf signer node relation)
  | refusal => .refused refusal

/-- For every requester and observation, an admitted command decides exactly
what the requester's embedded transaction decides. -/
theorem pairing_outcome_is_transport_independent (state : State) (runtime : DID)
    (agentsOf : DID → List DID) (requester : DID) (node : Option String) (relation : Relation) :
    pairingOverHttp true (some requester) requester true state runtime agentsOf node relation =
      .decided (pairingEmbedded state runtime agentsOf requester node relation) := by
  simp [pairingOverHttp, RuntimeReconcile.HttpAccess.decideCommand]

/-- The route decides only for its authenticated signer, and then exactly as
that signer's embedded request. -/
theorem pairing_route_decides_only_for_its_signer (authorityMatches : Bool) (caller : Option DID)
    (signer : DID) (signatureValid : Bool) (state : State) (runtime : DID)
    (agentsOf : DID → List DID) (node : Option String) (relation : Relation) (result : Outcome)
    (h : pairingOverHttp authorityMatches caller signer signatureValid state runtime agentsOf node
      relation = .decided result) :
    caller = some signer ∧ result = pairingEmbedded state runtime agentsOf signer node relation := by
  unfold pairingOverHttp at h
  split at h
  · rename_i executes
    cases h
    exact ⟨(RuntimeReconcile.HttpAccess.command_executes_only_for_its_authenticated_signer
      _ _ _ _ executes).2.1, rfl⟩
  · cases h

/-- No existence oracle over HTTP either: an admitted requester that may not
request the change gets the same `denied` whether the observation exists and
whoever owns it. -/
theorem pairing_route_refusal_independent_of_observation (state state' : State) (runtime : DID)
    (agentsOf : DID → List DID) (requester : DID) (node : Option String) (relation : Relation)
    (refused : authorized state requester relation (.pairingNode node) = false)
    (refused' : authorized state' requester relation (.pairingNode node) = false) :
    pairingOverHttp true (some requester) requester true state runtime agentsOf node relation =
        pairingOverHttp true (some requester) requester true state' runtime agentsOf node relation ∧
      pairingOverHttp true (some requester) requester true state runtime agentsOf node relation =
        .decided .denied := by
  simp [pairing_outcome_is_transport_independent, pairingEmbedded, outcome, refused, refused']

def pairingRequesters : List String := ["user", "admin", "outsider", "other-admin"]
def pairingDocuments : List String := ["route", "missing", "foreign"]

def pairingHttp (requester document : String) : PairingResponse :=
  pairingOverHttp true (some requester) requester true unlinkedPairing "runtime" agentsOf
    (some "node") (readerOn document)

theorem pairing_fixture_agrees_across_transports :
    (pairingRequesters.all fun requester => pairingDocuments.all fun document =>
      pairingHttp requester document == .decided (pairingOutcome requester document)) = true := by
  native_decide

/-- Route refusals precede the observation: a bearer that is not the signer,
an anonymous caller and an invalid signature are refused for the Node owner
too, and a foreign authority is misdirected. -/
theorem pairing_route_refuses_unauthenticated_commands :
    [pairingOverHttp true (some "admin") "user" true unlinkedPairing "runtime" agentsOf
        (some "node") routeReader,
     pairingOverHttp true none "user" true unlinkedPairing "runtime" agentsOf
        (some "node") routeReader,
     pairingOverHttp true (some "user") "user" false unlinkedPairing "runtime" agentsOf
        (some "node") routeReader,
     pairingOverHttp false (some "user") "user" true unlinkedPairing "runtime" agentsOf
        (some "node") routeReader] =
    [.refused .forbidden, .refused .forbidden, .refused .forbidden, .refused .misdirected] := by
  native_decide

def PairingResponse.name : PairingResponse → String
  | .refused .misdirected => "misdirected"
  | .refused .forbidden => "forbidden"
  | .refused .executes => "executes"
  | .decided result => result.name

def caseJson (path requester : String) (direct staged : Bool) (result : Outcome) : Lean.Json :=
  Lean.Json.mkObj [("path", Lean.toJson path), ("requester", Lean.toJson requester),
    ("direct", Lean.toJson direct), ("staged", Lean.toJson staged),
    ("outcome", Lean.toJson result.name)]

/-- One pairing case per requester and observation, shared by the contract
snapshot and the runtime route's generator. -/
def pairingCaseJson (requester document : String) : Lean.Json :=
  let relation := readerOn document
  let granted := request unlinkedPairing "runtime" agentsOf requester runtimeNode relation true
  Lean.Json.mkObj [("path", Lean.toJson "pairing_reader"), ("requester", Lean.toJson requester),
    ("document", Lean.toJson document),
    ("direct", Lean.toJson (mayManage unlinkedPairing requester relation)),
    ("staged", Lean.toJson (allowed 5 granted "agent" "pairing_observations" document "read" &&
      !allowed 5 unlinkedPairing "agent" "pairing_observations" document "read")),
    ("outcome", Lean.toJson (pairingOutcome requester document).name),
    ("http", Lean.toJson (pairingHttp requester document).name)]

def pairingCasesJson : List Lean.Json :=
  pairingDocuments.flatMap fun d => pairingRequesters.map fun r => pairingCaseJson r d

/-- `direct`: the requester's own relationship change takes effect.
`staged`: the owner-staged change made for the requester takes effect.
`outcome`: how the owner-staged request is decided. Pairing cases name the
observation: `route` is runtime-owned, `foreign` is owned by another actor,
`missing` does not exist; `http` is how the runtime's pairing-reader route
answers the same requester's admitted command, and the embedded and route
results are replayed against the same case. `direct` stays the native public
route's own answer, which no Gents pairing path relies on. -/
def casesJson : String :=
  let cursorDirect (requester : String) :=
    cursorWritable (Automation.rebindEventCursor runtimeCursor requester "cursor" "agent" "sibling") "sibling"
  let placementDirect (requester : String) :=
    PackPlacementReaders.readable (PackPlacementReaders.install agentPlacement requester [] ["writer"]) "writer"
  let selectionDirect (requester : String) :=
    allowed 8 (apply runtimeAgent (.grantAs requester selection)) "creator" "agents" "target" "read"
  let cursorOutcome (requester : String) :=
    outcome runtimeCursor "runtime" agentsOf requester .documentUpdate
      ⟨"configuration", "cursor", "writer", .actor "sibling"⟩
  (Lean.toJson (
    (["user", "outsider"].map fun r =>
      caseJson "event_cursor_rebind" r (cursorDirect r) (cursorWritable (cursorRebound r) "sibling")
        (cursorOutcome r)) ++
    (["user", "outsider"].map fun r =>
      caseJson "declared_placement_readers" r (placementDirect r)
        (PackPlacementReaders.readable (placementDeclared r) "writer")
        (outcome agentPlacement "runtime" agentsOf r .documentUpdate
          (DeclaredAccess.readerRelation "configuration" "placement" "writer"))) ++
    (["user", "outsider"].map fun r =>
      caseJson "workspace_selection" r (selectionDirect r)
        (allowed 8 (agentSelected r) "creator" "agents" "target" "read")
        (outcome runtimeAgent "runtime" agentsOf r .documentUpdate selection)) ++
    pairingCasesJson)).compress

end Identity.OwnerStaged
