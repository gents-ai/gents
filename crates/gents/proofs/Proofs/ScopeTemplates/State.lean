import Proofs.ConfigDocuments
import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

import Proofs.Identity.State

/-! P2P routes exchange discovery and enrollment protocol only. Runtime data is
read and written through its authenticated database endpoint. Neither a route
name nor transport enrollment grants document access. -/

namespace ScopeTemplates

abbrev TemplateId := String
abbrev Did := String

/-- Every protocol route pushes; no surviving template subscribes or replicates. -/
inductive Delivery where
  | push
  deriving DecidableEq, Repr

inductive RouteDirection where
  | clientToRuntime
  | runtimeToClient
  deriving DecidableEq, Repr

inductive DidSource where
  | localDid
  | homeDid
  deriving DecidableEq, Repr

structure CollectionRule where
  collection : String
  field : String
  source : DidSource
  deriving DecidableEq, Repr

inductive Scope where
  | perCollection (rules : List CollectionRule)
  | clientRoute
  deriving DecidableEq, Repr

structure CollectionScopeFilter where
  collection : String
  field : String
  operator : String := "_eq"
  value : Did
  deriving DecidableEq, Repr

structure FilterClause where
  field : String
  operator : String := "_eq"
  value : Did
  deriving DecidableEq, Repr

structure CollectionPredicate where
  collection : String
  clauses : List FilterClause
  deriving DecidableEq, Repr

structure Template where
  id : TemplateId
  collections : List String
  scope : Scope
  delivery : Delivery
  deriving DecidableEq

abbrev Catalog := List Template

/-- Runtime file review uses authenticated database reads; no application record
has an explicit document-push exception to the protocol-only transport boundary. -/

def transcriptCollections : List String :=
  ["AgentRequest", "AgentMessage", "AgentToolCall", "AgentOutputSegment",
   "AgentSession", "CompactionEntry"]

def credentialCollections : List String := ["InferenceBackend", "OAuthCredential"]

def clientToRuntimeCollections : List String := ["PeerRegistry"]
def clientCollections : List String := ["PeerRegistry"]
def clientRouteCollections : RouteDirection → List String
  | .clientToRuntime => clientToRuntimeCollections
  | .runtimeToClient => clientCollections

def machineCollections : List String := ["NodeDirectoryEntry"]

def machineRules : List CollectionRule :=
  [{ collection := "NodeDirectoryEntry", field := "source_did", source := .homeDid }]

def machineTemplate : Template :=
  { id := "machine", collections := machineCollections,
    scope := .perCollection machineRules, delivery := .push }

def clientTemplate : Template :=
  { id := "client", collections := clientCollections,
    scope := .clientRoute, delivery := .push }

/-- Application collection replication is not a supported route: the catalog
holds only the discovery templates. -/
def builtinCatalog : Catalog := [machineTemplate, clientTemplate]

end ScopeTemplates
