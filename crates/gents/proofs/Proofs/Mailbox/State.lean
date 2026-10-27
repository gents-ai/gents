import Proofs.Basic
import Mathlib.Data.Finset.Basic

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
  | session
  | agent
  | runtime
  | tool
  deriving DecidableEq, Repr

namespace SourceKind

def toDefraDB : SourceKind → String
  | .session => "session"
  | .agent => "agent"
  | .runtime => "runtime"
  | .tool => "tool"

end SourceKind

structure Identity where
  itemKey : String
  requesterDid : String
  nodeDid : String
  sourceKind : SourceKind
  sourceId : String
  kind : Kind
  deriving DecidableEq, Repr

structure Item where
  identity : Identity
  status : Status
  resolvedDocId : String
  deriving DecidableEq, Repr

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

structure StampContext where
  requesterDid : String
  nodeDid : String
  deriving DecidableEq, Repr

structure CreateRequest where
  identity : Identity
  context : StampContext
  docId : String := ""
  handling : Handling := .ack
  sessionId : String := ""
  requestId : RequestId := 0
  content : String := ""
  deriving DecidableEq, Repr

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

structure RegistryState where
  rows : List StoredRow := []
  deriving DecidableEq

def RegistryState.openPrefixes (state : RegistryState) : Finset OwnerPrefix :=
  ((state.rows.filter (·.isOpen)).map (·.envelope.identity.ownerPrefix)).toFinset

def RegistryState.itemKeys (state : RegistryState) : Finset String :=
  (state.rows.map (·.envelope.identity.itemKey)).toFinset

end Mailbox
