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
  ["AgentRequest", "AgentMessage", "AgentToolCall", "AgentOutputSegment",
   "AgentSession", "CompactionEntry"]

/-- Configuration reachable from Agent -> AgentContext / InferenceProfile
and its referenced settings and documents: Tools, CompactionConfig, sampling,
execution, retry policy, MCP registries, skills, datastore surfaces, agent
targets, chain keys and eth tools. Excludes credential-bearing documents by
construction, so ordinary client/conversation transport cannot carry them. -/
def reachableConfigKinds : List ConfigDocuments.Collection :=
  [.agent, .agentContext, .compaction, .tools, .agentTarget,
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
  ["NodeReadiness"]

def clientToRuntimeCollections : List String :=
  clientTranscriptCollections ++
    ["PeerEndpoint", "SessionHydrationRequest"]

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
    ["MailboxItem", "SessionHydrationRequest", "NodeDirectoryEntry"]

/-- A remote `agent_new` is an ordinary Peer AgentRequest authored on the
caller node with `node_did = target` and `requester_did = caller`. The
coordinator leg (caller → host) therefore carries only that request, selected
by the target's `node_did`; no tool-call row is needed to name the host. -/
def agentTargetCallerCollections : List String :=
  ["AgentRequest"]

/-- The host leg (host → caller) returns the caused request, its session and
its transcript, selected by the caller's `requester_did`, so the result reaches
the originating session. The host's AgentToolCall rows are host-local execution
records and stay on the host, as do results, compaction and configuration. -/
def agentTargetHostCollections : List String :=
  ["AgentRequest", "AgentSession", "AgentOutputSegment", "AgentMessage"]

/-- The eager client index retains its existing requester scope. -/
def clientIndexCollections : List String :=
  ["AgentSession", "MailboxItem"]

def conversationRules : List CollectionRule :=
  [ { collection := "AgentRequest",    field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",    field := "requester_did", source := .peerDid }
  , { collection := "AgentToolCall",   field := "requester_did", source := .peerDid }
  , { collection := "AgentOutputSegment", field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",    field := "requester_did", source := .peerDid }
  , { collection := "CompactionEntry", field := "requester_did", source := .peerDid } ]

def machineRules : List CollectionRule :=
  conversationRules ++
    [ { collection := "MailboxItem", field := "requester_did", source := .peerDid }
    , { collection := "SessionHydrationRequest", field := "requester_did", source := .peerDid }
    , { collection := "NodeDirectoryEntry", field := "source_did", source := .homeDid } ]

def agentTargetCallerRules : List CollectionRule :=
  [ { collection := "AgentRequest", field := "node_did", source := .peerDid } ]

def agentTargetHostRules : List CollectionRule :=
  [ { collection := "AgentRequest",    field := "requester_did", source := .peerDid }
  , { collection := "AgentSession",    field := "requester_did", source := .peerDid }
  , { collection := "AgentOutputSegment", field := "requester_did", source := .peerDid }
  , { collection := "AgentMessage",    field := "requester_did", source := .peerDid } ]

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

def agentTargetCallerTemplate : Template :=
  { id := "agent-target-caller"
  , collections := agentTargetCallerCollections.toFinset
  , scope := .perCollection agentTargetCallerRules
  , delivery := .push }

def agentTargetHostTemplate : Template :=
  { id := "agent-target-host"
  , collections := agentTargetHostCollections.toFinset
  , scope := .perCollection agentTargetHostRules
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
  , agentTargetCallerTemplate
  , agentTargetHostTemplate
  , appCollectionsTemplate
  , clientIndexTemplate ]

end ScopeTemplates
