import Proofs.Session.Hooks.Claim
import Proofs.Session.Hooks.Grant
import Proofs.Session.Hooks.MustCommit
import Proofs.Session.Hooks.OwedDelivery
import Proofs.Session.Hooks.Prepared
import Proofs.Automation.Hooks.QueueScope
import Proofs.Automation.Hooks.GoalResume
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.HooksContracts
open Lean Hooks
open Conformance.Contracts (jsonString jsonArray jsonStringArray)

abbrev HookEffect := Effect String String String
abbrev HookRow := Row String String
abbrev HookState := State (List String) String String
abbrev Record := Delivery.State String String

def caller : Actor := "did:key:z6MkLifecycleCaller"

/-- A concrete transaction: rows of plugin-owned collections, core state as the
list of operations core performed, the document ACP denials for `caller`, and
the core operations whose `perform` refuses. -/
structure World where
  rows : List HookRow
  core : List String := ["claimed"]
  denied : List (Access × Ref String) := []
  failingOps : List String := []

def World.state (world : World) : HookState := ⟨world.core, world.rows⟩

def World.allowed (world : World) : Acp String :=
  fun _ access ref => !world.denied.contains (access, ref)

def World.perform (world : World) : Actor → String → List String → Option (List String) :=
  fun _ op core => if world.failingOps.contains op then none else some (core ++ [op])

def World.host (world : World) : Host (List String) String String String String :=
  { select := fun _ docs collection => docs.filter fun row => row.ref.collection == collection
  , perform := world.perform
  , allowed := world.allowed }

/-- A scripted plugin decision. `byShown` chooses between two results by the
number of rows the plugin was shown across its declared reads. -/
inductive Decision (V : Type) where
  | refuse (reason : String)
  | proceed (value : V) (effects : List HookEffect)
  | byShown (count : Nat) (matching otherwise : V × List HookEffect)

def Decision.outcome {V : Type} :
    Decision V → List (List HookRow) → Outcome V String String String
  | .refuse reason, _ => .refuse reason
  | .proceed value effects, _ => .proceed value effects
  | .byShown count matching otherwise, rows =>
      let chosen := if (rows.map List.length).sum == count then matching else otherwise
      .proceed chosen.1 chosen.2

/-- A scripted plugin: a manifest, the collections it reads, and its decision. -/
structure Script (V : Type) where
  name : String
  owns : List String
  bound : Nat
  reads : List String := []
  decision : Decision V

def Script.manifest {V : Type} (script : Script V) : Manifest :=
  ⟨script.name, script.owns, script.bound⟩

def Script.plugin {V : Type} (script : Script V) :
    Plugin String Unit V String String String :=
  { manifest := script.manifest
  , hook :=
      { reads := fun _ => script.reads
      , decide := fun _ rows => script.decision.outcome rows } }

def Script.deciding {V : Type} (script : Script V) (decision : Decision V) : Script V :=
  { script with decision := decision }

private def ref (collection doc : String) : Ref String := ⟨collection, doc⟩

private def row (collection doc : String) (revision : Nat) (value : String) : HookRow :=
  ⟨ref collection doc, revision, value⟩

private def create (collection doc value : String) : HookEffect :=
  .write (.create (ref collection doc) value)

private def update (collection doc : String) (expected : Nat) (value : String) : HookEffect :=
  .write (.update (ref collection doc) expected value)

private def delete (collection doc : String) (expected : Nat) : HookEffect :=
  .write (.delete (ref collection doc) expected)

private def coreOp (name : String) : HookEffect := .core name

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def refFields (target : Ref String) : String :=
  "\"collection\":" ++ jsonString target.collection ++ ",\"doc\":" ++ jsonString target.doc

private def rowJson (entry : HookRow) : String :=
  "{" ++ refFields entry.ref ++ ",\"revision\":" ++ toString entry.revision ++
    ",\"value\":" ++ jsonString entry.value ++ "}"

private def accessString : Access → String
  | .create => "create"
  | .update => "update"
  | .delete => "delete"

private def effectJson : HookEffect → String
  | .write (.create target value) =>
      "{\"kind\":\"create\"," ++ refFields target ++ ",\"value\":" ++ jsonString value ++ "}"
  | .write (.update target expected value) =>
      "{\"kind\":\"update\"," ++ refFields target ++ ",\"expected\":" ++ toString expected ++
        ",\"value\":" ++ jsonString value ++ "}"
  | .write (.delete target expected) =>
      "{\"kind\":\"delete\"," ++ refFields target ++ ",\"expected\":" ++ toString expected ++ "}"
  | .core op => "{\"kind\":\"core\",\"op\":" ++ jsonString op ++ "}"

private def effectsJson (effects : List HookEffect) : String :=
  jsonArray (effects.map effectJson)

private def stateJson (state : HookState) : String :=
  "{\"core\":" ++ jsonStringArray state.core ++
    ",\"rows\":" ++ jsonArray (state.docs.map rowJson) ++ "}"

private def worldJson (world : World) : String :=
  "{\"rows\":" ++ jsonArray (world.rows.map rowJson) ++
    ",\"core\":" ++ jsonStringArray world.core ++
    ",\"denied\":" ++ jsonArray (world.denied.map fun entry =>
      "{\"access\":" ++ jsonString (accessString entry.1) ++ "," ++ refFields entry.2 ++ "}") ++
    ",\"failing_ops\":" ++ jsonStringArray world.failingOps ++ "}"

private def manifestJson (manifest : Manifest) : String :=
  "{\"name\":" ++ jsonString manifest.name ++
    ",\"owns\":" ++ jsonStringArray manifest.owns ++
    ",\"bound\":" ++ toString manifest.bound ++ "}"

private def refusalJson : Refusal → String
  | .core => "{\"kind\":\"core\"}"
  | .refused plugin reason =>
      "{\"kind\":\"refused\",\"plugin\":" ++ jsonString plugin ++
        ",\"reason\":" ++ jsonString reason ++ "}"
  | .invalid plugin => "{\"kind\":\"invalid\",\"plugin\":" ++ jsonString plugin ++ "}"
  | .conflict plugin => "{\"kind\":\"conflict\",\"plugin\":" ++ jsonString plugin ++ "}"

private def choiceJson {V : Type} (valueJson : V → String) (choice : V × List HookEffect) :
    String :=
  "{\"value\":" ++ valueJson choice.1 ++ ",\"effects\":" ++ effectsJson choice.2 ++ "}"

private def decisionJson {V : Type} (valueJson : V → String) : Decision V → String
  | .refuse reason => "{\"kind\":\"refuse\",\"reason\":" ++ jsonString reason ++ "}"
  | .proceed value effects =>
      "{\"kind\":\"proceed\",\"value\":" ++ valueJson value ++
        ",\"effects\":" ++ effectsJson effects ++ "}"
  | .byShown count matching otherwise =>
      "{\"kind\":\"by_shown\",\"count\":" ++ toString count ++
        ",\"matching\":" ++ choiceJson valueJson matching ++
        ",\"otherwise\":" ++ choiceJson valueJson otherwise ++ "}"

private def scriptJson {V : Type} (valueJson : V → String) (script : Script V) : String :=
  "{\"name\":" ++ jsonString script.name ++
    ",\"owns\":" ++ jsonStringArray script.owns ++
    ",\"bound\":" ++ toString script.bound ++
    ",\"reads\":" ++ jsonStringArray script.reads ++
    ",\"decision\":" ++ decisionJson valueJson script.decision ++ "}"

private def verdictJson : ClaimVerdict → String
  | .allow => "{\"kind\":\"allow\"}"
  | .leaveQueued => "{\"kind\":\"leave_queued\"}"
  | .retire reason successor =>
      "{\"kind\":\"retire\",\"reason\":" ++ jsonString reason ++
        ",\"successor\":{\"request_id\":" ++ jsonString successor.request ++
        ",\"request_doc_id\":" ++ jsonString successor.doc ++ "}}"
  | .reject reason => "{\"kind\":\"reject\",\"reason\":" ++ jsonString reason ++ "}"

/-! Effect validation and application: `Hooks.valid` and `Hooks.commit`. -/

structure EffectCase where
  name : String
  world : World
  manifest : Manifest
  effects : List HookEffect

def EffectCase.accepted (c : EffectCase) : Bool :=
  Hooks.valid c.manifest c.world.allowed caller c.effects

def EffectCase.committed (c : EffectCase) : Option HookState :=
  Hooks.commit c.manifest c.world.allowed c.world.perform caller c.effects c.world.state

private def notes : Manifest := ⟨"notes", ["Note"], 3⟩

private def effectWorld : World :=
  { rows := [row "Note" "n1" 2 "old", row "Other" "o1" 0 "foreign"]
  , denied := [(.create, ref "Note" "n9"), (.delete, ref "Note" "n1")]
  , failingOps := ["explode"] }

def effectCases : List EffectCase :=
  [ { name := "within_bound_applies_in_order", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "fresh", update "Note" "n1" 2 "new"] }
  , { name := "exactly_at_bound", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", create "Note" "n3" "b", coreOp "notes.touch"] }
  , { name := "over_bound_is_invalid", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", create "Note" "n3" "b", create "Note" "n4" "c",
                  create "Note" "n5" "d"] }
  , { name := "core_operations_count_toward_the_bound", world := effectWorld
    , manifest := ⟨"notes", ["Note"], 0⟩, effects := [coreOp "notes.touch"] }
  , { name := "empty_result_is_valid_and_changes_nothing", world := effectWorld
    , manifest := notes, effects := [] }
  , { name := "write_to_owned_collection", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "owned"] }
  , { name := "write_to_foreign_collection_is_invalid", world := effectWorld, manifest := notes
    , effects := [update "Other" "o1" 0 "taken"] }
  , { name := "one_foreign_write_invalidates_the_result", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "owned", create "Other" "o2" "taken"] }
  , { name := "acp_denied_create_is_invalid", world := effectWorld, manifest := notes
    , effects := [create "Note" "n9" "denied"] }
  , { name := "acp_denies_one_access_not_another", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "allowed"] }
  , { name := "acp_denied_delete_is_invalid", world := effectWorld, manifest := notes
    , effects := [delete "Note" "n1" 2] }
  , { name := "core_operations_only", world := effectWorld, manifest := notes
    , effects := [coreOp "request.supersede", coreOp "fire.mark_applied"] }
  , { name := "failed_core_operation_commits_nothing", world := effectWorld, manifest := notes
    , effects := [create "Note" "n2" "a", coreOp "explode"] }
  , { name := "create_of_existing_document_misses", world := effectWorld, manifest := notes
    , effects := [create "Note" "n1" "duplicate"] }
  , { name := "update_with_held_revision", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "new"] }
  , { name := "update_with_stale_revision_misses", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 1 "new"] }
  , { name := "update_of_absent_document_misses", world := effectWorld, manifest := notes
    , effects := [update "Note" "n7" 0 "new"] }
  , { name := "delete_with_held_revision"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 2] }
  , { name := "delete_with_stale_revision_misses"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 3] }
  , { name := "create_then_update_of_one_document_is_invalid", world := effectWorld
    , manifest := notes, effects := [create "Note" "n2" "a", update "Note" "n2" 0 "b"] }
  , { name := "update_then_create_of_one_document_is_invalid", world := effectWorld
    , manifest := notes, effects := [update "Note" "n2" 0 "b", create "Note" "n2" "a"] }
  , { name := "two_updates_of_one_document_are_invalid", world := effectWorld, manifest := notes
    , effects := [update "Note" "n1" 2 "b", update "Note" "n1" 3 "c"] }
  , { name := "delete_then_create_of_one_document_is_invalid"
    , world := { effectWorld with denied := [] }, manifest := notes
    , effects := [delete "Note" "n1" 2, create "Note" "n1" "again"] }
  , { name := "core_operations_apply_in_returned_order", world := effectWorld
    , manifest := notes, effects := [coreOp "second", coreOp "first"] }
  , { name := "writes_to_distinct_documents_check_against_the_snapshot", world := effectWorld
    , manifest := notes, effects := [update "Note" "n1" 2 "b", create "Note" "n2" "c"] }
  , { name := "one_stale_write_among_distinct_documents_misses", world := effectWorld
    , manifest := notes, effects := [create "Note" "n2" "c", update "Note" "n1" 1 "b"] } ]

structure RowWire where
  collection : String
  doc : String
  revision : Nat
  value : String
  deriving ToJson

def RowWire.ofModel (entry : HookRow) : RowWire :=
  ⟨entry.ref.collection, entry.ref.doc, entry.revision, entry.value⟩

def RowWire.toModel (wire : RowWire) : HookRow :=
  row wire.collection wire.doc wire.revision wire.value

structure EffectWire where
  kind : String
  collection : Option String
  doc : Option String
  expected : Option Nat
  value : Option String
  op : Option String
  deriving ToJson

def EffectWire.ofModel : HookEffect → EffectWire
  | .write (.create target value) =>
      ⟨"create", some target.collection, some target.doc, none, some value, none⟩
  | .write (.update target expected value) =>
      ⟨"update", some target.collection, some target.doc, some expected, some value, none⟩
  | .write (.delete target expected) =>
      ⟨"delete", some target.collection, some target.doc, some expected, none, none⟩
  | .core op => ⟨"core", none, none, none, none, some op⟩

def EffectWire.toModel (wire : EffectWire) : HookEffect :=
  let target := ref (wire.collection.getD "") (wire.doc.getD "")
  match wire.kind with
  | "core" => .core (wire.op.getD "")
  | "create" => .write (.create target (wire.value.getD ""))
  | "update" => .write (.update target (wire.expected.getD 0) (wire.value.getD ""))
  | _ => .write (.delete target (wire.expected.getD 0))

def accessOf : String → Access
  | "update" => .update
  | "delete" => .delete
  | _ => .create

structure DenialWire where
  access : String
  collection : String
  doc : String
  deriving ToJson

structure WorldWire where
  rows : List RowWire
  core : List String
  denied : List DenialWire
  failingOps : List String
  deriving ToJson

def WorldWire.ofModel (world : World) : WorldWire :=
  { rows := world.rows.map RowWire.ofModel, core := world.core
  , denied := world.denied.map fun entry =>
      ⟨accessString entry.1, entry.2.collection, entry.2.doc⟩
  , failingOps := world.failingOps }

def WorldWire.toModel (wire : WorldWire) : World :=
  { rows := wire.rows.map RowWire.toModel, core := wire.core
  , denied := wire.denied.map fun entry => (accessOf entry.access, ref entry.collection entry.doc)
  , failingOps := wire.failingOps }

structure StateWire where
  core : List String
  rows : List RowWire
  deriving ToJson

def StateWire.ofModel (state : HookState) : StateWire :=
  ⟨state.core, state.docs.map RowWire.ofModel⟩

structure ManifestWire where
  name : String
  owns : List String
  bound : Nat
  deriving ToJson

def ManifestWire.ofModel (manifest : Manifest) : ManifestWire :=
  ⟨manifest.name, manifest.owns, manifest.bound⟩

def ManifestWire.toModel (wire : ManifestWire) : Manifest :=
  ⟨wire.name, wire.owns, wire.bound⟩

structure EffectInput where
  name : String
  world : WorldWire
  manifest : ManifestWire
  effects : List EffectWire
  deriving ToJson

structure EffectOutput where
  valid : Bool
  committed : Option StateWire
  deriving ToJson

@[conformance_family]
def effectCommit : Family EffectInput EffectOutput where
  inputs := effectCases.map fun c =>
    { name := c.name, world := WorldWire.ofModel c.world
    , manifest := ManifestWire.ofModel c.manifest, effects := c.effects.map EffectWire.ofModel }
  model := fun i =>
    let c : EffectCase :=
      { name := i.name, world := i.world.toModel, manifest := i.manifest.toModel
      , effects := i.effects.map EffectWire.toModel }
    { valid := c.accepted, committed := c.committed.map StateWire.ofModel }

/-! Create grants: `Hooks.grantsValid` (decision 2026-11-11, maintainer-approved). -/

structure GrantCase where
  name : String
  declared : List Grantable := sessionChildGrants "Goal"
  named : Named String := ⟨["did:key:z6MkSessionAgent"], [ref "AgentSession" "s1"]⟩
  effects : List HookEffect := [create "Goal" "g1" "goal"]
  grants : List (Grant String)

def GrantCase.valid (c : GrantCase) : Bool :=
  Hooks.grantsValid c.declared caller c.named c.effects c.grants

private def parentGrant (target session : String) : Grant String :=
  ⟨ref "Goal" target, "parent", .document (ref "AgentSession" session)⟩

private def agentGrant (target did : String) : Grant String :=
  ⟨ref "Goal" target, "working_agent", .actor did⟩

def grantCases : List GrantCase :=
  [ { name := "no_grants_are_valid", grants := [] }
  , { name := "session_child_grants_on_the_created_goal"
    , grants := [parentGrant "g1" "s1", agentGrant "g1" caller] }
  , { name := "working_agent_to_an_actor_the_event_names"
    , grants := [agentGrant "g1" "did:key:z6MkSessionAgent"] }
  , { name := "grant_on_a_document_the_result_does_not_create"
    , grants := [parentGrant "g2" "s1"] }
  , { name := "grant_on_an_updated_document_is_not_a_create"
    , effects := [update "Goal" "g1" 0 "goal"], grants := [agentGrant "g1" caller] }
  , { name := "working_agent_to_an_unnamed_actor"
    , grants := [agentGrant "g1" "did:key:z6MkStranger"] }
  , { name := "parent_to_an_unnamed_session"
    , grants := [parentGrant "g1" "s9"] }
  , { name := "parent_to_a_named_document_of_another_collection"
    , named := ⟨[], [ref "AgentSession" "s1", ref "Agent" "a1"]⟩
    , grants := [⟨ref "Goal" "g1", "parent", .document (ref "Agent" "a1")⟩] }
  , { name := "undeclared_reader_relation"
    , grants := [⟨ref "Goal" "g1", "reader", .actor caller⟩] }
  , { name := "working_agent_to_a_document"
    , grants := [⟨ref "Goal" "g1", "working_agent", .document (ref "AgentSession" "s1")⟩] }
  , { name := "relations_are_declared_per_collection"
    , effects := [create "Note" "n1" "note"]
    , grants := [⟨ref "Note" "n1", "working_agent", .actor caller⟩] }
  , { name := "one_invalid_grant_invalidates_the_result"
    , grants := [parentGrant "g1" "s1", agentGrant "g1" "did:key:z6MkStranger"] }
  , { name := "grants_on_two_created_documents"
    , effects := [create "Goal" "g1" "a", create "Goal" "g2" "b"]
    , grants := [agentGrant "g1" caller, parentGrant "g2" "s1"] } ]

structure RefWire where
  collection : String
  doc : String
  deriving ToJson

def RefWire.ofModel (target : Ref String) : RefWire := ⟨target.collection, target.doc⟩

def RefWire.toModel (wire : RefWire) : Ref String := ref wire.collection wire.doc

structure SubjectKindWire where
  kind : String
  resource : Option String
  collection : Option String
  deriving ToJson

structure GrantableWire where
  collection : String
  relation : String
  subject : SubjectKindWire
  deriving ToJson

def GrantableWire.ofModel (entry : Grantable) : GrantableWire :=
  { collection := entry.collection, relation := entry.relation
  , subject := match entry.subject with
      | .actor => ⟨"actor", none, none⟩
      | .document resource collection => ⟨"document", some resource, some collection⟩ }

def GrantableWire.toModel (wire : GrantableWire) : Grantable :=
  { collection := wire.collection, relation := wire.relation
  , subject := match wire.subject.kind, wire.subject.resource, wire.subject.collection with
      | "document", some resource, some collection => .document resource collection
      | _, _, _ => .actor }

structure SubjectWire where
  kind : String
  did : Option String
  collection : Option String
  doc : Option String
  deriving ToJson

structure GrantWire where
  target : RefWire
  relation : String
  subject : SubjectWire
  deriving ToJson

def GrantWire.ofModel (grant : Grant String) : GrantWire :=
  { target := RefWire.ofModel grant.target, relation := grant.relation
  , subject := match grant.subject with
      | .actor did => ⟨"actor", some did, none, none⟩
      | .document target => ⟨"document", none, some target.collection, some target.doc⟩ }

def GrantWire.toModel (wire : GrantWire) : Grant String :=
  { target := wire.target.toModel, relation := wire.relation
  , subject := match wire.subject.kind with
      | "document" =>
          .document (ref (wire.subject.collection.getD "") (wire.subject.doc.getD ""))
      | _ => .actor (wire.subject.did.getD "") }

structure GrantInput where
  name : String
  declared : List GrantableWire
  actor : String
  namedActors : List String
  namedDocs : List RefWire
  effects : List EffectWire
  grants : List GrantWire
  deriving ToJson

structure GrantOutput where
  valid : Bool
  deriving ToJson

def GrantCase.input (c : GrantCase) : GrantInput :=
  { name := c.name, declared := c.declared.map GrantableWire.ofModel, actor := caller
  , namedActors := c.named.actors, namedDocs := c.named.docs.map RefWire.ofModel
  , effects := c.effects.map EffectWire.ofModel, grants := c.grants.map GrantWire.ofModel }

def grantModel (i : GrantInput) : GrantOutput :=
  { valid := Hooks.grantsValid (i.declared.map GrantableWire.toModel) i.actor
      ⟨i.namedActors, i.namedDocs.map RefWire.toModel⟩ (i.effects.map EffectWire.toModel)
      (i.grants.map GrantWire.toModel) }

/-- Whether every grant of the case targets a document its effects create. -/
def GrantCase.placed (c : GrantCase) : Bool :=
  c.grants.all fun grant => (created c.effects).contains grant.target

/-- A native grant rides on the create of its target (`Write::Create.grants`),
so only a result whose every grant targets a document it creates has a
production form. -/
@[conformance_family]
def grantValidity : Family GrantInput GrantOutput where
  inputs := (grantCases.filter GrantCase.placed).map GrantCase.input
  model := grantModel

/-- model-production-gap: production has no form for a grant on a document
the result does not create (a grant is a field of the create that carries
it), so these cases are invalid by construction there and are checked only
in the model. -/
@[conformance_family]
def grantValidityUnreached : Family GrantInput GrantOutput where
  inputs := (grantCases.filter fun c => !c.placed).map GrantCase.input
  model := grantModel

example : (grantCases.filter fun c => !c.placed).all (fun c => !c.valid) = true := by
  native_decide

/-! Staging at refusable points: `Hooks.stage` and `Hooks.settle`. -/

private def stageWorld : World :=
  { rows := [row "Note" "n1" 0 "a", row "Tag" "t1" 1 "x"]
  , denied := [(.update, ref "Tag" "locked")]
  , failingOps := ["explode"] }

private def notesPlugin : Script String :=
  { name := "notes", owns := ["Note"], bound := 2, reads := ["Note"]
  , decision := .proceed "notes-ok" [update "Note" "n1" 0 "b", coreOp "notes.op"] }

private def tagsPlugin : Script String :=
  { name := "tags", owns := ["Tag"], bound := 2, reads := ["Tag"]
  , decision := .proceed "tags-ok" [create "Tag" "t2" "y", coreOp "tags.op"] }

private def auditPlugin : Script String :=
  { name := "audit", owns := ["Audit"], bound := 1
  , decision := .proceed "audit-ok" [create "Audit" "a1" "seen"] }

private def tagsWith (decision : Decision String) : Script String :=
  tagsPlugin.deciding decision

/-- The view-rule pair: `writer` creates a Note; `reader` reads Notes and
records how many it was shown. -/
private def writerPlugin : Script String :=
  { name := "writer", owns := ["Note"], bound := 1
  , decision := .proceed "written" [create "Note" "n2" "b"] }

private def readerPlugin : Script String :=
  { name := "reader", owns := ["Mark"], bound := 1, reads := ["Note"]
  , decision := .byShown 1 ("saw-snapshot", [create "Mark" "m1" "one-note"])
      ("saw-staged", [create "Mark" "m1" "two-notes"]) }

structure StageCase where
  name : String
  world : World
  plugins : List (Script String)

def StageCase.result (c : StageCase) : Except Refusal (HookState × List String) :=
  Hooks.stage c.world.host (c.plugins.map Script.plugin) caller () c.world.state

def stageCases : List StageCase :=
  [ { name := "no_plugins_stage_nothing", world := stageWorld, plugins := [] }
  , { name := "all_proceed_apply_in_plugin_order", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin, auditPlugin] }
  , { name := "reversed_order_reverses_values_and_core_operations", world := stageWorld
    , plugins := [auditPlugin, tagsPlugin, notesPlugin] }
  , { name := "refusal_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "tag budget exhausted"), auditPlugin] }
  , { name := "first_failure_in_order_is_reported", world := stageWorld
    , plugins := [tagsWith (.refuse "first"),
        auditPlugin.deciding (.proceed "audit-ok" [create "Note" "n5" "foreign"])] }
  , { name := "foreign_write_is_invalid_and_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [create "Note" "n5" "foreign"])] }
  , { name := "acp_denied_write_is_invalid", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [update "Tag" "locked" 0 "z"])] }
  , { name := "over_bound_is_invalid", world := stageWorld
    , plugins := [notesPlugin, auditPlugin.deciding
        (.proceed "audit-ok" [create "Audit" "a1" "x", create "Audit" "a2" "y"])] }
  , { name := "stale_revision_conflicts_and_leaves_state_unchanged", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [update "Tag" "t1" 0 "z"])] }
  , { name := "failed_core_operation_conflicts", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [coreOp "explode"])] }
  , { name := "later_plugin_decides_on_the_shared_snapshot", world := stageWorld
    , plugins := [writerPlugin, readerPlugin] }
  , { name := "earlier_staged_effects_are_not_shown_in_either_order", world := stageWorld
    , plugins := [readerPlugin, writerPlugin] } ]

structure SuccessorWire where
  requestId : String
  requestDocId : String
  deriving ToJson

structure VerdictWire where
  kind : String
  reason : Option String
  successor : Option SuccessorWire
  deriving ToJson

def VerdictWire.ofModel : ClaimVerdict → VerdictWire
  | .allow => ⟨"allow", none, none⟩
  | .leaveQueued => ⟨"leave_queued", none, none⟩
  | .retire reason successor =>
      ⟨"retire", some reason, some ⟨successor.request, successor.doc⟩⟩
  | .reject reason => ⟨"reject", some reason, none⟩

def VerdictWire.toModel (wire : VerdictWire) : ClaimVerdict :=
  match wire.kind, wire.reason, wire.successor with
  | "leave_queued", _, _ => .leaveQueued
  | "retire", some reason, some successor =>
      .retire reason ⟨successor.requestId, successor.requestDocId⟩
  | "reject", some reason, _ => .reject reason
  | _, _, _ => .allow

structure AnswerWire where
  kind : String
  key : Option String
  reason : Option String
  deriving ToJson

/-- A scripted plugin's returned value at any point: `text` at a staging
point, `verdict` at the claim gate, `answers` at the queue scope. -/
structure ValueWire where
  text : Option String
  verdict : Option VerdictWire
  answers : List AnswerWire
  deriving ToJson

def ValueWire.ofText (text : String) : ValueWire := ⟨some text, none, []⟩

def ValueWire.toText (wire : ValueWire) : String := wire.text.getD ""

structure ChoiceWire where
  value : ValueWire
  effects : List EffectWire
  deriving ToJson

structure DecisionWire where
  kind : String
  reason : Option String
  value : Option ValueWire
  effects : List EffectWire
  count : Option Nat
  matching : Option ChoiceWire
  otherwise : Option ChoiceWire
  deriving ToJson

def ChoiceWire.ofModel {V : Type} (valueWire : V → ValueWire) (choice : V × List HookEffect) :
    ChoiceWire :=
  ⟨valueWire choice.1, choice.2.map EffectWire.ofModel⟩

def ChoiceWire.toModel {V : Type} (valueOf : ValueWire → V) (wire : ChoiceWire) :
    V × List HookEffect :=
  (valueOf wire.value, wire.effects.map EffectWire.toModel)

def DecisionWire.ofModel {V : Type} (valueWire : V → ValueWire) : Decision V → DecisionWire
  | .refuse reason => ⟨"refuse", some reason, none, [], none, none, none⟩
  | .proceed value effects =>
      ⟨"proceed", none, some (valueWire value), effects.map EffectWire.ofModel, none, none, none⟩
  | .byShown count matching otherwise =>
      ⟨"by_shown", none, none, [], some count, some (ChoiceWire.ofModel valueWire matching),
        some (ChoiceWire.ofModel valueWire otherwise)⟩

def DecisionWire.toModel {V : Type} (valueOf : ValueWire → V) (wire : DecisionWire) :
    Decision V :=
  match wire.kind, wire.value, wire.matching, wire.otherwise with
  | "proceed", some value, _, _ => .proceed (valueOf value) (wire.effects.map EffectWire.toModel)
  | "by_shown", _, some matching, some otherwise =>
      .byShown (wire.count.getD 0) (matching.toModel valueOf) (otherwise.toModel valueOf)
  | _, _, _, _ => .refuse (wire.reason.getD "")

structure ScriptWire where
  name : String
  owns : List String
  bound : Nat
  reads : List String
  decision : DecisionWire
  deriving ToJson

def ScriptWire.ofModel {V : Type} (valueWire : V → ValueWire) (script : Script V) : ScriptWire :=
  ⟨script.name, script.owns, script.bound, script.reads,
    DecisionWire.ofModel valueWire script.decision⟩

def ScriptWire.toModel {V : Type} (valueOf : ValueWire → V) (wire : ScriptWire) : Script V :=
  { name := wire.name, owns := wire.owns, bound := wire.bound, reads := wire.reads
  , decision := wire.decision.toModel valueOf }

structure RefusalWire where
  kind : String
  plugin : Option String
  reason : Option String
  deriving ToJson

def RefusalWire.ofModel : Refusal → RefusalWire
  | .core => ⟨"core", none, none⟩
  | .refused plugin reason => ⟨"refused", some plugin, some reason⟩
  | .invalid plugin => ⟨"invalid", some plugin, none⟩
  | .conflict plugin => ⟨"conflict", some plugin, none⟩

structure StageResultWire where
  kind : String
  state : Option StateWire
  values : List String
  refusal : Option RefusalWire
  deriving ToJson

def StageResultWire.ofModel : Except Refusal (HookState × List String) → StageResultWire
  | .ok (state, values) => ⟨"ok", some (StateWire.ofModel state), values, none⟩
  | .error refusal => ⟨"error", none, [], some (RefusalWire.ofModel refusal)⟩

structure StageInput where
  name : String
  world : WorldWire
  plugins : List ScriptWire
  deriving ToJson

structure StageOutput where
  result : StageResultWire
  settled : StateWire
  deriving ToJson

@[conformance_family]
def refusableStaging : Family StageInput StageOutput where
  inputs := stageCases.map fun c =>
    { name := c.name, world := WorldWire.ofModel c.world
    , plugins := c.plugins.map (ScriptWire.ofModel ValueWire.ofText) }
  model := fun i =>
    let c : StageCase :=
      { name := i.name, world := i.world.toModel
      , plugins := i.plugins.map (ScriptWire.toModel ValueWire.toText) }
    { result := StageResultWire.ofModel c.result
      settled := StateWire.ofModel (Hooks.settle c.world.state c.result) }

/-! Must-commit points: `Hooks.mustCommit`, `Hooks.stageDropping`, `Hooks.owed`. -/

structure MustCommitCase where
  name : String
  world : World
  plugins : List (Script String)
  transition : String := "terminal"
  transitionRefused : Bool := false
  readFailed : List String := []

def MustCommitCase.transitionStep (c : MustCommitCase) :
    List String → Option (List String) :=
  fun core => if c.transitionRefused then none else some (core ++ [c.transition])

def MustCommitCase.result (c : MustCommitCase) : Option (HookState × List Bool) :=
  Hooks.mustCommit c.world.host (c.plugins.map Script.plugin) caller c.transitionStep
    (fun _ _ => ()) (fun name => c.readFailed.contains name) c.world.state

def MustCommitCase.owedNames (c : MustCommitCase) : List String :=
  match c.result with
  | none => []
  | some (_, dropped) => Hooks.owed (c.plugins.map Script.plugin) dropped

def mustCommitCases : List MustCommitCase :=
  [ { name := "every_plugin_stages", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin, auditPlugin] }
  , { name := "read_failure_drops_only_that_plugin", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin], readFailed := ["notes"] }
  , { name := "refusing_plugin_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "cannot record"), auditPlugin] }
  , { name := "invalid_result_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.proceed "tags-ok" [create "Note" "n5" "foreign"])] }
  , { name := "miss_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin.deciding (.proceed "notes-ok" [update "Note" "n1" 4 "b"]),
                  tagsPlugin] }
  , { name := "failed_core_operation_is_dropped_whole", world := stageWorld
    , plugins := [notesPlugin.deciding
        (.proceed "notes-ok" [update "Note" "n1" 0 "b", coreOp "explode"]), tagsPlugin] }
  , { name := "repeated_document_is_dropped_and_owed", world := stageWorld
    , plugins := [notesPlugin.deciding
        (.proceed "notes-ok" [update "Note" "n1" 0 "b", update "Note" "n1" 1 "c"]), tagsPlugin] }
  , { name := "partial_miss_is_dropped_whole", world := stageWorld
    , plugins := [notesPlugin.deciding
        (.proceed "notes-ok" [update "Note" "n1" 0 "b", update "Note" "n7" 0 "c"]),
                  auditPlugin] }
  , { name := "every_plugin_failing_still_commits_the_transition", world := stageWorld
    , plugins := [notesPlugin, tagsWith (.refuse "cannot record")], readFailed := ["notes"] }
  , { name := "core_refusal_commits_nothing", world := stageWorld
    , plugins := [notesPlugin, tagsPlugin], transitionRefused := true } ]

structure MustCommitInput where
  name : String
  world : WorldWire
  plugins : List ScriptWire
  transition : String
  transitionRefused : Bool
  readFailed : List String
  deriving ToJson

structure CommittedWire where
  state : StateWire
  dropped : List Bool
  deriving ToJson

structure MustCommitOutput where
  committed : Option CommittedWire
  owed : List String
  deriving ToJson

@[conformance_family]
def mustCommitStaging : Family MustCommitInput MustCommitOutput where
  inputs := mustCommitCases.map fun c =>
    { name := c.name, world := WorldWire.ofModel c.world
    , plugins := c.plugins.map (ScriptWire.ofModel ValueWire.ofText)
    , transition := c.transition, transitionRefused := c.transitionRefused
    , readFailed := c.readFailed }
  model := fun i =>
    let c : MustCommitCase :=
      { name := i.name, world := i.world.toModel
      , plugins := i.plugins.map (ScriptWire.toModel ValueWire.toText)
      , transition := i.transition, transitionRefused := i.transitionRefused
      , readFailed := i.readFailed }
    { committed := c.result.map fun result => ⟨StateWire.ofModel result.1, result.2⟩
      owed := c.owedNames }

/-! Re-run requests through owner wrappers: `Hooks.rerunSurvives`. -/

private def carriers : List Carrier := [.context, .sourced, .transparent, .stringified]

/-- Every path of at most three carriers. -/
def rerunPaths : List (List Carrier) :=
  [[]] ++ carriers.map (fun first => [first]) ++
    carriers.flatMap (fun first => carriers.map fun second => [first, second]) ++
    carriers.flatMap (fun first => carriers.flatMap fun second =>
      carriers.map fun third => [first, second, third])

private def carrierString : Carrier → String
  | .context => "context"
  | .sourced => "sourced"
  | .transparent => "transparent"
  | .stringified => "stringified"

private def rerunCaseJson (path : List Carrier) : String :=
  "{\"name\":" ++ jsonString ("direct" :: path.map carrierString |>.foldl
      (fun acc part => if acc.isEmpty then part else acc ++ "_" ++ part) "") ++
    ",\"path\":" ++ jsonStringArray (path.map carrierString) ++
    ",\"rerun\":" ++ boolJson (rerunSurvives path) ++ "}"

def rerunCasesJson : String := jsonArray (rerunPaths.map rerunCaseJson)

/-! The Goals instance at resume: `Hooks.GoalResume.restage`, `bind` and
`resumeGoal`. -/

structure GoalResumeCase where
  name : String
  goal : GoalResume.Goal
  requests : List Resume.Request
  next : Nat := 50

private def resumeAgent : Actor := "did:key:z6MkResumedAgent"

private def resumeGoalRef : Ref Nat := ⟨GoalResume.collection, 1⟩

private def finished (id : Nat) : Resume.Request :=
  { id := id, agent := resumeAgent, signer := resumeAgent, terminal := true }

private def running (id : Nat) : Resume.Request :=
  { id := id, agent := resumeAgent, signer := resumeAgent }

def GoalResumeCase.event (c : GoalResumeCase) : GoalResume.Resumed := ⟨c.next, resumeAgent⟩

def GoalResumeCase.state (c : GoalResumeCase) : State Resume.Session Nat GoalResume.Fact :=
  ⟨⟨true, c.requests⟩, [GoalResume.goalRow resumeGoalRef 0 c.goal]⟩

def GoalResumeCase.restaged (c : GoalResumeCase) : Outcome Unit Nat GoalResume.Fact Resume.CreateArgs :=
  GoalResume.restage c.event [[GoalResume.goalRow resumeGoalRef 0 c.goal],
    c.requests.map GoalResume.render]

def GoalResumeCase.created (c : GoalResumeCase) : Option Resume.CreateArgs :=
  c.restaged.effects.findSome? fun effect =>
    match effect with
    | .core args => some args
    | .write _ => none

def GoalResumeCase.bound (c : GoalResumeCase) : Option (Outcome Unit Nat GoalResume.Fact Resume.CreateArgs) :=
  c.created.map fun args =>
    GoalResume.bind (some (args.request resumeAgent))
      [[GoalResume.goalRow resumeGoalRef 1 { c.goal with status := .active }]]

def GoalResumeCase.result (c : GoalResumeCase) :=
  GoalResume.resumeGoal (fun _ _ _ => true) resumeAgent c.event c.state

def goalResumeCases : List GoalResumeCase :=
  [ { name := "rootless_idle_goal_binds_its_continuation"
    , goal := ⟨7, .paused, none⟩, requests := [finished 10] }
  , { name := "bound_idle_goal_keeps_its_root"
    , goal := ⟨7, .paused, some 10⟩, requests := [finished 10] }
  , { name := "busy_goal_creates_nothing"
    , goal := ⟨7, .paused, none⟩, requests := [running 11, finished 10] }
  , { name := "active_goal_is_left_alone"
    , goal := ⟨7, .active, none⟩, requests := [finished 10] }
  , { name := "goal_without_predecessor_refuses"
    , goal := ⟨7, .paused, none⟩, requests := [] } ]

private def statusString : GoalResume.Status → String
  | .active => "active"
  | .paused => "paused"
  | .complete => "complete"

private def optionNat : Option Nat → String
  | some value => toString value
  | none => "null"

private def goalJson (goal : GoalResume.Goal) : String :=
  "{\"goal\":" ++ toString goal.goal ++ ",\"status\":" ++ jsonString (statusString goal.status) ++
    ",\"root\":" ++ optionNat goal.root ++ "}"

private def referenceJson : Option (Nat × Option Nat) → String
  | some (goal, root) => "{\"goal\":" ++ toString goal ++ ",\"root\":" ++ optionNat root ++ "}"
  | none => "null"

private def resumeRequestJson (request : Resume.Request) : String :=
  "{\"id\":" ++ toString request.id ++ ",\"terminal\":" ++ boolJson request.terminal ++
    ",\"trigger\":" ++ optionNat request.trigger ++
    ",\"reference\":" ++ referenceJson request.reference ++ "}"

private def writtenGoal (effects : List (Effect Nat GoalResume.Fact Resume.CreateArgs)) :
    Option GoalResume.Goal :=
  effects.findSome? fun effect =>
    match effect with
    | .write (.update _ _ (.goal goal)) => some goal
    | _ => none

private def goalDecisionJson (createdOf : Bool) :
    Outcome Unit Nat GoalResume.Fact Resume.CreateArgs → String
  | .refuse reason => "{\"kind\":\"refuse\",\"reason\":" ++ jsonString reason ++ "}"
  | .proceed _ effects =>
      "{\"kind\":\"proceed\",\"goal\":" ++ ((writtenGoal effects).map goalJson).getD "null" ++
        (if createdOf then
          ",\"create\":" ++ ((effects.findSome? fun effect =>
            match effect with
            | .core args => some ("{\"id\":" ++ toString args.id ++
                ",\"trigger\":" ++ optionNat args.trigger ++
                ",\"reference\":" ++ referenceJson args.reference ++ "}")
            | .write _ => none).getD "null")
        else "") ++ "}"

private def goalResumeResultJson :
    Except Refusal (State Resume.Session Nat GoalResume.Fact × List Unit) → String
  | .ok (state, _) =>
      "{\"kind\":\"ok\",\"goals\":" ++ jsonArray (state.docs.filterMap fun row =>
          match row.value with
          | .goal goal => some (goalJson goal)
          | .request _ => none) ++
        ",\"requests\":" ++ jsonArray (state.core.requests.map resumeRequestJson) ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def goalResumeCaseJson (c : GoalResumeCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"goal\":" ++ goalJson c.goal ++
    ",\"requests\":" ++ jsonArray (c.requests.map resumeRequestJson) ++
    ",\"next\":" ++ toString c.next ++
    ",\"restage\":" ++ goalDecisionJson true c.restaged ++
    ",\"bind\":" ++ ((c.bound.map (goalDecisionJson false)).getD "null") ++
    ",\"result\":" ++ goalResumeResultJson c.result ++ "}"

def goalResumeCasesJson : String := jsonArray (goalResumeCases.map goalResumeCaseJson)

/-! Claim verdicts: `Hooks.combine` and `Hooks.gate`. -/

/-- A successor named by its request id; its document id is derived. -/
private def successorOf (request : String) : Successor := ⟨request, request ++ "-doc"⟩

structure CombineCase where
  name : String
  verdicts : List ClaimVerdict

def combineCases : List CombineCase :=
  [ { name := "no_plugins_allow", verdicts := [] }
  , { name := "all_allow", verdicts := [.allow, .allow, .allow] }
  , { name := "leave_queued_over_allow", verdicts := [.allow, .leaveQueued, .allow] }
  , { name := "retire_over_leave_queued"
    , verdicts := [.leaveQueued, .retire "superseded" (successorOf "r2"), .allow] }
  , { name := "reject_over_retire"
    , verdicts := [.retire "superseded" (successorOf "r2"), .reject "denied", .leaveQueued] }
  , { name := "reject_over_retire_permuted"
    , verdicts := [.leaveQueued, .reject "denied", .retire "superseded" (successorOf "r2")] }
  , { name := "reject_first_permuted"
    , verdicts := [.reject "denied", .leaveQueued, .retire "superseded" (successorOf "r2")] }
  , { name := "equal_rejects_first_reason_wins", verdicts := [.allow, .reject "first", .reject "second"] }
  , { name := "equal_rejects_swapped_first_reason_wins", verdicts := [.reject "second", .allow, .reject "first"] }
  , { name := "equal_retires_first_reason_and_successor_win"
    , verdicts := [.retire "older" (successorOf "r2"), .leaveQueued, .retire "newer" (successorOf "r3")] }
  , { name := "equal_reasons_first_successor_wins"
    , verdicts := [.retire "superseded" (successorOf "r3"), .retire "superseded" (successorOf "r2")] }
  , { name := "single_leave_queued", verdicts := [.leaveQueued] } ]

structure CombinationInput where
  name : String
  verdicts : List VerdictWire
  deriving ToJson

structure CombinationOutput where
  combined : VerdictWire
  deriving ToJson

@[conformance_family]
def verdictCombination : Family CombinationInput CombinationOutput where
  inputs := combineCases.map fun c =>
    { name := c.name, verdicts := c.verdicts.map VerdictWire.ofModel }
  model := fun i =>
    { combined := VerdictWire.ofModel (Hooks.combine (i.verdicts.map VerdictWire.toModel)) }

structure GateCase where
  name : String
  world : World
  plugins : List (Script ClaimVerdict)

def GateCase.result (c : GateCase) : Except Refusal (List ClaimVerdict) :=
  Hooks.gate c.world.host () c.world.state (c.plugins.map Script.plugin)

private def fence : Script ClaimVerdict :=
  { name := "goal-fence", owns := ["GoalFence"], bound := 1, reads := ["Note"]
  , decision := .byShown 1 (.retire "goal superseded" (successorOf "r2"), []) (.allow, []) }

private def mailbox : Script ClaimVerdict :=
  { name := "mailbox", owns := ["Mailbox"], bound := 1
  , decision := .proceed .allow [] }

def gateCases : List GateCase :=
  [ { name := "no_plugins_answer_nothing", world := stageWorld, plugins := [] }
  , { name := "every_plugin_allows", world := stageWorld
    , plugins := [mailbox, fence.deciding (.proceed .allow [])] }
  , { name := "verdict_from_shown_rows", world := stageWorld, plugins := [mailbox, fence] }
  , { name := "verdict_from_other_shown_rows"
    , world := { stageWorld with rows := row "Note" "n2" 0 "b" :: stageWorld.rows }
    , plugins := [mailbox, fence] }
  , { name := "mixed_severities_keep_every_verdict", world := stageWorld
    , plugins := [mailbox.deciding (.proceed (.reject "reply rejected") []), fence] }
  , { name := "refusal_is_reported", world := stageWorld
    , plugins := [fence, mailbox.deciding (.refuse "mailbox closed")] }
  , { name := "verdict_with_effects_is_invalid", world := stageWorld
    , plugins := [fence.deciding
        (.proceed (.retire "goal superseded" (successorOf "r2")) [create "GoalFence" "f1" "retired"]),
        mailbox] }
  , { name := "effects_rejected_even_when_allowing", world := stageWorld
    , plugins := [mailbox, fence.deciding (.proceed .allow [coreOp "request.supersede"])] } ]

private def gateResultJson : Except Refusal (List ClaimVerdict) → String
  | .ok verdicts =>
      "{\"kind\":\"ok\",\"verdicts\":" ++ jsonArray (verdicts.map verdictJson) ++
        ",\"combined\":" ++ verdictJson (Hooks.combine verdicts) ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def gateCaseJson (c : GateCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"plugins\":" ++ jsonArray (c.plugins.map (scriptJson verdictJson)) ++
    ",\"result\":" ++ gateResultJson c.result ++ "}"

def gateCasesJson : String := jsonArray (gateCases.map gateCaseJson)

/-! Queue partitions: `Hooks.QueueScope.scopes` at `request.queue.scope`. -/

abbrev ScopeAnswers := QueueScope.Answers String

/-- A scripted plugin at `request.queue.scope`: its answers are aligned with
the requests core asked about, whatever they are. -/
def Script.scopePlugin (script : Script ScopeAnswers) :
    Plugin String (List String) ScopeAnswers String String String :=
  { manifest := script.manifest
  , hook :=
      { reads := fun _ => script.reads
      , decide := fun _ rows => script.decision.outcome rows } }

structure QueueScopeCase where
  name : String
  world : World
  plugins : List (Script ScopeAnswers)
  asked : List String

def QueueScopeCase.result (c : QueueScopeCase) :
    Except Refusal (List (Except Refusal (List (String × String)))) :=
  QueueScope.scopes c.world.host (c.plugins.map Script.scopePlugin) c.asked c.world.state

private def keyed (key : String) : QueueScope.Answer String := .keyed key

private def failed (reason : String) : QueueScope.Answer String := .failed reason

private def silent : QueueScope.Answer String := .unanswered

private def goalsScope : Script ScopeAnswers :=
  { name := "goals", owns := ["Goal"], bound := 0, reads := ["Note"]
  , decision := .proceed [keyed "goal-a", silent] [] }

private def lanes : Script ScopeAnswers :=
  { name := "lanes", owns := ["Lane"], bound := 0
  , decision := .proceed [keyed "lane-1", keyed "lane-1"] [] }

def queueScopeCases : List QueueScopeCase :=
  [ { name := "no_plugins_leave_every_request_unscoped", world := stageWorld, plugins := []
    , asked := ["r1", "r2"] }
  , { name := "a_key_and_the_unscoped_default", world := stageWorld, plugins := [goalsScope]
    , asked := ["r1", "r2"] }
  , { name := "a_missing_position_is_unanswered", world := stageWorld
    , plugins := [goalsScope.deciding (.proceed [keyed "goal-a"] [])]
    , asked := ["r1", "r2", "r3"] }
  , { name := "keys_from_shown_rows", world := stageWorld
    , plugins := [goalsScope.deciding
        (.byShown 1 ([keyed "one-note", keyed "one-note"], []) ([keyed "other", silent], []))]
    , asked := ["r1", "r2"] }
  , { name := "keys_from_other_shown_rows"
    , world := { stageWorld with rows := row "Note" "n2" 0 "b" :: stageWorld.rows }
    , plugins := [goalsScope.deciding
        (.byShown 1 ([keyed "one-note", keyed "one-note"], []) ([keyed "other", silent], []))]
    , asked := ["r1", "r2"] }
  , { name := "keys_are_named_in_plugin_order", world := stageWorld
    , plugins := [goalsScope, lanes], asked := ["r1", "r2"] }
  , { name := "keys_are_named_in_plugin_order_swapped", world := stageWorld
    , plugins := [lanes, goalsScope], asked := ["r1", "r2"] }
  , { name := "a_failure_belongs_to_its_request", world := stageWorld
    , plugins := [goalsScope.deciding (.proceed [failed "history unavailable", keyed "goal-b"] [])]
    , asked := ["r1", "r2"] }
  , { name := "the_first_failure_in_plugin_order_is_reported", world := stageWorld
    , plugins := [goalsScope.deciding (.proceed [failed "goal history", silent] []),
                  lanes.deciding (.proceed [failed "lane closed", keyed "lane-1"] [])]
    , asked := ["r1", "r2"] }
  , { name := "a_later_failure_overrides_an_earlier_key", world := stageWorld
    , plugins := [goalsScope, lanes.deciding (.proceed [failed "lane closed", silent] [])]
    , asked := ["r1", "r2"] }
  , { name := "refusal_is_reported", world := stageWorld
    , plugins := [lanes, goalsScope.deciding (.refuse "goal rows unreadable")]
    , asked := ["r1"] }
  , { name := "answer_with_effects_is_invalid", world := stageWorld
    , plugins := [goalsScope.deciding (.proceed [keyed "goal-a"] [create "Goal" "g1" "x"]), lanes]
    , asked := ["r1"] } ]

private def answerJson : QueueScope.Answer String → String
  | .unanswered => "{\"kind\":\"unanswered\"}"
  | .keyed key => "{\"kind\":\"keyed\",\"key\":" ++ jsonString key ++ "}"
  | .failed reason => "{\"kind\":\"failed\",\"reason\":" ++ jsonString reason ++ "}"

private def answersJson (answers : ScopeAnswers) : String :=
  jsonArray (answers.map answerJson)

private def partitionJson : Except Refusal (List (String × String)) → String
  | .ok keys =>
      "{\"kind\":\"ok\",\"partition\":" ++ jsonArray (keys.map fun entry =>
        "{\"plugin\":" ++ jsonString entry.1 ++ ",\"key\":" ++ jsonString entry.2 ++ "}") ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def queueScopeResultJson :
    Except Refusal (List (Except Refusal (List (String × String)))) → String
  | .ok scopes => "{\"kind\":\"ok\",\"scopes\":" ++ jsonArray (scopes.map partitionJson) ++ "}"
  | .error refusal => "{\"kind\":\"error\",\"refusal\":" ++ refusalJson refusal ++ "}"

private def queueScopeCaseJson (c : QueueScopeCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"world\":" ++ worldJson c.world ++
    ",\"plugins\":" ++ jsonArray (c.plugins.map (scriptJson answersJson)) ++
    ",\"asked\":" ++ jsonStringArray c.asked ++
    ",\"result\":" ++ queueScopeResultJson c.result ++ "}"

def queueScopeCasesJson : String := jsonArray (queueScopeCases.map queueScopeCaseJson)

/-! The delivery record: `Hooks.Delivery.publish`, `deliver` and `pending`. -/

inductive DeliveryStep where
  | publish (outcome : String) (keys : List String) (commit : Bool)
  | deliver (key : String) (applied : Bool)

def DeliveryStep.run : DeliveryStep → Record → Record
  | .publish outcome keys commit, record => Delivery.publish record outcome keys commit
  | .deliver key applied, record => Delivery.deliver record key applied

structure DeliveryCase where
  name : String
  initial : Record := ⟨none, [], []⟩
  steps : List DeliveryStep

def DeliveryCase.trace (c : DeliveryCase) : List (DeliveryStep × Record) :=
  (c.steps.foldl (fun (acc : Record × List (DeliveryStep × Record)) step =>
    let next := step.run acc.1
    (next, acc.2 ++ [(step, next)])) (c.initial, [])).2

def deliveryCases : List DeliveryCase :=
  [ { name := "rollback_publishes_nothing"
    , steps := [.publish "completed" ["goal", "mailbox"] false, .deliver "goal" true] }
  , { name := "committed_terminal_owes_each_key"
    , steps := [.publish "completed" ["goal", "mailbox"] true, .deliver "goal" true,
                .deliver "mailbox" true] }
  , { name := "terminal_is_published_once"
    , steps := [.publish "completed" ["goal"] true, .publish "failed" ["mailbox"] true] }
  , { name := "failed_delivery_stays_pending"
    , steps := [.publish "completed" ["goal"] true, .deliver "goal" false,
                .deliver "goal" false, .deliver "goal" true] }
  , { name := "redelivery_is_inert"
    , steps := [.publish "completed" ["goal"] true, .deliver "goal" true,
                .deliver "goal" true, .deliver "goal" false] }
  , { name := "unowed_delivery_is_inert"
    , steps := [.deliver "goal" true, .publish "completed" ["goal"] true] }
  , { name := "duplicate_keys_are_owed_once"
    , steps := [.publish "completed" ["goal", "mailbox", "goal"] true, .deliver "goal" true] }
  , { name := "keys_owed_before_publication_are_merged"
    , initial := ⟨none, ["trigger", "goal"], ["trigger"]⟩
    , steps := [.publish "interrupted" ["goal", "mailbox"] true, .deliver "mailbox" true] } ]

structure RecordWire where
  terminal : Option String
  owed : List String
  delivered : List String
  deriving ToJson

def RecordWire.ofModel (record : Record) : RecordWire :=
  ⟨record.terminal, record.owed, record.delivered⟩

def RecordWire.toModel (wire : RecordWire) : Record :=
  ⟨wire.terminal, wire.owed, wire.delivered⟩

structure DeliveryStepWire where
  kind : String
  outcome : Option String
  keys : List String
  commit : Option Bool
  key : Option String
  applied : Option Bool
  deriving ToJson

def DeliveryStepWire.ofModel : DeliveryStep → DeliveryStepWire
  | .publish outcome keys commit => ⟨"publish", some outcome, keys, some commit, none, none⟩
  | .deliver key applied => ⟨"deliver", none, [], none, some key, some applied⟩

def DeliveryStepWire.toModel (wire : DeliveryStepWire) : DeliveryStep :=
  match wire.kind, wire.key with
  | "deliver", some key => .deliver key (wire.applied.getD false)
  | _, _ => .publish (wire.outcome.getD "") wire.keys (wire.commit.getD false)

/-- One step of a delivery trace: `before` is the record the trace started
from (`initial`) folded through the steps preceding this one. -/
structure DeliveryStepInput where
  name : String
  initial : RecordWire
  before : RecordWire
  step : DeliveryStepWire
  deriving ToJson

structure DeliveryStepOutput where
  state : RecordWire
  pending : List String
  deriving ToJson

@[conformance_family]
def deliveryStep : Family DeliveryStepInput DeliveryStepOutput where
  inputs := deliveryCases.flatMap fun c =>
    (c.trace.zip (c.initial :: c.trace.map Prod.snd)).map fun (entry, before) =>
      { name := c.name, initial := RecordWire.ofModel c.initial
      , before := RecordWire.ofModel before, step := DeliveryStepWire.ofModel entry.1 }
  model := fun i =>
    let next := i.step.toModel.run i.before.toModel
    { state := RecordWire.ofModel next, pending := Delivery.pending next }

/-! Prepared decisions: `Hooks.prepare` and `Hooks.admit`. -/

abbrev AdmitBinding := Binding String

private def admitBinding : AdmitBinding :=
  { actor := caller, point := "request.admit.verify", subject := "request-doc"
  , inputs := "sha256:shown", configuration := "" }

structure AdmitCase where
  name : String
  authorized : Bool := true
  cancelled : Bool := false
  current : AdmitBinding := admitBinding
  /-- The binding the host captured when it prepared, and the plugin's
  answer; `none` when nothing was prepared. -/
  prepared : Option (AdmitBinding × Verdict String)

def admitCases : List AdmitCase :=
  [ { name := "matching_allow_admits_with_its_payload"
    , prepared := some (admitBinding, .allow "sequence-1") }
  , { name := "unauthorized_actor_never_admits", authorized := false
    , prepared := some (admitBinding, .allow "sequence-1") }
  , { name := "unauthorized_wins_over_cancelled", authorized := false, cancelled := true
    , prepared := none }
  , { name := "cancelled_step_never_admits", cancelled := true
    , prepared := some (admitBinding, .allow "sequence-1") }
  , { name := "missing_decision_never_admits", prepared := none }
  , { name := "changed_inputs_are_stale"
    , prepared := some ({ admitBinding with inputs := "sha256:earlier" }, .allow "sequence-1") }
  , { name := "changed_subject_is_stale"
    , prepared := some ({ admitBinding with subject := "other-doc" }, .allow "sequence-1") }
  , { name := "changed_actor_is_stale"
    , prepared := some ({ admitBinding with actor := "did:key:z6MkOther" }, .allow "x") }
  , { name := "changed_configuration_is_stale"
    , prepared := some ({ admitBinding with configuration := "v2" }, .allow "x") }
  , { name := "stale_wins_over_deny"
    , prepared := some ({ admitBinding with point := "request.work.complete" }, .deny "no") }
  , { name := "deny_carries_its_reason"
    , prepared := some (admitBinding, .deny "continuation receipt has a different physical goal edge") }
  , { name := "unavailable_never_admits", prepared := some (admitBinding, .unavailable) } ]

structure BindingWire where
  actor : String
  point : String
  subject : String
  inputs : String
  configuration : String
  deriving ToJson

def BindingWire.ofModel (binding : AdmitBinding) : BindingWire :=
  ⟨binding.actor, binding.point, binding.subject, binding.inputs, binding.configuration⟩

def BindingWire.toModel (wire : BindingWire) : AdmitBinding :=
  { actor := wire.actor, point := wire.point, subject := wire.subject
  , inputs := wire.inputs, configuration := wire.configuration }

structure PreparedVerdictWire where
  kind : String
  payload : Option String
  reason : Option String
  deriving ToJson

def PreparedVerdictWire.ofModel : Verdict String → PreparedVerdictWire
  | .allow payload => ⟨"allow", some payload, none⟩
  | .deny reason => ⟨"deny", none, some reason⟩
  | .unavailable => ⟨"unavailable", none, none⟩

def PreparedVerdictWire.toModel (wire : PreparedVerdictWire) : Verdict String :=
  match wire.kind, wire.payload, wire.reason with
  | "allow", some payload, _ => .allow payload
  | "deny", _, some reason => .deny reason
  | _, _, _ => .unavailable

structure PreparedWire where
  binding : BindingWire
  verdict : PreparedVerdictWire
  deriving ToJson

structure StaleWire where
  kind : String
  reason : Option String
  deriving ToJson

def StaleWire.ofModel : Stale → StaleWire
  | .unauthorized => ⟨"unauthorized", none⟩
  | .cancelled => ⟨"cancelled", none⟩
  | .missing => ⟨"missing", none⟩
  | .stale => ⟨"stale", none⟩
  | .denied reason => ⟨"denied", some reason⟩
  | .unavailable => ⟨"unavailable", none⟩

structure AdmitResultWire where
  kind : String
  payload : Option String
  stale : Option StaleWire
  deriving ToJson

def AdmitResultWire.ofModel : Except Stale String → AdmitResultWire
  | .ok payload => ⟨"ok", some payload, none⟩
  | .error reason => ⟨"error", none, some (StaleWire.ofModel reason)⟩

/-- `captured` is the binding the host captured when it prepared, with the
plugin's answer; `none` when nothing was prepared. -/
structure AdmissionInput where
  name : String
  authorized : Bool
  cancelled : Bool
  current : BindingWire
  captured : Option PreparedWire
  deriving ToJson

structure AdmissionOutput where
  prepared : Option PreparedWire
  result : AdmitResultWire
  deriving ToJson

@[conformance_family]
def preparedAdmission : Family AdmissionInput AdmissionOutput where
  inputs := admitCases.map fun c =>
    { name := c.name, authorized := c.authorized, cancelled := c.cancelled
    , current := BindingWire.ofModel c.current
    , captured := c.prepared.map fun entry =>
        ⟨BindingWire.ofModel entry.1, PreparedVerdictWire.ofModel entry.2⟩ }
  model := fun i =>
    let decision := i.captured.map fun captured =>
      Hooks.prepare captured.binding.toModel (fun (_ : Unit) => captured.verdict.toModel) ()
    { prepared := decision.map fun prepared =>
        ⟨BindingWire.ofModel prepared.binding, PreparedVerdictWire.ofModel prepared.verdict⟩
      result := AdmitResultWire.ofModel
        (Hooks.admit i.authorized i.cancelled i.current.toModel decision) }

/-! The owed-delivery record: `Hooks.OwedDelivery.owe`, `publish`, `redrive`
and `performs` over the records of one node; `deriveHalts`; `prune`. -/

inductive OwedStep where
  | owe (point : String) (actor owner : Option String) (signable : List String)
      (keys : List OwedDelivery.Key) (commit : Bool)
  | redrive (key : OwedDelivery.Key) (authorized current : Bool)
      (attempt : OwedDelivery.Attempt) (refused : List OwedDelivery.Write)

def OwedStep.runner : OwedStep → Option String
  | .owe point actor owner signable _ _ =>
      OwedDelivery.owe (OwedDelivery.Kind.ofPoint point) actor owner (signable.contains ·)
  | .redrive .. => none

def OwedStep.run (step : OwedStep) (records : List OwedDelivery.Record) :
    List OwedDelivery.Record :=
  match step with
  | .owe point actor _ _ keys commit =>
      match step.runner with
      | some runner =>
          OwedDelivery.publish records
            (OwedDelivery.Record.fresh (OwedDelivery.Kind.ofPoint point) actor runner) keys commit
      | none => records
  | .redrive key authorized current attempt refused =>
      records.map fun record =>
        if record.key == key then
          OwedDelivery.redrive record ⟨authorized, current⟩ attempt (!refused.contains ·)
        else record

def OwedStep.performed : OwedStep → List OwedDelivery.Record → Bool
  | .owe .., _ => false
  | .redrive key authorized current attempt refused, records =>
      records.any fun record =>
        record.key == key &&
          OwedDelivery.performs record ⟨authorized, current⟩ attempt (!refused.contains ·)

structure OwedCase where
  name : String
  steps : List OwedStep

def OwedCase.trace (c : OwedCase) :
    List (OwedStep × Option String × List OwedDelivery.Record × Bool) :=
  (c.steps.foldl (fun (acc : List OwedDelivery.Record ×
      List (OwedStep × Option String × List OwedDelivery.Record × Bool)) step =>
    let next := step.run acc.1
    (next, acc.2 ++ [(step, step.runner, next, step.performed acc.1)])) ([], [])).2

def haltKey : OwedDelivery.Key := ⟨"control.session.halt", "goals", "session-1"⟩
def terminalKey : OwedDelivery.Key := ⟨"request.terminal.stage", "triggers", "request-1"⟩
def stopKey : OwedDelivery.Key := ⟨"safety.monitor.stop", "monitor", "request-1"⟩
def goalWrite : OwedDelivery.Write := ⟨"session-1", "goal-1"⟩
def fireWrite : OwedDelivery.Write := ⟨"request-1", "fire-1"⟩

def owedCases : List OwedCase :=
  [ { name := "rollback_publishes_nothing"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] false,
                .redrive haltKey true true (.applied [goalWrite]) []] }
  , { name := "user_halt_runs_as_the_owning_agent"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] true,
                .redrive haltKey true true (.applied [goalWrite]) []] }
  , { name := "actorless_staging_runs_as_the_owning_agent"
    , steps := [.owe "request.terminal.stage" none (some "agent") [] [terminalKey] true,
                .redrive terminalKey true true (.applied [fireWrite]) []] }
  , { name := "staging_without_an_owning_agent_is_not_recorded"
    , steps := [.owe "control.session.halt" (some "user") none ["user"] [haltKey] true] }
  , { name := "unowned_subject_is_dropped_unperformed"
    , steps := [.owe "control.session.halt" (some "agent") (some "agent") ["agent"] [haltKey] true,
                .redrive haltKey false true (.applied [goalWrite]) []] }
  , { name := "superseded_transition_is_dropped_unperformed"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] true,
                .redrive haltKey true false (.applied [goalWrite]) []] }
  , { name := "write_off_the_subject_is_refused_and_stays_owed"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] true,
                .redrive haltKey true true (.applied [fireWrite]) [],
                .redrive haltKey true true (.applied [goalWrite]) []] }
  , { name := "write_the_agent_may_not_make_is_refused_and_stays_owed"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] true,
                .redrive haltKey true true (.applied [goalWrite]) [goalWrite]] }
  , { name := "failed_attempts_stay_owed_until_delivered"
    , steps := [.owe "request.terminal.stage" (some "agent") (some "agent") [] [terminalKey] true,
                .redrive terminalKey true true (.failed "refused") [],
                .redrive terminalKey true true (.failed "unreadable") [],
                .redrive terminalKey true true (.applied [fireWrite]) []] }
  , { name := "on_behalf_runs_under_its_signable_actor"
    , steps := [.owe "safety.monitor.stop" (some "agent") (some "other") ["agent"] [stopKey] true,
                .redrive stopKey true true (.applied [fireWrite]) []] }
  , { name := "on_behalf_for_an_unsignable_actor_is_refused_at_owe"
    , steps := [.owe "safety.monitor.stop" (some "user") (some "agent") ["agent"] [stopKey] true,
                .redrive stopKey true true (.applied [fireWrite]) []] }
  , { name := "revoked_on_behalf_actor_is_dropped"
    , steps := [.owe "safety.monitor.stop" (some "agent") (some "agent") ["agent"] [stopKey] true,
                .redrive stopKey false true (.applied [fireWrite]) []] }
  , { name := "redelivery_is_inert"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey] true,
                .redrive haltKey true true (.applied [goalWrite]) [],
                .redrive haltKey true true (.applied [goalWrite]) [],
                .redrive haltKey false false (.failed "late") []] }
  , { name := "publishing_twice_is_inert"
    , steps := [.owe "control.session.halt" (some "user") (some "agent") [] [haltKey, haltKey] true,
                .owe "control.session.halt" (some "other") (some "agent") [] [haltKey] true,
                .redrive haltKey true true (.applied [goalWrite]) [],
                .owe "control.session.halt" (some "other") (some "agent") [] [haltKey] true] } ]

structure OwedDeriveCase where
  name : String
  bound : Nat
  sessions : List OwedDelivery.HaltedSession

def owedDeriveCases : List OwedDeriveCase :=
  [ { name := "current_halts_are_derived_within_the_bound", bound := 2
    , sessions := [⟨"s1", some "agent", some "t1", true⟩, ⟨"s2", some "agent", some "t2", true⟩,
                   ⟨"s3", some "other", some "t3", true⟩] }
  , { name := "superseded_unowned_and_unlatched_sessions_are_skipped", bound := 8
    , sessions := [⟨"resumed", some "agent", some "t1", false⟩, ⟨"orphan", none, some "t2", true⟩,
                   ⟨"running", some "agent", none, true⟩, ⟨"halted", some "agent", some "t4", true⟩] }
  , { name := "zero_bound_derives_nothing", bound := 0
    , sessions := [⟨"s1", some "agent", some "t1", true⟩] } ]

structure OwedPruneCase where
  name : String
  retention : Nat
  records : List (OwedDelivery.Record × Nat)

def settledRecord (key : OwedDelivery.Key) (settled : Option OwedDelivery.Settlement) :
    OwedDelivery.Record :=
  { OwedDelivery.Record.fresh .staging (some "user") "agent" key with settled := settled }

def owedPruneCases : List OwedPruneCase :=
  [ { name := "settled_records_past_retention_are_pruned", retention := 10
    , records := [(settledRecord haltKey (some .delivered), 11),
                  (settledRecord terminalKey (some .dropped), 10),
                  (settledRecord stopKey none, 100)] } ]

structure OwedKeyWire where
  point : String
  plugin : String
  subject : String
  deriving ToJson

def OwedKeyWire.ofModel (key : OwedDelivery.Key) : OwedKeyWire :=
  ⟨key.point, key.plugin, key.subject⟩

def OwedKeyWire.toModel (wire : OwedKeyWire) : OwedDelivery.Key :=
  ⟨wire.point, wire.plugin, wire.subject⟩

structure OwedWriteWire where
  subject : String
  doc : String
  deriving ToJson

def OwedWriteWire.ofModel (write : OwedDelivery.Write) : OwedWriteWire :=
  ⟨write.subject, write.doc⟩

def OwedWriteWire.toModel (wire : OwedWriteWire) : OwedDelivery.Write :=
  ⟨wire.subject, wire.doc⟩

structure OwedAttemptWire where
  kind : String
  writes : List OwedWriteWire
  error : Option String
  deriving ToJson

def OwedAttemptWire.ofModel : OwedDelivery.Attempt → OwedAttemptWire
  | .applied writes => ⟨"applied", writes.map OwedWriteWire.ofModel, none⟩
  | .failed error => ⟨"failed", [], some error⟩

def OwedAttemptWire.toModel (wire : OwedAttemptWire) : OwedDelivery.Attempt :=
  match wire.kind, wire.error with
  | "failed", some error => .failed error
  | _, _ => .applied (wire.writes.map OwedWriteWire.toModel)

structure OwedStepWire where
  kind : String
  point : Option String
  actor : Option String
  owner : Option String
  signable : List String
  keys : List OwedKeyWire
  commit : Option Bool
  key : Option OwedKeyWire
  authorized : Option Bool
  current : Option Bool
  attempt : Option OwedAttemptWire
  refused : List OwedWriteWire
  deriving ToJson

def OwedStepWire.ofModel : OwedStep → OwedStepWire
  | .owe point actor owner signable keys commit =>
      ⟨"owe", some point, actor, owner, signable, keys.map OwedKeyWire.ofModel, some commit,
        none, none, none, none, []⟩
  | .redrive key authorized current attempt refused =>
      ⟨"redrive", none, none, none, [], [], none, some (OwedKeyWire.ofModel key),
        some authorized, some current, some (OwedAttemptWire.ofModel attempt),
        refused.map OwedWriteWire.ofModel⟩

def OwedStepWire.toModel (wire : OwedStepWire) : OwedStep :=
  match wire.kind, wire.key, wire.attempt with
  | "redrive", some key, some attempt =>
      .redrive key.toModel (wire.authorized.getD false) (wire.current.getD false)
        attempt.toModel (wire.refused.map OwedWriteWire.toModel)
  | _, _, _ =>
      .owe (wire.point.getD "") wire.actor wire.owner wire.signable
        (wire.keys.map OwedKeyWire.toModel) (wire.commit.getD false)

structure OwedRecordWire where
  key : OwedKeyWire
  kind : String
  actor : Option String
  runner : String
  attempts : Nat
  lastError : Option String
  settled : Option String
  deriving ToJson

def OwedRecordWire.ofModel (record : OwedDelivery.Record) : OwedRecordWire :=
  { key := OwedKeyWire.ofModel record.key
  , kind := match record.kind with
      | .staging => "staging"
      | .onBehalf => "on_behalf"
  , actor := record.actor, runner := record.runner, attempts := record.attempts
  , lastError := record.lastError
  , settled := record.settled.map fun
      | .delivered => "delivered"
      | .dropped => "dropped" }

def OwedRecordWire.toModel (wire : OwedRecordWire) : OwedDelivery.Record :=
  { key := wire.key.toModel
  , kind := if wire.kind == "on_behalf" then .onBehalf else .staging
  , actor := wire.actor, runner := wire.runner, attempts := wire.attempts
  , lastError := wire.lastError
  , settled := wire.settled.map fun settled =>
      if settled == "dropped" then .dropped else .delivered }

structure OwedTraceInput where
  name : String
  steps : List OwedStepWire
  deriving ToJson

/-- What one step of an owed-delivery trace leaves: the identity an `owe`
would run as, the records of the node after the step, and whether a redrive
performed its delivery. -/
structure OwedObservation where
  runner : Option String
  records : List OwedRecordWire
  performed : Bool
  deriving ToJson

structure OwedTraceOutput where
  trace : List OwedObservation
  deriving ToJson

@[conformance_family]
def owedTrace : Family OwedTraceInput OwedTraceOutput where
  inputs := owedCases.map fun c => { name := c.name, steps := c.steps.map OwedStepWire.ofModel }
  model := fun i =>
    { trace := (OwedCase.trace ⟨i.name, i.steps.map OwedStepWire.toModel⟩).map
        fun (_, runner, records, performed) =>
          { runner, records := records.map OwedRecordWire.ofModel, performed } }

deriving instance ToJson for OwedDelivery.HaltedSession

structure OwedDeriveInput where
  name : String
  bound : Nat
  sessions : List OwedDelivery.HaltedSession
  deriving ToJson

structure DerivedHaltWire where
  session : String
  haltedAt : String
  owner : String
  deriving ToJson

structure OwedDeriveOutput where
  derived : List DerivedHaltWire
  deriving ToJson

@[conformance_family]
def owedDerive : Family OwedDeriveInput OwedDeriveOutput where
  inputs := owedDeriveCases.map fun c => { name := c.name, bound := c.bound, sessions := c.sessions }
  model := fun i =>
    { derived := (OwedDelivery.deriveHalts i.bound i.sessions).map fun (session, time, owner) =>
        ⟨session, time, owner⟩ }

structure AgedRecordWire where
  record : OwedRecordWire
  age : Nat
  deriving ToJson

structure OwedPruneInput where
  name : String
  retention : Nat
  records : List AgedRecordWire
  deriving ToJson

structure OwedPruneOutput where
  kept : List AgedRecordWire
  deriving ToJson

@[conformance_family]
def owedPrune : Family OwedPruneInput OwedPruneOutput where
  inputs := owedPruneCases.map fun c =>
    { name := c.name, retention := c.retention
    , records := c.records.map fun (record, age) => ⟨OwedRecordWire.ofModel record, age⟩ }
  model := fun i =>
    { kept := (OwedDelivery.prune (i.records.map fun aged => (aged.record.toModel, aged.age))
        i.retention).map fun (record, age) => ⟨OwedRecordWire.ofModel record, age⟩ }

end Conformance.HooksContracts
