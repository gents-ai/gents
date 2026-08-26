import Proofs.Basic
import Mathlib.Data.Finset.Basic

/-!
# Mailbox state

Payload-abstract model for the human-attention index.  `Item` models one
immutable envelope plus its mutable terminal projection.  `RegistryState`
models the collection-wide facts used by stamped create/idempotence without
pulling UI payload fields into the proof.
-/

namespace Mailbox

inductive Status where
  | open
  | acted
  | dismissed
  | expired
  deriving DecidableEq, Repr

namespace Status

def toDefraDB : Status → String
  | .open => "open"
  | .acted => "acted"
  | .dismissed => "dismissed"
  | .expired => "expired"

def fromDefraDB? : String → Option Status
  | "open" => some .open
  | "acted" => some .acted
  | "dismissed" => some .dismissed
  | "expired" => some .expired
  | _ => none

def terminal : Status → Bool
  | .open => false
  | .acted | .dismissed | .expired => true

theorem fromDefraDB_toDefraDB (status : Status) :
    fromDefraDB? status.toDefraDB = some status := by
  cases status <;> rfl

end Status

inductive Kind where
  | ask
  | gate
  | finished
  | failed
  | flag
  deriving DecidableEq, Repr

namespace Kind

def toDefraDB : Kind → String
  | .ask => "ask"
  | .gate => "gate"
  | .finished => "finished"
  | .failed => "failed"
  | .flag => "flag"

end Kind

inductive Handling where
  | ack
  | startRequest
  | writeDocument
  deriving DecidableEq, Repr

namespace Handling

def toDefraDB : Handling → String
  | .ack => "ack"
  | .startRequest => "start_request"
  | .writeDocument => "write_document"

end Handling

inductive SourceKind where
  | graph
  | session
  | agent
  | runtime
  | tool
  deriving DecidableEq, Repr

namespace SourceKind

def toDefraDB : SourceKind → String
  | .graph => "graph"
  | .session => "session"
  | .agent => "agent"
  | .runtime => "runtime"
  | .tool => "tool"

end SourceKind

/-- Immutable envelope identity relevant to safety and idempotence. -/
structure Identity where
  itemKey : String
  requesterDid : String
  agentDid : String
  sourceKind : SourceKind
  sourceId : String
  kind : Kind
  deriving DecidableEq, Repr

/-- One persisted mailbox row.  `resolvedDocId` abstracts the satisfying
AgentRequest/domain document and is empty for non-acted terminals. -/
structure Item where
  identity : Identity
  status : Status
  resolvedDocId : String
  deriving DecidableEq, Repr

/-- The owner-scoped prefix on which open-row retries coalesce. -/
structure OwnerPrefix where
  requesterDid : String
  sourceKind : SourceKind
  sourceId : String
  kind : Kind
  deriving DecidableEq, Repr

def Identity.ownerPrefix (identity : Identity) : OwnerPrefix :=
  { requesterDid := identity.requesterDid
  , sourceKind := identity.sourceKind
  , sourceId := identity.sourceId
  , kind := identity.kind
  }

/-- Runtime context that must stamp a create. -/
structure StampContext where
  requesterDid : String
  agentDid : String
  deriving DecidableEq, Repr

structure CreateRequest where
  identity : Identity
  context : StampContext
  deriving DecidableEq, Repr

/-- Collection-wide facts required by the create helper. `graphEdges` is kept
explicitly separate so close/create can be proven not to grant graph progress. -/
structure RegistryState where
  openPrefixes : Finset OwnerPrefix
  itemKeys : Finset String
  graphEdges : Finset String
  deriving DecidableEq

end Mailbox
