import Proofs.Documents.ScopeTemplates.Derivation
import Proofs.Conformance.Family

namespace Conformance.Scope

open Lean ScopeTemplates

deriving instance ToJson for RouteDirection, Delivery

@[conformance_vocabulary]
def directionWire : RouteDirection → String
  | .clientToRuntime => "client-to-runtime"
  | .runtimeToClient => "runtime-to-client"

def directions : List RouteDirection := [.clientToRuntime, .runtimeToClient]

def owners : List String := ["did:key:owner", "did:key:quoted\"owner"]

structure DirectionInput where
  direction : RouteDirection
  deriving ToJson

structure NodeRouteAllowed where
  allowed : Bool
  deriving ToJson

@[conformance_family]
def nodeRouteAllowed : Family DirectionInput NodeRouteAllowed where
  inputs := directions.map fun direction => { direction }
  model := fun i => { allowed := (clientRouteCollections i.direction).contains "Node" }

structure RouteInput where
  direction : RouteDirection
  requester : String
  owner : String
  deriving ToJson

structure NodeRouteFilter where
  nodeDid : Option String
  deriving ToJson

@[conformance_family]
def nodeRouteFilter : Family RouteInput NodeRouteFilter where
  inputs := directions.flatMap fun direction =>
    owners.map fun owner => { direction, requester := "did:key:reader", owner }
  model := fun i =>
    { nodeDid := ((resolveTemplate builtinCatalog "client").map fun template =>
        directionalScopeFilters template i.direction i.requester i.owner).bind fun filters =>
          (filters.find? (·.collection == "Node")).bind fun predicate =>
            (predicate.clauses.find? (·.field == "node_did")).map (·.value) }

structure TemplateInput where
  id : String
  deriving ToJson

structure TemplateRoute where
  collections : List String
  delivery : Delivery
  deriving ToJson

@[conformance_family]
def template : Family TemplateInput (Option TemplateRoute) where
  inputs := builtinCatalog.map fun template => { id := template.id }
  model := fun i => (resolveTemplate builtinCatalog i.id).map fun template =>
    { collections := template.collections, delivery := template.delivery }

structure TemplateFiltersInput where
  id : String
  direction : RouteDirection
  requester : String
  owner : String
  deriving ToJson

structure Clause where
  field : String
  operator : String
  value : String
  deriving ToJson

structure Predicate where
  collection : String
  clauses : List Clause
  deriving ToJson

structure TemplateFilters where
  filters : List Predicate
  deriving ToJson

@[conformance_family]
def templateFilters : Family TemplateFiltersInput (Option TemplateFilters) where
  inputs := builtinCatalog.flatMap fun template => directions.map fun direction =>
    { id := template.id, direction, requester := "did:key:reader", owner := "did:key:owner" }
  model := fun i => (resolveTemplate builtinCatalog i.id).map fun template =>
    { filters := (directionalScopeFilters template i.direction i.requester i.owner).map fun predicate =>
        { collection := predicate.collection
          clauses := predicate.clauses.map fun clause =>
            { field := clause.field, operator := clause.operator, value := clause.value } } }

end Conformance.Scope
