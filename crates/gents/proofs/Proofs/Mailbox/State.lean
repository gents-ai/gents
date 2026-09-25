import Proofs.Basic
import Mathlib.Data.Finset.Basic

/-!
# Mailbox state

Model for the human-attention index. `Item` models one immutable identity plus
its mutable terminal projection. `RegistryState` keeps one row collection;
open-prefix and key observations are derived from it.
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
  /-- Physical document identity allocated by the storage create owner. -/
  docId : String := ""
  handling : Handling := .ack
  sessionId : String := ""
  requestId : RequestId := 0
  content : String := ""
  deriving DecidableEq, Repr

/-- Immutable fields of a row actually written by the stamped-create owner. -/
structure StoredEnvelope where
  docId : String
  identity : Identity
  handling : Handling
  sessionId : String
  requestId : RequestId
  content : String
  deriving DecidableEq, Repr

structure StoredRow where
  envelope : StoredEnvelope
  isOpen : Bool
  deriving DecidableEq, Repr

def CreateRequest.storedEnvelope (request : CreateRequest) : StoredEnvelope :=
  { docId := request.docId
  , identity := request.identity
  , handling := request.handling
  , sessionId := request.sessionId
  , requestId := request.requestId
  , content := request.content }

/-- The one stored collection used by stamped create and open-row reuse. -/
structure RegistryState where
  rows : List StoredRow := []
  deriving DecidableEq

def RegistryState.openPrefixes (state : RegistryState) : Finset OwnerPrefix :=
  ((state.rows.filter (·.isOpen)).map (·.envelope.identity.ownerPrefix)).toFinset

def RegistryState.itemKeys (state : RegistryState) : Finset String :=
  (state.rows.map (·.envelope.identity.itemKey)).toFinset

end Mailbox
