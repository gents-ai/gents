import Proofs.ConfigDocuments
import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

/-! Pairing scope templates. Ordinary transcript/client routes never carry
credential-bearing documents; backend and OAuth credentials ride only a route
whose grant carries an explicit operator/runtime ACP authorization. Template
names are not evidence of that authorization. -/

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

/-- These records travel through filtered control routes or explicit document
push. Whole-collection subscriptions would bypass the file-review selection
boundary and eagerly hydrate retained images. -/
def explicitDeliveryCollections : List String :=
  ["ToolFileChange", "ToolFileContent", "ToolFileInventory", "FileImageHydrationRequest",
   "FileInventoryRequest", "FileInventoryPage"]

def transcriptCollections : List String :=
  ["AgentRequest", "AgentMessage", "AgentToolCall", "AgentOutputSegment",
   "AgentSession", "CompactionEntry"]

/-- Excludes credential-bearing documents, so ordinary client/conversation
transport cannot carry them. -/
def reachableConfigKinds : List ConfigDocuments.Collection :=
  [.agent, .agentContext, .compaction, .tools, .agentTarget,
   .inferenceProfile, .inferenceSampling, .inferenceExecution,
   .inferenceRetryPolicy, .toolServiceRegistry, .skill, .datastoreToolSurface,
   .chainKeyBinding, .ethTool]

def agentConfigCollections : List String :=
  reachableConfigKinds.map ConfigDocuments.Collection.collectionName

/-- Credential-bearing; neither belongs in ordinary client/conversation
replication. Names are not evidence of authorization. -/
def operatorConfigCollections : List String :=
  agentConfigCollections ++ ["InferenceBackend"]

def credentialCollections : List String := ["InferenceBackend", "OAuthCredential"]

def conversationCollections : List String :=
  transcriptCollections ++ agentConfigCollections

def clientTranscriptCollections : List String :=
  transcriptCollections ++ ["MailboxItem"]

def clientOwnerProjectionCollections : List String :=
  ["NodeReadiness", "GoalObservation"]

def clientToRuntimeCollections : List String :=
  clientTranscriptCollections ++
    ["PeerEndpoint", "SessionHydrationRequest", "FileImageHydrationRequest", "FileInventoryRequest",
     "AgentSessionInputEdit", "AgentSessionGoalControl"]

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
    ["MailboxItem", "SessionHydrationRequest", "AgentSessionInputEdit", "AgentSessionGoalControl", "NodeDirectoryEntry"]

def agentTargetCallerCollections : List String :=
  ["AgentRequest"]

def agentTargetHostCollections : List String :=
  ["AgentRequest", "AgentSession", "AgentOutputSegment", "AgentMessage"]

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
    , { collection := "AgentSessionInputEdit", field := "requester_did", source := .peerDid }
    , { collection := "AgentSessionGoalControl", field := "requester_did", source := .peerDid }
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
