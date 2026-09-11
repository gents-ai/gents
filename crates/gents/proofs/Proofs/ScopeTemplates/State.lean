import Proofs.ConfigDocuments
import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

/-! Pairing scope templates. AgentSession is the single durable session
document. Ordinary
transcript/client routes never carry credential-bearing documents; backend and
OAuth credentials ride only a route whose grant carries an explicit
operator/runtime ACP authorization. Template names are not evidence of that
authorization. -/

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

/-- Canonical transcript artifacts rooted in AgentSession. -/
def transcriptCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "CompactionEntry"]

/-- Configuration reachable from AgentBehavior -> AgentContext / InferenceProfile
and its referenced settings and documents: Tools, CompactionConfig, sampling,
execution, retry policy, MCP registries, skills, datastore surfaces, subagent
targets, chain keys and eth tools. Excludes credential-bearing documents by
construction, so ordinary client/conversation transport cannot carry them. -/
def reachableConfigKinds : List ConfigDocuments.Collection :=
  [.agentBehavior, .agentContext, .compaction, .tools, .subagentTarget,
   .inferenceProfile, .inferenceSampling, .inferenceExecution,
   .inferenceRetryPolicy, .toolServiceRegistry, .skill, .datastoreToolSurface,
   .chainKeyBinding, .ethTool]

def agentConfigCollections : List String :=
  reachableConfigKinds.map ConfigDocuments.Collection.collectionName

/-- The full operator configuration plane adds the credential-bearing backend
document. Existing DID/ACP admission must authorize its operator/runtime
route; this template model describes selection, not authorization. -/
def operatorConfigCollections : List String :=
  agentConfigCollections ++ ["InferenceBackend"]

/-- Documents carrying credentials. `InferenceBackend` holds API-key material
and `OAuthCredential` holds principal login/refresh credentials; neither
belongs in ordinary client/conversation replication. -/
def credentialCollections : List String := ["InferenceBackend", "OAuthCredential"]

def conversationCollections : List String :=
  transcriptCollections ++ agentConfigCollections

def clientTranscriptCollections : List String :=
  transcriptCollections ++ ["MailboxItem"]

def clientOwnerProjectionCollections : List String :=
  ["AgentBehaviorReadiness"]

def clientToRuntimeCollections : List String :=
  clientTranscriptCollections ++
    ["PersonaConfigRequest", "PeerEndpoint", "SessionHydrationRequest"]

def clientControlPlaneCollections : List String :=
  agentConfigCollections ++
    ([.task, .schedule, .trigger, .eventSource] : List ConfigDocuments.Collection).map
      ConfigDocuments.Collection.collectionName

def clientCollections : List String :=
  clientToRuntimeCollections ++ clientControlPlaneCollections ++
    clientOwnerProjectionCollections

def clientRouteCollections : RouteDirection → List String
  | .clientToRuntime => clientToRuntimeCollections
  | .runtimeToClient => clientCollections

def machineCollections : List String :=
  conversationCollections ++
    ["MailboxItem", "SessionHydrationRequest", "AgentDirectoryEntry"]

/-- Subagent legs stay minimal requester-scoped transcript carriers; host-local
artifacts and configuration never ride them. -/
def subagentHostCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall"]

/-- The eager client index retains its existing requester scope. -/
def clientIndexCollections : List String :=
  ["AgentSession", "MailboxItem"]

def conversationRules : List CollectionRule :=
  [ { collection := "AgentRequest",    field := "requester_did", source := .peerDid }
  , { collection := "AgentResponse",   field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",    field := "requester_did", source := .peerDid }
  , { collection := "AgentToolCall",   field := "requester_did", source := .peerDid }
  , { collection := "AgentToolResult", field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",    field := "requester_did", source := .peerDid }
  , { collection := "CompactionEntry", field := "requester_did", source := .peerDid } ]

def machineRules : List CollectionRule :=
  conversationRules ++
    [ { collection := "MailboxItem", field := "requester_did", source := .peerDid }
    , { collection := "SessionHydrationRequest", field := "requester_did", source := .peerDid }
    , { collection := "AgentDirectoryEntry", field := "source_did", source := .homeDid } ]

def subagentCoordinatorRules : List CollectionRule :=
  [ { collection := "AgentToolCall", field := "spawn_target_did", source := .peerDid } ]

def subagentHostRules : List CollectionRule :=
  [ { collection := "AgentRequest",    field := "requester_did", source := .peerDid }
  , { collection := "AgentResponse",   field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",    field := "requester_did", source := .peerDid }
  , { collection := "AgentToolCall",   field := "requester_did", source := .peerDid } ]

def clientIndexRules : List CollectionRule :=
  [ { collection := "AgentSession", field := "requester_did", source := .peerDid }
  , { collection := "MailboxItem",  field := "requester_did", source := .peerDid } ]

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
  , collections := operatorConfigCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def backupTemplate : Template :=
  { id := "backup"
  , collections := transcriptCollections.toFinset
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
  , subagentCoordinatorTemplate
  , subagentHostTemplate
  , appCollectionsTemplate
  , clientIndexTemplate ]

end ScopeTemplates
