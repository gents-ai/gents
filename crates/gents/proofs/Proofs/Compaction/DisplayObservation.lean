import Proofs.Compaction.DurableReduction

namespace Compaction.DisplayObservation

open Compaction.DurableReduction

abbrev RequesterDid := Nat

structure Metadata where
  requester : RequesterDid
  summary : Nat
  canonicalThrough : Option Nat
  messagesCompacted : Nat
  originalTokens : Nat
  compactedTokens : Nat
  createdAt : Nat
  deriving DecidableEq, Repr

structure Display where
  key : ReductionKey
  metadata : Metadata
  deriving DecidableEq, Repr

/-- Replay projections and checkpoints are local audit facts. A display
observation contains only explicitly selected metadata, scoped by requester
and node, and is created only when its reduction was persisted. -/
def project (outcome : PersistOutcome) (key : ReductionKey) (metadata : Metadata) :
    Option Display :=
  if outcome.durable then some ⟨key, metadata⟩ else none

def visible (display : Display) (node : NodeDid) (requester : RequesterDid) : Bool :=
  decide (display.key.nodeDid = node ∧ display.metadata.requester = requester)

/-- An older runtime can omit the display collection. When it advertises the
collection, its schema must match before replication can use it. -/
def displaySchemaCompatible (localVersion : Option Nat) (remoteVersion : Option Nat) : Bool :=
  match remoteVersion with
  | none => true
  | some version => decide (localVersion = some version)

theorem advertised_display_requires_same_schema (localVersion : Option Nat) (version : Nat)
    (h : displaySchemaCompatible localVersion (some version) = true) :
    localVersion = some version := by
  simpa [displaySchemaCompatible] using h

structure SchemaCase where
  localVersion : Option Nat
  remoteVersion : Option Nat
  expected : Bool
  deriving Repr

def schemaCases : List SchemaCase :=
  [(some 1, none), (some 1, some 1), (some 1, some 2), (none, some 1)].map
    (fun (localVersion, remoteVersion) =>
      ⟨localVersion, remoteVersion, displaySchemaCompatible localVersion remoteVersion⟩)

/-- Retained-history recovery derives metadata from the stored reduction. -/
def recover (store : ReductionKey → Option (Fact × Metadata)) (key : ReductionKey) : Option Display :=
  match store key with
  | some (_, metadata) => project .idempotent key metadata
  | none => none

theorem non_durable_has_no_display (outcome : PersistOutcome) (key : ReductionKey)
    (metadata : Metadata) (h : outcome.durable = false) :
    project outcome key metadata = none := by
  simp [project, h]

theorem visible_requires_scope (display : Display) (node : NodeDid)
    (requester : RequesterDid) (h : visible display node requester = true) :
    display.key.nodeDid = node ∧ display.metadata.requester = requester := by
  simpa [visible] using h

theorem other_node_cannot_observe (display : Display) (node : NodeDid)
    (requester : RequesterDid) (h : display.key.nodeDid ≠ node) :
    visible display node requester = false := by
  simp [visible, h]

theorem other_requester_cannot_observe (display : Display) (node : NodeDid)
    (requester : RequesterDid) (h : display.metadata.requester ≠ requester) :
    visible display node requester = false := by
  simp [visible, h]

theorem durable_observation_is_visible_to_its_scope (outcome : PersistOutcome)
    (key : ReductionKey) (metadata : Metadata) (h : outcome.durable = true) :
    ∃ display, project outcome key metadata = some display ∧
      visible display key.nodeDid metadata.requester = true := by
  refine ⟨⟨key, metadata⟩, ?_, ?_⟩
  · simp [project, h]
  · simp [visible]

theorem recovery_requires_persisted_fact (store : ReductionKey → Option (Fact × Metadata))
    (key : ReductionKey) (h : store key = none) :
    recover store key = none := by
  simp [recover, h]

theorem recovery_matches_new_observation (store : ReductionKey → Option (Fact × Metadata))
    (key : ReductionKey) (fact : Fact) (metadata : Metadata)
    (h : store key = some (fact, metadata)) :
    recover store key = project .fresh key metadata := by
  simp [recover, project, PersistOutcome.durable, h]

theorem recovered_display_keeps_original_scope
    (store : ReductionKey → Option (Fact × Metadata)) (key : ReductionKey)
    (fact : Fact) (metadata : Metadata) (node : NodeDid) (requester : RequesterDid)
    (h : store key = some (fact, metadata))
    (hvisible : (recover store key).map (fun display => visible display node requester) = some true) :
    key.nodeDid = node ∧ metadata.requester = requester := by
  simp [recover, project, PersistOutcome.durable, h, visible] at hvisible
  exact hvisible

structure ObservationCase where
  name : String
  outcome : PersistOutcome
  ownerNode : NodeDid
  owner : RequesterDid
  readerNode : NodeDid
  reader : RequesterDid
  expectedVisible : Bool
  deriving Repr

private def exampleKey : ReductionKey :=
  { nodeDid := 7, sessionId := 11, requestDocId := 23, turnIndex := 1, ordinal := 1 }

private def exampleMetadata : Metadata :=
  { requester := 13, summary := 17, canonicalThrough := some 19,
    messagesCompacted := 3, originalTokens := 100, compactedTokens := 20, createdAt := 29 }

private def observationCase (name : String) (outcome : PersistOutcome)
    (node : NodeDid) (requester : RequesterDid) : ObservationCase :=
  { name, outcome, ownerNode := exampleKey.nodeDid, owner := exampleMetadata.requester,
    readerNode := node, reader := requester,
    expectedVisible := (project outcome exampleKey exampleMetadata).any
      (fun display => visible display node requester) }

def observationCases : List ObservationCase :=
  [ observationCase "fresh_matching_scope" .fresh 7 13
  , observationCase "retry_matching_scope" .idempotent 7 13
  , observationCase "conflict_is_not_published" .conflict 7 13
  , observationCase "open_pair_is_not_published" .pairOpen 7 13
  , observationCase "different_requester" .fresh 7 14
  , observationCase "different_node" .fresh 8 13
  ]

theorem observationCases_match_scope_and_durability :
    observationCases.map (fun case => (case.name, case.expectedVisible)) =
      [("fresh_matching_scope", true), ("retry_matching_scope", true),
       ("conflict_is_not_published", false), ("open_pair_is_not_published", false),
       ("different_requester", false), ("different_node", false)] := by
  rfl

end Compaction.DisplayObservation
