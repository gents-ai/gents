import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

namespace ScopeTemplates

abbrev TemplateId := String
abbrev Did := String

inductive Delivery where
  | push
  | replicate
  deriving DecidableEq, Repr

inductive RouteDirection where
  | clientToRuntime
  | runtimeToClient
  deriving DecidableEq, Repr

inductive DidSource where
  | localDid
  | peerDid
  | homeDid
  deriving DecidableEq, Repr

structure CollectionRule where
  collection : String
  field : String
  source : DidSource
  deriving DecidableEq, Repr

inductive Scope where
  | peerDid (field : String)
  | unscoped
  | perCollection (rules : List CollectionRule)
  | clientRoute
  deriving DecidableEq, Repr

structure ScopeFilterKey where
  field : String
  operator : String := "_eq"
  value : Did
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
  collections : Finset String
  scope : Scope
  delivery : Delivery
  deriving DecidableEq

abbrev Catalog := List Template

def conversationTranscriptCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry",
   "BearerPairingReady"]

def agentConfigCollections : List String :=
  ["AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill", "DatastoreToolSurface"]

def conversationCollections : List String :=
  conversationTranscriptCollections ++ agentConfigCollections

def clientTranscriptCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry"]

def clientControlPlaneCollections : List String :=
  ["AgentBehavior", "ToolSelection", "InferenceProfile", "ToolServiceRegistry",
   "Skill", "DatastoreToolSurface", "Task", "Schedule", "EventTrigger"]

def clientToRuntimeCollections : List String :=
  clientTranscriptCollections ++ ["BearerPairingReady", "PeerEndpoint"]

def clientCollections : List String :=
  clientToRuntimeCollections ++ clientControlPlaneCollections

def clientRouteCollections : RouteDirection → List String
  | .clientToRuntime => clientToRuntimeCollections
  | .runtimeToClient => clientCollections

def machineCollections : List String :=
  conversationCollections ++ ["PersonaConfigRequest", "AgentDirectoryEntry"]

def discoveryCollections : List String :=
  ["AgentNetwork", "NetworkMembership", "PeerEndpoint", "NetworkJoinRequest",
   "AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill", "DatastoreToolSurface"]

def networkControlCollections : List String :=
  ["AgentNetwork", "NetworkMembership", "PeerEndpoint", "NetworkJoinRequest"]

def subagentHostCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall"]

def clientIndexCollections : List String :=
  ["AgentConversation", "AgentSession"]

def conversationRules : List CollectionRule :=
  [ { collection := "AgentRequest",      field := "requester_did", source := .peerDid }
  , { collection := "AgentResponse",     field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",      field := "requester_did", source := .peerDid }
  , { collection := "AgentToolCall",     field := "requester_did", source := .peerDid }
  , { collection := "AgentToolResult",   field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",      field := "requester_did", source := .peerDid }
  , { collection := "AgentConversation", field := "requester_did", source := .peerDid }
  , { collection := "CompactionEntry",   field := "requester_did", source := .peerDid }
  , { collection := "BearerPairingReady", field := "claimant_did", source := .peerDid } ]

def machineRules : List CollectionRule :=
  conversationRules ++
    [ { collection := "PersonaConfigRequest", field := "requester_did", source := .peerDid }
    , { collection := "AgentDirectoryEntry", field := "source_did", source := .homeDid } ]

def subagentCoordinatorRules : List CollectionRule :=
  [ { collection := "AgentToolCall", field := "spawn_target_did", source := .peerDid } ]

def subagentHostRules : List CollectionRule :=
  [ { collection := "AgentRequest",      field := "requester_did", source := .peerDid }
  , { collection := "AgentResponse",     field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",      field := "requester_did", source := .peerDid }
  , { collection := "AgentToolCall",     field := "requester_did", source := .peerDid } ]

def clientIndexRules : List CollectionRule :=
  [ { collection := "AgentConversation", field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",      field := "requester_did", source := .peerDid } ]

def conversationTemplate : Template :=
  { id := "conversation"
  , collections := conversationCollections.toFinset
  , scope := .perCollection conversationRules
  , delivery := .push }

def machineTemplate : Template :=
  { id := "machine"
  , collections := machineCollections.toFinset
  , scope := .perCollection machineRules
  , delivery := .push }

def clientTemplate : Template :=
  { id := "client"
  , collections := clientCollections.toFinset
  , scope := .clientRoute
  , delivery := .push }

def agentConfigTemplate : Template :=
  { id := "agent-config"
  , collections := agentConfigCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def backupTemplate : Template :=
  { id := "backup"
  , collections := conversationTranscriptCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def discoveryTemplate : Template :=
  { id := "discovery"
  , collections := discoveryCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def networkControlTemplate : Template :=
  { id := "network-control"
  , collections := networkControlCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def subagentCoordinatorTemplate : Template :=
  { id := "subagent-coordinator"
  , collections := ["AgentToolCall"].toFinset
  , scope := .perCollection subagentCoordinatorRules
  , delivery := .push }

def subagentHostTemplate : Template :=
  { id := "subagent-host"
  , collections := subagentHostCollections.toFinset
  , scope := .perCollection subagentHostRules
  , delivery := .push }

def appCollectionsTemplate : Template :=
  { id := "app-collections"
  , collections := (∅ : Finset String)
  , scope := .unscoped
  , delivery := .replicate }

/-- Bring-your-own app collections are admitted only outside the protocol
catalog. This keeps the extensible app data plane disjoint from schemas whose
migration compatibility is owned by the runtime and bundled clients. -/
def admitAppCollections
    (protocolCatalog requested : Finset String) : Option (Finset String) :=
  if requested.Nonempty ∧ Disjoint requested protocolCatalog
  then some requested
  else none

def clientIndexTemplate : Template :=
  { id := "client-index"
  , collections := clientIndexCollections.toFinset
  , scope := .perCollection clientIndexRules
  , delivery := .push }

def builtinCatalog : Catalog :=
  [ conversationTemplate
  , machineTemplate
  , clientTemplate
  , agentConfigTemplate
  , backupTemplate
  , discoveryTemplate
  , networkControlTemplate
  , subagentCoordinatorTemplate
  , subagentHostTemplate
  , appCollectionsTemplate
  , clientIndexTemplate ]

end ScopeTemplates
