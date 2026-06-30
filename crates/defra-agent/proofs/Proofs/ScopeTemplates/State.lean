import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

/-!
# Scope Templates — State

A pure resolution model that sits *beside* the `PairingReconcile` reconciler
(the pattern is `PeerRegistryDiscovery`: a derivation below the machine, not a
new machine). A `ScopeTemplate` is a named pairing intent — a collection set, a
`Scope` (how per-peer document filtering is derived) and a `Delivery` (filtered
push vs. subscribe+replicate).

Mirrors the Rust `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`:
`ScopeTemplate { id, collections, scope, delivery }`, `Scope = PeerDid {field} |
Unscoped | PerCollection`, `Delivery = Push | Replicate`, and `scope_filter`.
The catalog is a static `&[ScopeTemplate]`; here it is a concrete
`builtinCatalog` plus a `List Template` type over which resolution is proven
deterministic and total.

`@immutable`/DAG-completeness is upstream's obligation (#1033); not modeled here.
-/

namespace ScopeTemplates

abbrev TemplateId := String
abbrev Did := String

/-- Delivery mode. `Push` = filtered replicator only (no subscription);
`Replicate` = subscribe + unfiltered replicator (whole-collection). -/
inductive Delivery where
  | push
  | replicate
  deriving DecidableEq, Repr

/-- Source of the DID value used in a per-collection filter rule. -/
inductive DidSource where
  | localDid
  | peerDid
  deriving DecidableEq, Repr

/-- One per-collection filter rule. -/
structure CollectionRule where
  collection : String
  field : String
  source : DidSource
  deriving DecidableEq, Repr

/-- Scoping policy. `PeerDid f` filters each collection on field `f` equal to the
peer's DID; `Unscoped` applies no filter; `PerCollection` carries exact
collection/field/source rules for directional pairings. -/
inductive Scope where
  | peerDid (field : String)
  | unscoped
  | perCollection (rules : List CollectionRule)
  deriving DecidableEq, Repr

/-- The scope filter key resolved from a scope against a concrete peer DID.
Byte-identical in shape to the predicate part of Rust
`FilterPredicate { field, value }`. -/
structure ScopeFilterKey where
  field : String
  value : Did
  deriving DecidableEq, Repr

/-- One per-collection filter resolved from a scope against a concrete peer DID.
Mirrors Rust `PairingFilters` entries: map key = `collection`, value =
`FilterPredicate { field, value }`. -/
structure CollectionScopeFilter where
  collection : String
  field : String
  value : Did
  deriving DecidableEq, Repr

/-- A named pairing intent. -/
structure Template where
  id : TemplateId
  collections : Finset String
  scope : Scope
  delivery : Delivery
  deriving DecidableEq

/-- The catalog: an ordered list of templates. Mirrors the static Rust
`&[ScopeTemplate]`. Resolution is `List.find?` by id, matching
`resolve_template`'s `iter().find(|t| t.id == id)`. -/
abbrev Catalog := List Template

def conversationCollections : List String :=
  ["AgentRequest", "AgentResponse", "AgentMessage", "AgentToolCall",
   "AgentToolResult", "AgentSession", "AgentConversation", "CompactionEntry"]

def agentConfigCollections : List String :=
  ["AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill"]

def discoveryCollections : List String :=
  ["AgentNetwork", "NetworkMembership", "PeerEndpoint", "NetworkJoinRequest",
   "AgentBehavior", "ToolSelection", "InferenceBackend", "InferenceProfile",
   "ToolServiceRegistry", "Skill"]

def networkControlCollections : List String :=
  ["AgentNetwork", "NetworkMembership", "PeerEndpoint", "NetworkJoinRequest"]

def subagentHostCollections : List String :=
  conversationCollections

def subagentCoordinatorRules : List CollectionRule :=
  [ { collection := "AgentRequest",  field := "agent_did",        source := .localDid }
  , { collection := "AgentToolCall", field := "spawn_target_did", source := .peerDid } ]

def subagentHostRules : List CollectionRule :=
  subagentHostCollections.map
    (fun c => { collection := c, field := "agent_did", source := .localDid })

def conversationTemplate : Template :=
  { id := "conversation"
  , collections := conversationCollections.toFinset
  , scope := .peerDid "agent_did"
  , delivery := .push }

def agentConfigTemplate : Template :=
  { id := "agent-config"
  , collections := agentConfigCollections.toFinset
  , scope := .unscoped
  , delivery := .replicate }

def backupTemplate : Template :=
  { id := "backup"
  , collections := conversationCollections.toFinset
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
  , collections := ["AgentRequest", "AgentToolCall"].toFinset
  , scope := .perCollection subagentCoordinatorRules
  , delivery := .push }

def subagentHostTemplate : Template :=
  { id := "subagent-host"
  , collections := subagentHostCollections.toFinset
  , scope := .perCollection subagentHostRules
  , delivery := .push }

/-- Concrete catalog mirroring Rust `BUILTIN_TEMPLATES`. -/
def builtinCatalog : Catalog :=
  [ conversationTemplate
  , agentConfigTemplate
  , backupTemplate
  , discoveryTemplate
  , networkControlTemplate
  , subagentCoordinatorTemplate
  , subagentHostTemplate ]

end ScopeTemplates
