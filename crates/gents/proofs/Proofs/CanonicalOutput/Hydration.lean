import Proofs.CanonicalOutput.Message
import Proofs.ToolPolicy.PluginCalls
import Mathlib.Data.List.Dedup
import Mathlib.Order.Interval.Finset.Nat

/-!
Authorized session-hydration closure. Hydration roots come from the session
owner; a request's `caused_by_parent_*` lineage is not a capability or hydration
root. Composed plugin children are retained only through a tool call already in
the closure, via their single persisted-provenance decoder.
-/
namespace CanonicalOutput.Hydration

inductive Collection where
  | agentRequest | agentMessage | agentToolCall | agentOutputSegment | compactionEntry
  deriving DecidableEq, Repr

structure DocumentKey where
  collection : Collection
  id : DocId
  deriving DecidableEq, Repr

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
  state : RequestState := .completed
  deriving DecidableEq, Repr

inductive AccessState where
  | authorized | missing | denied
  deriving DecidableEq, Repr

structure AuthorizationScope where
  peer : String
  requester : String
  node : String
  session : String
  nativeSession : SessionId
  deriving DecidableEq, Repr

/-- One observation returned by the existing document ACP owner. The scope is
part of the observation and cannot be replayed for a different request. -/
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
          requirement.request requirement.session
            (observeTerminalSelection requirement.state requirement.selection) with
        | .ok selected => .ok selected
        | .error error => .error (.terminal error)
      let later ← terminalRoots targetSession messages deniedHeaders rest
      match selected with
      | none => .ok later
      | some header => .ok (header.id :: later)

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

structure EffectRow where
  id : DocId
  parent : Option DocId
  ordinal : Option Nat
  spawned : Bool
  toolCallId : String
  terminal : Option PayloadRef := none
  deriving DecidableEq, Repr

def effectIdentity (parent : DocId) (ordinal : Nat) : String :=
  "plugin-effect:" ++ toString parent ++ ":" ++ toString ordinal

def decodeEffect (row : EffectRow) : Option (DocId × Nat) :=
  match row.parent, row.ordinal with
  | some parent, some ordinal =>
      if 0 < ordinal && ordinal ≤ ToolPolicy.PluginCalls.maxEffects && !row.spawned &&
          row.toolCallId == effectIdentity parent ordinal then some (parent, ordinal)
      else none
  | _, _ => none

def isEffect (effects : List EffectRow) (id : DocId) : Bool :=
  effects.any fun row => row.id == id && (decodeEffect row).isSome

def selectedChildren (effects : List EffectRow) (retained : List DocumentKey) :
    List EffectRow :=
  effects.dedup.filter fun row => match decodeEffect row with
    | some (parent, _) =>
        retained.contains ⟨.agentToolCall, parent⟩ && !isEffect effects parent
    | none => false

def observedEffects (scope : AuthorizationScope) (provenance : List ProvenanceAccess)
    (effects : List EffectRow) : List EffectRow :=
  effects.filter fun row =>
    match validateProvenance scope provenance ⟨.agentToolCall, row.id⟩ with
    | .ok _ => true
    | .error _ => false

def childrenCoherent (children : List EffectRow) : Bool :=
  decide (children.map decodeEffect).Nodup && decide (children.map (·.id)).Nodup

def childConflict (children : List EffectRow) : DocId :=
  ((children.find? fun row => children.any fun other =>
    other != row && (other.id == row.id || decodeEffect other == decodeEffect row)).map
      (·.id)).getD 0

def effectArguments (segments : List Segment) (child : DocId) : Except Error PayloadRef :=
  match uniqueRecord (Error.reference (.lookup .unavailable))
      (Error.reference (.lookup .conflictingClosures))
      (segments.filter fun segment =>
        segment.coordinate.source == .toolEffectArguments child && segment.close.isSome) with
  | .ok closing => .ok ⟨closing.id, 0⟩
  | .error error => .error error

def effectTerminal (segments : List Segment) (deniedSegments : List DocId) (child : DocId)
    (reference : PayloadRef) : Except Error PayloadRef :=
  match resolveClose segments deniedSegments reference with
  | .ok closing =>
      if closing.coordinate.source == .tool child then .ok reference
      else .error (.reference (.lookup .invalidReference))
  | .error error => .error (.reference (.lookup error))

def effectDocuments (scope : AuthorizationScope) (segments : List Segment)
    (deniedSegments : List DocId) (dependencyDenials : List DependencyDenial)
    (provenance : List ProvenanceAccess) (row : EffectRow) :
    Except Error (List DocumentKey) := do
  let arguments ← effectArguments segments row.id
  let terminal ← row.terminal.toList.mapM (effectTerminal segments deniedSegments row.id)
  let payload ← referenceListDocuments scope segments deniedSegments dependencyDenials
    provenance (arguments :: terminal)
  .ok (⟨.agentToolCall, row.id⟩ :: payload)

def effectClosure (scope : AuthorizationScope) (segments : List Segment)
    (deniedSegments : List DocId) (dependencyDenials : List DependencyDenial)
    (provenance : List ProvenanceAccess) (effects : List EffectRow)
    (retained : List DocumentKey) : Except Error (List DocumentKey) := do
  let children := selectedChildren (observedEffects scope provenance effects) retained
  if !childrenCoherent children then
    .error (.provenanceConflict ⟨.agentToolCall, childConflict children⟩)
  else
    let documents ← children.mapM
      (effectDocuments scope segments deniedSegments dependencyDenials provenance)
    .ok documents.flatten

theorem undecodable_row_is_not_a_child (effects : List EffectRow)
    (retained : List DocumentKey) (row : EffectRow) (h : decodeEffect row = none) :
    row ∉ selectedChildren effects retained := by
  simp [selectedChildren, h]

theorem unobserved_row_is_not_a_child (scope : AuthorizationScope)
    (provenance : List ProvenanceAccess) (effects : List EffectRow)
    (retained : List DocumentKey) (row : EffectRow)
    (h : ∀ key, validateProvenance scope provenance ⟨.agentToolCall, row.id⟩ ≠ .ok key) :
    row ∉ selectedChildren (observedEffects scope provenance effects) retained := by
  intro hmem
  simp only [selectedChildren, List.mem_filter, List.mem_dedup, observedEffects] at hmem
  obtain ⟨⟨_, hobserved⟩, _⟩ := hmem
  split at hobserved
  · rename_i key hok
    exact h key hok
  · contradiction

theorem child_requires_retained_direct_parent (effects : List EffectRow)
    (retained : List DocumentKey) (row : EffectRow)
    (h : row ∈ selectedChildren effects retained) :
    ∃ parent ordinal, decodeEffect row = some (parent, ordinal) ∧
      ⟨.agentToolCall, parent⟩ ∈ retained ∧ isEffect effects parent = false := by
  simp only [selectedChildren, List.mem_filter] at h
  obtain ⟨_, hsel⟩ := h
  cases hdecode : decodeEffect row with
  | none => simp [hdecode] at hsel
  | some decoded =>
      obtain ⟨parent, ordinal⟩ := decoded
      simp only [hdecode, Bool.and_eq_true, List.contains_iff_mem, Bool.not_eq_true'] at hsel
      exact ⟨parent, ordinal, rfl, hsel.1, hsel.2⟩

theorem decoded_ordinal_bounded (row : EffectRow) (parent ordinal : Nat)
    (h : decodeEffect row = some (parent, ordinal)) :
    0 < ordinal ∧ ordinal ≤ ToolPolicy.PluginCalls.maxEffects := by
  unfold decodeEffect at h
  split at h
  · split at h
    · rename_i hguard
      simp only [Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨_, rfl⟩ := h
      simp only [Bool.and_eq_true, decide_eq_true_eq] at hguard
      exact ⟨hguard.1.1.1, hguard.1.1.2⟩
    · contradiction
  · contradiction

theorem children_per_parent_bounded (children : List EffectRow) (parent : DocId)
    (h : childrenCoherent children = true) :
    (children.filter fun row => (decodeEffect row).map Prod.fst == some parent).length ≤
      ToolPolicy.PluginCalls.maxEffects := by
  simp only [childrenCoherent, Bool.and_eq_true, decide_eq_true_eq] at h
  let siblings := children.filter fun row => (decodeEffect row).map Prod.fst == some parent
  have hnodup : (siblings.map decodeEffect).Nodup :=
    h.1.sublist ((List.filter_sublist children).map _)
  have hsub : (siblings.map decodeEffect).toFinset ⊆
      (Finset.Icc 1 ToolPolicy.PluginCalls.maxEffects).image
        (fun ordinal => some (parent, ordinal)) := by
    intro value hvalue
    simp only [List.mem_toFinset, List.mem_map] at hvalue
    obtain ⟨row, hrow, rfl⟩ := hvalue
    simp only [siblings, List.mem_filter] at hrow
    cases hdecode : decodeEffect row with
    | none => simp [hdecode] at hrow
    | some decoded =>
        obtain ⟨rowParent, ordinal⟩ := decoded
        simp only [hdecode, Option.map_some', beq_iff_eq, Option.some.injEq] at hrow
        have hbound := decoded_ordinal_bounded row rowParent ordinal hdecode
        simp only [Finset.mem_image, Finset.mem_Icc]
        exact ⟨ordinal, ⟨hbound.1, hbound.2⟩, by rw [hrow.2]⟩
  calc siblings.length = (siblings.map decodeEffect).length := (List.length_map siblings decodeEffect).symm
    _ = (siblings.map decodeEffect).toFinset.card := (List.toFinset_card_of_nodup hnodup).symm
    _ ≤ ((Finset.Icc 1 ToolPolicy.PluginCalls.maxEffects).image
          (fun ordinal => some (parent, ordinal))).card := Finset.card_le_card hsub
    _ ≤ (Finset.Icc 1 ToolPolicy.PluginCalls.maxEffects).card := Finset.card_image_le
    _ = ToolPolicy.PluginCalls.maxEffects := by simp

def validateRootSession (targetSession : SessionId) (messages : List MessageEnvelope)
    (deniedHeaders : List DocId) (id : DocId) : Except Error DocId := do
  let message ← lookupMessage messages deniedHeaders id
  if message.header.session = targetSession then pure id else .error .rootSessionMismatch

def buildManifest (scope : AuthorizationScope) (targetSession : SessionId)
    (bases : List AuthorizedBase) (authorizedRootMessages : List DocId)
    (requirements : List TerminalRequirement) (messages : List MessageEnvelope)
    (segments : List Segment) (provenance : List ProvenanceAccess)
    (deniedHeaders deniedSegments : List DocId)
    (dependencyDenials : List DependencyDenial := []) (effects : List EffectRow := []) :
    Except Error (List DocumentKey) := do
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
  let retained := baseKeys ++ terminalOwners ++ closure
  let children ← effectClosure scope segments deniedSegments dependencyDenials provenance
    effects retained
  .ok (canonicalManifest (retained ++ children))

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

theorem causal_provenance_is_not_a_base (key : AuthorizedBase) :
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
    (buildManifest originScope 1 [] [] [⟨10, 1, some (.message 999), .completed⟩]
      [originMessage] [closing] originAccess [] [])
    (.error (.terminal .missingHeader)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some (.message 200), .completed⟩]
      [originMessage] [closing] originAccess [] [])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentMessage, 200⟩,
      ⟨.agentOutputSegment, 100⟩]) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some .noMessage, .completed⟩] [] [] originAccess [] [])
    (.ok [⟨.agentRequest, 10⟩]) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, some .noMessage, .completed⟩] [] [] [] [] [])
    (.error (.provenanceMissing ⟨.agentRequest, 10⟩)) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, none, .superseded⟩] [] [] originAccess [] [])
    (.ok [⟨.agentRequest, 10⟩]) = true := by native_decide

example : resultMatches
    (buildManifest originScope 1 [] [] [⟨10, 1, none, .completed⟩] [] [] originAccess [] [])
    (.error (.terminal .missingSelection)) = true := by native_decide

def cycleLeft : MessageEnvelope :=
  { childMessage with header :=
      { childMessage.header with id := 210, origin := some 211, publication := .fork 211 } }
def cycleRight : MessageEnvelope :=
  { childMessage with
    header := { childMessage.header with
      id := 211, session := 1, origin := some 210, publication := .fork 210 }
    key := "cycle-right" }

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

def effectSegment (id : DocId) (source : Source) (writer : Writer) (kind : PayloadKind)
    (tool : Option ToolIdentity) (payload : List UInt8) : Segment :=
  { id, coordinate := ⟨10, source⟩, writer
  , flush := some
      { ordinal := 0
      , runs := [{ stream := 0, bytes := payload.length
                 , declaration := some { block := 0, part := 0, kind, tool } }]
      , payload }
  , close := some (.closed .complete 1 [payload.length]), createdAt := 1 }

def parentOutput : Segment := effectSegment 301 (.tool 300) (.tool 300) .toolOutput none [111, 107]

def parentDelivery : MessageEnvelope :=
  { header :=
      { id := 220, session := 2, request := some 10, origin := none
      , refs := [⟨301, 0⟩], outcome := .complete, role := .user
      , publication := .toolDelivery 300 }
  , key := "delivery", sequence := 1, nativeId := none
  , blocks := [.toolResult 300 "parent" none [.text ⟨⟨301, 0⟩, .full⟩]], createdAt := 3 }

def childArguments : Segment :=
  effectSegment 410 (.toolEffectArguments 400) (.request 7) .arguments
    (some ⟨effectIdentity 300 1, none, "echo"⟩) [123, 125]

def childOutput : Segment := effectSegment 411 (.tool 400) (.tool 400) .toolOutput none [100]

def child : EffectRow :=
  { id := 400, parent := some 300, ordinal := some 1, spawned := false
  , toolCallId := effectIdentity 300 1, terminal := some ⟨411, 0⟩ }

def forgedChild : EffectRow :=
  { id := 401, parent := some 300, ordinal := some 2, spawned := false, toolCallId := "forged" }

def unreachableChild : EffectRow :=
  { id := 402, parent := some 399, ordinal := some 1, spawned := false
  , toolCallId := effectIdentity 399 1 }

def effectAccess : List ProvenanceAccess :=
  access ++ [220, 300, 301, 400, 401, 402, 410, 411].flatMap fun id =>
    [ ⟨scope, ⟨.agentMessage, id⟩, .authorized⟩, ⟨scope, ⟨.agentToolCall, id⟩, .authorized⟩
    , ⟨scope, ⟨.agentOutputSegment, id⟩, .authorized⟩ ]

def effectSegments : List Segment := [parentOutput, childArguments, childOutput]

example : resultMatches
    (buildManifest scope 2 [] [220] [] [parentDelivery] effectSegments effectAccess [] [] []
      [child, forgedChild, unreachableChild])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentMessage, 220⟩, ⟨.agentToolCall, 300⟩,
      ⟨.agentToolCall, 400⟩, ⟨.agentOutputSegment, 301⟩, ⟨.agentOutputSegment, 410⟩,
      ⟨.agentOutputSegment, 411⟩]) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [201] [] [originMessage, childMessage, parentDelivery]
      (closing :: effectSegments) effectAccess [] [] [] [child])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentMessage, 200⟩, ⟨.agentMessage, 201⟩,
      ⟨.agentOutputSegment, 100⟩]) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [220] [] [parentDelivery] effectSegments
      (effectAccess ++ [⟨scope, ⟨.agentToolCall, 403⟩, .authorized⟩]) [] [] []
      [child, { child with id := 403, terminal := none }])
    (.error (.provenanceConflict ⟨.agentToolCall, 400⟩)) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [220] [] [parentDelivery] [parentOutput, childOutput]
      effectAccess [] [] [] [child])
    (.error (.reference (.lookup .unavailable))) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [] [220] [] [parentDelivery] effectSegments effectAccess [] [] []
      [{ child with terminal := some ⟨301, 0⟩ }])
    (.error (.reference (.lookup .invalidReference))) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [.toolCall 300] [] [] [] [childArguments, childOutput] effectAccess
      [] [] [] [child, forgedChild])
    (.ok [⟨.agentRequest, 10⟩, ⟨.agentToolCall, 300⟩, ⟨.agentToolCall, 400⟩,
      ⟨.agentOutputSegment, 410⟩, ⟨.agentOutputSegment, 411⟩]) = true := by native_decide

example : resultMatches
    (buildManifest scope 2 [.toolCall 300] [] [] [] [childArguments, childOutput]
      (effectAccess.map fun observation =>
        if observation.key = ⟨.agentToolCall, 400⟩ then { observation with state := .denied }
        else observation) [] [] [] [child])
    (.ok [⟨.agentToolCall, 300⟩]) = true := by native_decide

end Examples
end CanonicalOutput.Hydration
