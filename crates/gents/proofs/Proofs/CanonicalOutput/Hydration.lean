import Proofs.CanonicalOutput.Message
import Mathlib.Data.List.Dedup

/-!
# Authorized session-hydration closure

Hydration starts from exact messages authorized by the session owner. It
validates each native envelope through shared reconstruction, then adds its
immutable origin messages, closing records, exact extent segments and the
request/tool provenance derived from those records. Mutable provenance enters
only after its existing owner returns an explicit access observation.

There is deliberately no input for delegated-call provenance. A remote
`delegated_input.source` explains copied arguments; it is not a capability,
hydration root, or grant of a parent output document.
-/
namespace CanonicalOutput.Hydration

inductive Collection where
  | agentRequest | agentMessage | agentToolCall | agentOutputSegment | compactionEntry
  deriving DecidableEq, Repr

structure DocumentKey where
  collection : Collection
  id : DocId
  deriving DecidableEq, Repr

/-- Separately selected session roots have already passed their existing
owners. They cannot satisfy provenance derived from a payload reference. -/
inductive AuthorizedBase where
  | request (id : DocId)
  | toolCall (id : DocId)
  | compaction (id : DocId)
  deriving DecidableEq, Repr

def AuthorizedBase.key : AuthorizedBase → DocumentKey
  | .request id => ⟨.agentRequest, id⟩
  | .toolCall id => ⟨.agentToolCall, id⟩
  | .compaction id => ⟨.compactionEntry, id⟩

def Collection.rank : Collection → Nat
  | .agentRequest => 0 | .agentMessage => 1 | .agentToolCall => 2
  | .agentOutputSegment => 3 | .compactionEntry => 4

def keyBefore (left right : DocumentKey) : Bool :=
  left.collection.rank < right.collection.rank ||
    (left.collection.rank == right.collection.rank && left.id < right.id)

def insertKey (key : DocumentKey) : List DocumentKey → List DocumentKey
  | [] => [key]
  | head :: tail =>
      if key = head then head :: tail
      else if keyBefore key head then key :: head :: tail
      else head :: insertKey key tail

def canonicalManifest : List DocumentKey → List DocumentKey
  | [] => []
  | head :: tail => insertKey head (canonicalManifest tail)

theorem mem_insertKey_iff (candidate key : DocumentKey) (keys : List DocumentKey) :
    candidate ∈ insertKey key keys ↔ candidate = key ∨ candidate ∈ keys := by
  induction keys with
  | nil => simp [insertKey]
  | cons head tail ih =>
      simp only [insertKey]
      split
      · subst key; simp
      · split <;> simp [ih, or_assoc, or_left_comm]

theorem mem_canonicalManifest_iff (key : DocumentKey) (keys : List DocumentKey) :
    key ∈ canonicalManifest keys ↔ key ∈ keys := by
  induction keys with
  | nil => simp [canonicalManifest]
  | cons head tail ih => simp [canonicalManifest, mem_insertKey_iff, ih, eq_comm]

structure TerminalRequirement where
  request : DocId
  session : SessionId
  selection : Option TerminalSelection
  deriving DecidableEq, Repr

inductive AccessState where
  | authorized | missing | denied
  deriving DecidableEq, Repr

structure AuthorizationScope where
  peer : String
  requester : String
  agent : String
  session : String
  nativeSession : SessionId
  deriving DecidableEq, Repr

/-- One observation returned by the existing document ACP owner.  Scope is
part of the observation: evidence obtained for one hydration request cannot be
replayed for another request, even when both requests name the same session. -/
structure ProvenanceAccess where
  scope : AuthorizationScope
  key : DocumentKey
  state : AccessState
  deriving DecidableEq, Repr

inductive Error where
  | headerUnavailable | headerDenied | conflictingMessages | invalidOrigin | originMismatch
  | rootSessionMismatch
  | provenanceMissing (key : DocumentKey)
  | provenanceDenied (key : DocumentKey)
  | provenanceConflict (key : DocumentKey)
  | reference (error : ReconstructionError)
  | message (error : MessageError)
  | terminal (error : TerminalError)
  deriving DecidableEq, Repr

def lookupMessage (messages : List MessageEnvelope) (denied : List DocId) (id : DocId) :
    Except Error MessageEnvelope :=
  if id ∈ denied then .error .headerDenied
  else match uniqueRecord Error.headerUnavailable .conflictingMessages
      (messages.filter (fun message => message.header.id == id)) with
    | .ok message =>
        if messages.any (fun other => other != message &&
            other.header.session == message.header.session &&
            (other.key == message.key || other.sequence == message.sequence)) then
          .error .conflictingMessages
        else .ok message
    | .error error => .error error

def checkedOrigin (header : Header) : Except Error (Option DocId) :=
  match header.publication with
  | .fork origin =>
      if header.origin = some origin then .ok (some origin) else .error .invalidOrigin
  | _ => if header.origin = none then .ok none else .error .invalidOrigin

/-- A fork may change child identity, session and key only. Numeric sequence
is retained in the child session, exactly as the fork publication owner writes
it; readers cannot authorize an arbitrary reorder of individually valid copies. Native
content, source references, outcome, role and creation provenance remain the
origin facts. -/
def forkMetadataMatches (child origin : MessageEnvelope) : Bool :=
  child.header == forkHeader origin.header child.header.id child.header.session &&
    child.nativeId == origin.nativeId && child.blocks == origin.blocks &&
    child.createdAt == origin.createdAt && child.sequence == origin.sequence

theorem fork_metadata_preserves_sequence (child origin : MessageEnvelope)
    (h : forkMetadataMatches child origin = true) : child.sequence = origin.sequence := by
  simp only [forkMetadataMatches, Bool.and_eq_true, beq_iff_eq] at h
  exact h.2

theorem reordered_fork_rejected (child origin : MessageEnvelope)
    (h : child.sequence ≠ origin.sequence) : forkMetadataMatches child origin = false := by
  simp [forkMetadataMatches, h]

/-- Every referenced physical request is retained. Tool-owned sources also
retain the exact lifecycle row; authored delivery derives its owner from the
writer. Shared reconstruction rejects incoherent source/writers. -/
def requiredProvenance (closing : Segment) : List DocumentKey :=
  let request : DocumentKey := ⟨.agentRequest, closing.coordinate.request⟩
  match closing.coordinate.source, closing.writer with
  | .tool call, _ => [request, ⟨.agentToolCall, call⟩]
  | .authored _, .tool call => [request, ⟨.agentToolCall, call⟩]
  | _, _ => [request]

def headerProvenance (header : Header) : List DocumentKey :=
  let request := header.request.toList.map fun id => ⟨Collection.agentRequest, id⟩
  match header.publication with
  | .toolDelivery call => request ++ [⟨.agentToolCall, call⟩]
  | _ => request

def validateProvenance (scope : AuthorizationScope)
    (observations : List ProvenanceAccess) (key : DocumentKey) :
    Except Error DocumentKey :=
  match uniqueRecord (Error.provenanceMissing key) (Error.provenanceConflict key)
      (observations.filter (fun observation => observation.scope == scope &&
        observation.key == key)) with
  | .error error => .error error
  | .ok observation => match observation.state with
      | .authorized => .ok key
      | .missing => .error (.provenanceMissing key)
      | .denied => .error (.provenanceDenied key)

def referenceDocuments (scope : AuthorizationScope) (segments : List Segment) (deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial) (provenance : List ProvenanceAccess)
    (ref : PayloadRef) : Except Error (List DocumentKey) := do
  let _ ← match reconstructPayload segments deniedSegments ref dependencyDenials with
    | .ok payload => .ok payload
    | .error error => .error (.reference error)
  let closing ← match resolveClose segments deniedSegments ref with
    | .ok record => .ok record
    | .error error => .error (.reference (.lookup error))
  let owners ← (requiredProvenance closing).mapM (validateProvenance scope provenance)
  let closingKey ← validateProvenance scope provenance
    ⟨Collection.agentOutputSegment, closing.id⟩
  match closing.close with
  | some (.closed _ count _) =>
      let extentKeys := (extent segments closing.coordinate count).map
        (fun segment => ⟨Collection.agentOutputSegment, segment.id⟩)
      let authorizedExtent ← extentKeys.mapM (validateProvenance scope provenance)
      .ok (owners ++ closingKey :: authorizedExtent)
  | _ => .error (.reference (.lookup .invalidReference))

def referenceListDocuments (scope : AuthorizationScope) (segments : List Segment) (deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial) (provenance : List ProvenanceAccess) :
    List PayloadRef → Except Error (List DocumentKey)
  | [] => .ok []
  | ref :: rest => do
      let here ← referenceDocuments scope segments deniedSegments dependencyDenials provenance ref
      let later ← referenceListDocuments scope segments deniedSegments dependencyDenials provenance rest
      .ok (here ++ later)

def terminalRoots (targetSession : SessionId) (messages : List MessageEnvelope)
    (deniedHeaders : List DocId) :
    List TerminalRequirement → Except Error (List DocId)
  | [] => .ok []
  | requirement :: rest => do
      if requirement.session != targetSession then .error .rootSessionMismatch else pure ()
      let selected ← match resolveTerminal (messages.map (·.header)) deniedHeaders
          requirement.request requirement.session requirement.selection with
        | .ok selected => .ok selected
        | .error error => .error (.terminal error)
      let later ← terminalRoots targetSession messages deniedHeaders rest
      match selected with
      | none => .ok later
      | some header => .ok (header.id :: later)

/-- Depth-first origin traversal rejects an ID already on the active path. An
ID visited through a completed sibling/root is shared DAG data and is skipped.
Native immutable identity is the premise behind document IDs as vertices. -/
def collectOne (scope : AuthorizationScope) (messages : List MessageEnvelope) (segments : List Segment)
    (provenance : List ProvenanceAccess) (deniedHeaders deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial) : Nat → DocId → List DocId →
    List DocId → List DocumentKey → Except Error (List DocId × List DocumentKey)
  | 0, _, _, _, _ => .error .invalidOrigin
  | fuel + 1, id, active, visited, manifest =>
      if id ∈ active then .error .invalidOrigin
      else if id ∈ visited then .ok (visited, manifest)
      else do
        let message ← lookupMessage messages deniedHeaders id
        let messageKey ← validateProvenance scope provenance ⟨Collection.agentMessage, id⟩
        let origin ← checkedOrigin message.header
        match origin with
        | some originId =>
            let originMessage ← lookupMessage messages deniedHeaders originId
            if !forkMetadataMatches message originMessage then .error .originMismatch
            else pure ()
        | none => pure ()
        let headerOwners ← (headerProvenance message.header).mapM
          (validateProvenance scope provenance)
        let payload ← referenceListDocuments scope segments deniedSegments dependencyDenials
          provenance message.header.refs
        let _ ← (reconstructMessage segments deniedSegments message).mapError Error.message
        let visited := id :: visited
        let manifest := messageKey :: headerOwners ++ payload ++ manifest
        match origin with
        | none => .ok (visited, manifest)
        | some originId =>
            collectOne scope messages segments provenance deniedHeaders deniedSegments dependencyDenials
              fuel originId (id :: active) visited manifest

def collectRoots (scope : AuthorizationScope) (messages : List MessageEnvelope) (segments : List Segment)
    (provenance : List ProvenanceAccess) (deniedHeaders deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial) (fuel : Nat) :
    List DocId → List DocId → List DocumentKey → Except Error (List DocumentKey)
  | [], _, manifest => .ok manifest
  | root :: rest, visited, manifest => do
      let (visited, manifest) ← collectOne scope messages segments provenance deniedHeaders
        deniedSegments dependencyDenials fuel root [] visited manifest
      collectRoots scope messages segments provenance deniedHeaders deniedSegments
        dependencyDenials fuel rest visited manifest

def validateRootSession (targetSession : SessionId) (messages : List MessageEnvelope)
    (deniedHeaders : List DocId) (id : DocId) : Except Error DocId := do
  let message ← lookupMessage messages deniedHeaders id
  if message.header.session = targetSession then pure id else .error .rootSessionMismatch

def buildManifest (scope : AuthorizationScope) (targetSession : SessionId)
    (bases : List AuthorizedBase) (authorizedRootMessages : List DocId)
    (requirements : List TerminalRequirement) (messages : List MessageEnvelope)
    (segments : List Segment) (provenance : List ProvenanceAccess)
    (deniedHeaders deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial := []) : Except Error (List DocumentKey) := do
  if scope.nativeSession != targetSession then .error .rootSessionMismatch else pure ()
  let explicitRoots ← authorizedRootMessages.mapM
    (validateRootSession targetSession messages deniedHeaders)
  let selectedRoots ← terminalRoots targetSession messages deniedHeaders requirements
  let baseKeys ← (bases.map AuthorizedBase.key).mapM (validateProvenance scope provenance)
  let terminalOwners ← (requirements.map fun requirement =>
    ⟨Collection.agentRequest, requirement.request⟩).mapM
      (validateProvenance scope provenance)
  let roots := (explicitRoots ++ selectedRoots).dedup
  let closure ← collectRoots scope messages segments provenance deniedHeaders deniedSegments
    dependencyDenials (messages.length + 1) roots [] []
  .ok (canonicalManifest (baseKeys ++ terminalOwners ++ closure))

def canDelete (manifest : List DocumentKey) (key : DocumentKey) : Bool :=
  !manifest.contains key

theorem retained_document_cannot_be_deleted (manifest : List DocumentKey)
    (key : DocumentKey) (h : key ∈ manifest) : canDelete manifest key = false := by
  simp [canDelete, List.contains_eq_mem, h]

theorem denied_message_is_not_missing (messages : List MessageEnvelope)
    (denied : List DocId) (id : DocId) (h : id ∈ denied) :
    lookupMessage messages denied id = .error .headerDenied := by
  simp [lookupMessage, h]

theorem absent_message_is_unavailable (id : DocId) :
    lookupMessage [] [] id = .error .headerUnavailable := by rfl

theorem delegated_provenance_is_not_a_base (key : AuthorizedBase) :
    key.key.collection ≠ .agentMessage ∧ key.key.collection ≠ .agentOutputSegment := by
  cases key <;> simp [AuthorizedBase.key]

namespace Examples

def resultMatches (actual expected : Except Error (List DocumentKey)) : Bool :=
  match actual, expected with
  | .ok left, .ok right => decide (left = right)
  | .error left, .error right => decide (left = right)
  | _, _ => false

def closing : Segment :=
  { id := 100, coordinate := ⟨10, .provider 0 0 0⟩, writer := .request 7
  , flush := some
      { ordinal := 0
      , runs := [
          { stream := 0
          , bytes := 1
          , declaration := some { block := 0, part := 0, kind := .text } }]
      , payload := [65] }
  , close := some (.closed .complete 1 [1]), createdAt := 1 }

def originMessage : MessageEnvelope :=
  { header :=
      { id := 200, session := 1, request := some 10, origin := none
      , refs := [⟨100, 0⟩], outcome := .complete, role := .assistant
      , publication := .requestExecution 7 }
  , key := "origin", sequence := 0, nativeId := some "native"
  , blocks := [.text ⟨⟨100, 0⟩, .full⟩], createdAt := 2 }

def childMessage : MessageEnvelope :=
  { originMessage with header := forkHeader originMessage.header 201 2, key := "child" }

def scope : AuthorizationScope := ⟨"peer-a", "requester-a", "agent-a", "session-a", 2⟩

def access : List ProvenanceAccess :=
  [ ⟨scope, ⟨.agentRequest, 10⟩, .authorized⟩
  , ⟨scope, ⟨.agentMessage, 200⟩, .authorized⟩
  , ⟨scope, ⟨.agentMessage, 201⟩, .authorized⟩
  , ⟨scope, ⟨.agentOutputSegment, 100⟩, .authorized⟩ ]

def originScope : AuthorizationScope := { scope with nativeSession := 1 }
def originAccess : List ProvenanceAccess :=
  access.map fun observation => { observation with scope := originScope }

example : resultMatches
    (buildManifest scope 2 [] [201] [] [originMessage, childMessage] [closing] access [] [])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentMessage, 200⟩, ⟨.agentMessage, 201⟩,
      ⟨.agentOutputSegment, 100⟩]) = true := by native_decide

example : resultMatches
    (buildManifest scope 1 [] [201] [] [originMessage, childMessage] [closing] access [] [])
    (.error .rootSessionMismatch) = true := by native_decide

def originDependenciesProtected : Bool :=
  match buildManifest scope 2 [] [201] [] [originMessage, childMessage] [closing] access [] [] with
  | .ok manifest => !canDelete manifest ⟨.agentMessage, 200⟩ &&
      !canDelete manifest ⟨.agentOutputSegment, 100⟩ &&
      !canDelete manifest ⟨.agentRequest, 10⟩
  | .error _ => false

example : originDependenciesProtected = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [201] [] [childMessage] [closing] access [] [])
    (.error .headerUnavailable) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [201] [] [originMessage, childMessage] [closing] access [200] [])
    (.error .headerDenied) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing]
      (originAccess.map fun observation => if observation.key = ⟨.agentMessage, 200⟩
        then { observation with state := .denied } else observation) [] [])
    (.error (.provenanceDenied ⟨.agentMessage, 200⟩)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing]
      (originAccess.map fun observation => if observation.key = ⟨.agentOutputSegment, 100⟩
        then { observation with state := .denied } else observation) [] [])
    (.error (.provenanceDenied ⟨.agentOutputSegment, 100⟩)) = true := by native_decide

/-- Evidence for a hydration twin is not evidence for this request. -/
example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing]
      (originAccess.map fun observation =>
        { observation with scope := { originScope with peer := "peer-b" } }) [] [])
    (.error (.provenanceMissing ⟨.agentMessage, 200⟩)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing] [] [] [])
    (.error (.provenanceMissing ⟨.agentMessage, 200⟩)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing]
      [⟨originScope, ⟨.agentMessage, 200⟩, .authorized⟩,
       ⟨originScope, ⟨.agentRequest, 10⟩, .denied⟩] [] [])
    (.error (.provenanceDenied ⟨.agentRequest, 10⟩)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [] originAccess [] [])
    (.error (.reference (.lookup .unavailable))) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing] originAccess [] [100])
    (.error (.reference (.lookup .denied))) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [200] [] [originMessage] [closing] originAccess [] [] [⟨100, 999⟩])
    (.error (.reference (.lookup .denied))) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some (.message 999)⟩]
      [originMessage] [closing] originAccess [] [])
    (.error (.terminal .missingHeader)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some (.message 200)⟩]
      [originMessage] [closing] originAccess [] [])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentMessage, 200⟩,
      ⟨.agentOutputSegment, 100⟩]) = true := by native_decide

/-- Explicit `NoMessage` still retains and authorizes the physical terminal
request; it is not an empty, ownerless manifest. -/
example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some .noMessage⟩] [] [] originAccess [] [])
    (.ok [⟨.agentRequest, 10⟩]) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some .noMessage⟩] [] [] [] [] [])
    (.error (.provenanceMissing ⟨.agentRequest, 10⟩)) = true := by native_decide

def cycleLeft : MessageEnvelope :=
  { childMessage with header :=
      { childMessage.header with id := 210, origin := some 211, publication := .fork 211 } }
def cycleRight : MessageEnvelope :=
  { childMessage with
    header := { childMessage.header with
      id := 211, session := 1, origin := some 210, publication := .fork 210 }
    key := "cycle-right" }

/-- Per-header content equality cannot grant a reordered child transcript. -/
example : resultMatches
    (buildManifest scope 2 [] [201] []
      [originMessage, { childMessage with sequence := childMessage.sequence + 1 }]
      [closing] access [] []) (.error .originMismatch) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [210] [] [cycleLeft, cycleRight] [closing]
      (access ++ [⟨scope, ⟨.agentMessage, 210⟩, .authorized⟩,
        ⟨scope, ⟨.agentMessage, 211⟩, .authorized⟩]) [] [])
    (.error .invalidOrigin) = true := by native_decide

def sameKeyTwin : MessageEnvelope :=
  { originMessage with header := { originMessage.header with id := 299 }, sequence := 1 }

def sameSequenceTwin : MessageEnvelope :=
  { originMessage with header := { originMessage.header with id := 298 }, key := "other" }

def isMessageConflict : Except Error MessageEnvelope → Bool
  | .error .conflictingMessages => true
  | _ => false

example : isMessageConflict (lookupMessage [originMessage, sameKeyTwin] []
    originMessage.header.id) = true := by native_decide

example : isMessageConflict (lookupMessage [originMessage, sameSequenceTwin] []
    originMessage.header.id) = true := by native_decide

end Examples
end CanonicalOutput.Hydration
