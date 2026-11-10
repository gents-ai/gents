import Proofs.ConfigDocuments
import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.List.Basic

import Proofs.Identity.State

/-! P2P routes exchange discovery and enrollment protocol, plus application
collections a node explicitly opts into for an enrolled peer. All other runtime
data is read and written through its authenticated database endpoint. Neither a
route name nor transport enrollment grants document access. -/

namespace ScopeTemplates

abbrev TemplateId := String
abbrev Did := String

/-- Every route pushes through a replicator; no surviving template subscribes. -/
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

/-- Application data replicates only where a node opted in for that peer: its
Agent-owned overlay supplies the collections, so the route itself carries none.

Accepted limits (maintainer decisions, 2026-11-09):

- Receiver consent is enrollment. The route exists only toward a peer that
  currently holds an approved enrollment in the sending node's network, which
  the peer obtained by requesting to join with that node's offer; the
  reconciler replaces the overlay's peer and address with that enrollment's,
  and the route closes when the enrollment is revoked or its lease expires.
  Enrollment does not imply that the peer wants any particular application
  collection, and the peer is not asked again: replicator pushes skip DefraDB's
  receiver-side collection check (external premise, pinned DefraDB). Receiver
  gating needs further design and would live in DefraDB.
- The opt-in covers this node's documents in the named collections, including
  documents written before it. Revoking stops new pushes; documents already
  delivered stay on the peer, because replicated data cannot be recalled.
- Peer schema compatibility is not checked before applying: preview reports
  only local collection identities, and a mismatch surfaces as failed delivery
  on the peer. It affects whether data arrives, not which data is sent.
- Which collections an Agent may pair is bounded only by `decideAppCollection`
  and the P2P mutation grant (`Identity.Configuration.toolOperationSources`);
  there is no per-collection pairing grant. -/
def appCollectionsTemplate : Template :=
  { id := "app-collections", collections := [], scope := .perCollection [],
    delivery := .push }

def builtinCatalog : Catalog := [machineTemplate, clientTemplate, appCollectionsTemplate]

/-- What admitting one application collection to a pairing reads. -/
structure AppCollectionFacts where
  managed : Bool
  hasPolicy : Bool
  deriving DecidableEq, Repr

inductive AppCollectionRefusal where
  | managed
  | policyProtected
  deriving DecidableEq, Repr

/-- Decision (maintainer, 2026-11-09): a node may replicate application data over
P2P on its own initiative, through an explicit per-peer opt-in. The database can
do it; the Engineer sets it up. Enrollment never starts it, and there is no
Gents-managed replication of application data.

Managed collections (protocol, runtime and credential documents) are refused
always, whatever grants or document ACP say; they keep their own exact routes or
stay local.

External premise (pinned DefraDB merge path): the receiver registers a replicated
document with its block signer as owner, and relationships do not replicate, so
on the receiver only that signer can read a replica of a policy-bearing
collection. Any local reader would need a receiving-side grant the policy never
delegated, for a relation name a foreign policy need not define. Replicating it
would deliver data nobody may read, and granting it would widen access, so a
collection with a document policy is refused rather than replicated. -/
def decideAppCollection (facts : AppCollectionFacts) : Option AppCollectionRefusal :=
  if facts.managed then some .managed
  else if facts.hasPolicy then some .policyProtected
  else none

end ScopeTemplates
