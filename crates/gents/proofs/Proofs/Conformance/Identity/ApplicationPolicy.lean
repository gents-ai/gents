import Proofs.Store.Identity.ApplicationPolicy
import Proofs.Conformance.Family

namespace Conformance.ApplicationPolicy

open Lean Identity Identity.Household

structure RelationWire where
  name : String
  types : List String
  manages : List String
  deriving ToJson

structure ParentWire where
  relation : String
  permission : String
  deriving ToJson

structure PermissionWire where
  name : String
  relations : List String
  parents : List ParentWire
  deriving ToJson

structure ResourceWire where
  name : String
  relations : List RelationWire
  permissions : List PermissionWire
  deriving ToJson

structure DefinitionWire where
  name : String
  resources : List ResourceWire
  deriving ToJson

structure AdmissionOutput where
  refusal : Option String
  yaml : Option String
  deriving ToJson

def DefinitionWire.ofModel (d : ApplicationPolicy.Definition) : DefinitionWire :=
  { name := d.name
    resources := d.resources.map fun r =>
      { name := r.name
        relations := r.relations.map fun rel => { name := rel.name, types := rel.types, manages := rel.manages }
        permissions := r.permissions.map fun p =>
          { name := p.name, relations := p.relations
            parents := p.parents.map fun (relation, permission) => { relation, permission } } } }

def DefinitionWire.model (w : DefinitionWire) : ApplicationPolicy.Definition :=
  { name := w.name
    resources := w.resources.map fun r =>
      { name := r.name
        relations := r.relations.map fun rel => ⟨rel.name, rel.types, rel.manages⟩
        permissions := r.permissions.map fun p =>
          ⟨p.name, p.relations, p.parents.map fun parent => (parent.relation, parent.permission)⟩ } }

private def documentPermissionsOver (read update : List String) : List PermissionDefinition :=
  [permission "read" read [], permission "update" update [], permission "delete" [] []]

private def ordersWith (resources : List ResourceDefinition) : ApplicationPolicy.Definition :=
  { ApplicationPolicy.orders with resources }

private def tables : ApplicationPolicy.Definition :=
  { name := "Shared tables"
    resources :=
      [{ name := "tables", relations := [⟨"reader", ["actor"], []⟩],
         permissions := documentPermissionsOver ["reader"] [] },
       { name := "rows", relations := [⟨"table", ["tables"], []⟩, ⟨"writer", ["actor"], []⟩],
         permissions := [permission "read" ["writer"] [("table", "read")],
           permission "update" ["writer"] [], permission "delete" [] []] }] }

private def teams : ApplicationPolicy.Definition :=
  { name := "Team notes"
    resources :=
      [{ name := "teams", relations := [⟨"member", ["actor"], []⟩],
         permissions := documentPermissionsOver ["member"] [] },
       { name := "notes", relations := [⟨"team", ["teams->member"], []⟩],
         permissions := documentPermissionsOver ["team"] ["team"] }] }

private def withRelations (relations : List RelationDefinition) : ApplicationPolicy.Definition :=
  ordersWith [{ ApplicationPolicy.ordersResource with relations }]

private def withPermissions (permissions : List PermissionDefinition) : ApplicationPolicy.Definition :=
  ordersWith [{ ApplicationPolicy.ordersResource with permissions }]

/-- Admitted definitions of each shape, then one definition breaking each rule,
with the policy name rule at its edges. -/
def cases : List ApplicationPolicy.Definition :=
  [ApplicationPolicy.orders, tables, teams,
   { ApplicationPolicy.orders with name := "" },
   { ApplicationPolicy.orders with name := policyName },
   { ApplicationPolicy.orders with name := "Gents retained file images" },
   { ApplicationPolicy.orders with name := "Orders\nresources: []" },
   { ApplicationPolicy.orders with name := "Orders " },
   { ApplicationPolicy.orders with name := "1 orders" },
   { ApplicationPolicy.orders with name := "Orders: read" },
   { ApplicationPolicy.orders with name := String.mk (List.replicate 65 'a') },
   { ApplicationPolicy.orders with name := "Orders_read-only 2" },
   ordersWith [],
   ordersWith [{ ApplicationPolicy.ordersResource with name := "Orders" }],
   ordersWith [{ ApplicationPolicy.ordersResource with name := "null" }],
   withRelations [⟨"owner", ["actor"], []⟩],
   ordersWith [ApplicationPolicy.ordersResource, ApplicationPolicy.ordersResource],
   withRelations [⟨"read", ["actor"], []⟩],
   withPermissions [permission "read" ["reader"] [], permission "update" ["writer"] []],
   withRelations [⟨"reader", [], []⟩, ⟨"writer", ["actor"], []⟩, ⟨"reader_manager", ["actor"], ["reader"]⟩],
   withRelations [⟨"reader", ["nodes->administrator"], []⟩, ⟨"writer", ["actor"], []⟩,
     ⟨"reader_manager", ["actor"], ["reader"]⟩],
   withRelations [⟨"reader", ["actor:individual"], []⟩, ⟨"writer", ["actor"], []⟩,
     ⟨"reader_manager", ["actor"], ["reader"]⟩],
   { teams with resources := teams.resources.map fun r =>
       if r.name == "notes" then { r with relations := [⟨"team", ["teams->read"], []⟩] } else r },
   withRelations [⟨"reader", ["actor"], []⟩, ⟨"writer", ["actor"], []⟩,
     ⟨"reader_manager", ["actor"], ["read"]⟩],
   withPermissions (documentPermissionsOver ["reader", "editor"] []),
   withPermissions [permission "read" [] [("reader", "read")], permission "update" [] [],
     permission "delete" [] []],
   { tables with resources := tables.resources.map fun r =>
       if r.name == "rows" then
         { r with permissions := [permission "read" [] [("table", "write")],
           permission "update" [] [], permission "delete" [] []] }
       else r }]

/-- Whether the policy tool registers a definition, the rule it breaks first,
and the policy text it registers. -/
@[conformance_family]
def admission : Family DefinitionWire AdmissionOutput where
  inputs := cases.map DefinitionWire.ofModel
  model := fun w =>
    let d := w.model
    { refusal := (ApplicationPolicy.refusal d).map (·.wire)
      yaml := if ApplicationPolicy.admissible d then some (ApplicationPolicy.yaml d) else none }

end Conformance.ApplicationPolicy
