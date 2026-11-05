import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

namespace Identity

abbrev DID := String
abbrev AgentLabel := String
abbrev CreationId := String
abbrev NodeKey := String

/-- User signing keys remain on clients; desktop and CLI have distinct client identities.
Users sign configuration and plugin installation. A user or an authorized Agent
may create an Agent under its own identity; the runtime never signs as a user. The node key authenticates P2P
pairing and peer admission, creates and updates schemas and indexes, registers policies,
and owns node-local operational data whose cause is not
agent work (readiness, shared backend health, credential refresh). The Node configuration
resource remains user-owned; operational children inherit user access from it.
Clients explicitly create Node configuration. Runtime startup must report a missing
Node rather than auto-create it under the runtime key.
Agent-caused workspace, plugin, request and other effects always use that agent,
never runtime authority as an ACP-denial fallback. -/
structure Node where
  did         : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

/-- Schema and policy maintenance belongs to the runtime, independently of the
identity that owns any documents stored under those schemas. -/
inductive SchemaOperation where
  | createSchema | updateSchema | createIndex | registerPolicy
  deriving DecidableEq, Repr

def schemaActor (node : Node) (_operation : SchemaOperation) : DID := node.did

theorem schema_operations_use_runtime (node : Node) (operation : SchemaOperation) :
    schemaActor node operation = node.did := by rfl

/-- Agents are local to their node. Their DID is the primary identifier; `label` is a
human label and cannot select key material. Runtime-managed keys derive from the node
secret and an immutable random creation identifier allocated before document creation,
not from a label or a document hash. Renames preserve that identifier; a new creation
uses another identifier even when its label is reused. Deliberate restoration may
reuse an existing creation identifier and DID; deletion creates no permanent tombstone.
Fresh provisioning always allocates a new identity. A new key means a new did:key;
rotation is future work.
The runtime holds agent keys, so the boundary isolates agents
from one another, not from the runtime. Agent work, bookkeeping, callbacks and plugin
writes use the selected agent actor. Agent self-configuration requires an explicit user
grant to update its Agent document. Each agent owns the data it creates. Users own Node
configuration and transitively administer its Agents. User-created sessions are user-owned
with an explicit working-agent relationship; unattended sessions are agent-owned with
configured user access inherited through their Agent. Children remain agent-owned and
inherit user access and control through their parent session. -/
structure Agent where
  label       : AgentLabel
  did         : DID
  creationId  : CreationId
  node        : DID
  displayName : Option String
  enabled     : Bool
  deriving DecidableEq, Repr

/-- Only discovery and enrollment protocol records travel through P2P. Configuration,
requests, sessions, transcripts, tools, callbacks and all other runtime data stay on
that runtime. Clients query and subscribe to its authenticated database endpoint;
replica presence is never an alternative data source or an authorization grant.
Arbitrary application collections have no implicit replication path. PeerRegistry binds
a node DID to signed delivery endpoints and carries peer liveness and offered network
metadata. NodeDirectoryEntry publishes agent DIDs for discovery, not authority. -/
def replicatedProtocolCollections : List String :=
  ["PeerRegistry", "NodeDirectoryEntry", "Network",
   "NetworkEnrollmentRequest", "NetworkEnrollmentDecision",
   "NetworkAuthorizationRevision", "NetworkEnrollmentRouteReceipt"]

def collectionReplicates (collection : String) : Bool :=
  replicatedProtocolCollections.contains collection

/-- Representative local families; the positive protocol allowlist above is exhaustive. -/
def nodeLocalCollections : List String :=
  ["Agent", "CallbackBinding", "CallbackInvocation", "CallbackResult"]

theorem unlisted_collection_stays_local (collection : String)
    (unlisted : collection ∉ replicatedProtocolCollections) :
    collectionReplicates collection = false := by
  simp [collectionReplicates, unlisted]

structure World where
  nodes      : Finset Node
  agents     : Finset Agent

def World.WellFormed (w : World) : Prop :=
  (∀ p₁ ∈ w.nodes, ∀ p₂ ∈ w.nodes,
      p₁.did = p₂.did → p₁ = p₂) ∧
  (∀ b₁ ∈ w.agents, ∀ b₂ ∈ w.agents,
      b₁.node = b₂.node → b₁.creationId = b₂.creationId → b₁ = b₂) ∧
  (∀ b : Agent, b ∈ w.agents →
      b.node ∈ w.nodes.image (·.did)) ∧
  (∀ b ∈ w.agents, b.did ≠ b.node ∧ b.did ≠ "" ∧ b.creationId ≠ "")

instance (w : World) : Decidable w.WellFormed := by
  unfold World.WellFormed
  infer_instance

def findAgent? (agents : List Agent) (node : DID) (did : DID) : Option Agent :=
  match agents.filter (fun b => b.node == node && b.did == did) with
  | [agent] => some agent
  | _ => none

/-- Cryptographic collision resistance is represented by injectivity of the derived
DID in the node secret and random creation identifier. This is the external premise
of the abstract model, not a claim that finite cryptographic hashes are mathematically
injective. Native conformance checks derivation stability and domain separation.
Provisioning returns only the public creation nonce and derived DID to an
already authenticated client. It creates no database document and
confers no document permission. The client signs Agent configuration with its
own identity; the runtime never receives or uses the user's private key.
The required pack contract persists each authored agent slot's public creation
binding in the existing installation record. Upgrades must reuse that binding;
a fresh install must receive a new one. Explicit restoration can reuse an earlier
public binding; it is distinct from fresh provisioning. Slots select persisted bindings and never
derive keys. Native implementation and transaction conformance remain obligations. -/
structure KeyDerivation where
  derive : NodeKey → CreationId → DID
  injective : Function.Injective (fun input : NodeKey × CreationId => derive input.1 input.2)

def World.DerivationBound (world : World) (derivation : KeyDerivation)
    (keys : DID → NodeKey) : Prop :=
  ∀ agent ∈ world.agents, agent.did = derivation.derive (keys agent.node) agent.creationId

theorem different_creation_different_did (derivation : KeyDerivation)
    (key : NodeKey) (left right : CreationId) (different : left ≠ right) :
    derivation.derive key left ≠ derivation.derive key right := by
  intro same
  have inputs : (key, left) = (key, right) := derivation.injective same
  exact different (congrArg Prod.snd inputs)

theorem different_node_key_different_did (derivation : KeyDerivation)
    (left right : NodeKey) (creation : CreationId) (different : left ≠ right) :
    derivation.derive left creation ≠ derivation.derive right creation := by
  intro same
  have inputs : (left, creation) = (right, creation) := derivation.injective same
  exact different (congrArg Prod.fst inputs)

theorem derived_did_unique (world : World) (wellFormed : world.WellFormed)
    (derivation : KeyDerivation) (keys : DID → NodeKey) (keysUnique : Function.Injective keys)
    (bound : world.DerivationBound derivation keys) (left right : Agent)
    (hl : left ∈ world.agents) (hr : right ∈ world.agents)
    (same : left.did = right.did) : left = right := by
  rw [bound left hl, bound right hr] at same
  have inputs : (keys left.node, left.creationId) = (keys right.node, right.creationId) :=
    derivation.injective same
  exact wellFormed.2.1 left hl right hr
    (keysUnique (congrArg Prod.fst inputs)) (congrArg Prod.snd inputs)

end Identity
