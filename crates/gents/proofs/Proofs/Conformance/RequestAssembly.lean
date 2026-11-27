import Proofs.Session.PromptAssembly.ContextCalibration
import Proofs.Session.PromptAssembly.ReasoningIdentity
import Proofs.Session.PromptAssembly.SourceTagging
import Proofs.Conformance.Family

namespace Conformance.RequestAssembly

open Lean PromptAssembly.ContextCalibration PromptAssembly.ReasoningIdentity
  PromptAssembly.SourceTagging

structure CapacityInput where
  ratio : Nat
  raw : Nat
  contextWindow : Nat
  maxOutputTokens : Nat
  deriving ToJson

structure Dispatched where
  maxOutputTokens : Nat
  contextTokens : Nat
  deriving ToJson

@[conformance_family]
def dispatchCapacity : Family CapacityInput (Option Dispatched) where
  inputs :=
    [10000, 40000].flatMap fun ratio =>
      [1000, 2048].flatMap fun maxOutputTokens =>
        [500, 1200, 2500, 3999, 4000, 5000].map fun raw =>
          { ratio, raw, contextWindow := 4000, maxOutputTokens }
  model := fun i =>
    let s := seed i.ratio []
    (s.dispatchCap i.raw i.contextWindow i.maxOutputTokens).map fun cap =>
      { maxOutputTokens := cap, contextTokens := s.tokens i.raw }

deriving instance ToJson for Rebuild

structure RebuildInput where
  ratio : Nat
  beforeRaw : Nat
  afterRaw : Nat
  contextWindow : Nat
  thresholdBasisPoints : Nat
  maxOutputTokens : Nat
  deriving ToJson

structure RebuildObservation where
  rebuild : Rebuild
  compactorCalls : Nat
  commits : Nat
  replacesSession : Bool
  keepsAnchor : Bool
  deriving ToJson

/-- Every case runs on a session anchored at a call counted as one token, so
the anchor never lowers the count and only its survival is observed. -/
def anchoredSession (ratio : Nat) : State :=
  { seed ratio [] with anchor := some ⟨1, 0, 1⟩ }

@[conformance_family]
def turnRebuild : Family RebuildInput RebuildObservation where
  inputs :=
    [(20000, 30000, 30000), (20000, 40000, 30000), (20000, 40000, 40000),
     (20000, 40000, 100000), (10000, 80000, 40000), (10000, 80000, 76000)].map
      fun (ratio, beforeRaw, afterRaw) =>
        { ratio, beforeRaw, afterRaw, contextWindow := 100000, thresholdBasisPoints := 7500,
          maxOutputTokens := 8000 }
  model := fun i =>
    let r := (anchoredSession i.ratio).rebuild i.beforeRaw i.afterRaw i.contextWindow
      i.thresholdBasisPoints i.maxOutputTokens
    { rebuild := r, compactorCalls := r.compactorCalls, commits := r.commits,
      replacesSession := r.replacesSession, keepsAnchor := r.keepsAnchor }

deriving instance ToJson for EvidenceOrigin, Block

structure IdentityInput where
  origin : EvidenceOrigin
  complete : Bool
  hasIdentity : Bool
  content : List Block
  deriving ToJson

structure IdentityObservation where
  reasoningReplayed : Bool
  retained : List Block
  deriving ToJson

@[conformance_family]
def reasoningIdentity : Family IdentityInput IdentityObservation where
  inputs :=
    [EvidenceOrigin.acceptedProvider, .recoveredProvider].flatMap fun origin =>
      [false, true].flatMap fun complete =>
        [false, true].map fun hasIdentity =>
          { origin, complete, hasIdentity, content := [.reasoning, .text] }
  model := fun i =>
    let replayed := replaysReasoning i.origin i.complete i.hasIdentity
    { reasoningReplayed := replayed, retained := retained replayed i.content }

deriving instance ToJson for RowRole, SourceTagError

structure SourceTagWire where
  request : Nat
  scope : Nat
  turn : Nat
  attempt : Nat
  deriving ToJson

def SourceTagWire.toModel (tag : SourceTagWire) : PromptAssembly.ClaudeMap.ReplayTag :=
  { request := tag.request, source := .provider tag.scope tag.turn tag.attempt }

def SourceTagWire.ofModel (tag : PromptAssembly.ClaudeMap.ReplayTag) : SourceTagWire :=
  match tag.source with
  | .provider scope turn attempt => { request := tag.request, scope, turn, attempt }
  | _ => { request := tag.request, scope := 0, turn := 0, attempt := 0 }

structure LoadedRowWire where
  role : RowRole
  source : Option SourceTagWire := none
  physicalHeader : Option String := none
  blockIndices : List Nat := []
  blockCount : Nat := 0
  stubReceiptKeys : List String := []
  deriving ToJson

def LoadedRowWire.toModel (row : LoadedRowWire) : LoadedRow :=
  { role := row.role
    source := row.source.map SourceTagWire.toModel
    physicalHeader := row.physicalHeader
    blockIndices := row.blockIndices
    blockCount := row.blockCount
    stubReceiptKeys := row.stubReceiptKeys }

structure SourcedRowWire where
  sourceIndex : Nat
  role : RowRole
  blockCount : Nat := 0
  blockIndices : List Nat := []
  keptToolResultKeys : List String := []
  deriving ToJson

def SourcedRowWire.toModel (row : SourcedRowWire) : SourcedRow :=
  { sourceIndex := row.sourceIndex
    role := row.role
    blockCount := row.blockCount
    blockIndices := row.blockIndices
    keptToolResultKeys := row.keptToolResultKeys }

structure TaggedRowWire where
  role : RowRole
  source : Option SourceTagWire
  physicalHeader : Option String
  blockIndices : List Nat
  stubReceiptKeys : List String
  deriving ToJson

def TaggedRowWire.ofModel (row : TaggedRow) : TaggedRowWire :=
  { role := row.role
    source := row.source.map SourceTagWire.ofModel
    physicalHeader := row.physicalHeader
    blockIndices := row.blockIndices
    stubReceiptKeys := row.stubReceiptKeys }

structure SourceTaggingInput where
  name : String
  originals : List LoadedRowWire
  sourced : List SourcedRowWire
  deriving ToJson

structure SourceTaggingOutput where
  error : Option SourceTagError
  rows : List TaggedRowWire
  deriving ToJson

private def providerTagWire (turn : Nat) : SourceTagWire :=
  { request := 7, scope := 1, turn, attempt := 0 }

@[conformance_family]
def sourceTagging : Family SourceTaggingInput SourceTaggingOutput where
  inputs :=
    let tag1 := providerTagWire 1
    let tag2 := providerTagWire 2
    [ { name := "empty_sourced_over_valid_originals"
        originals :=
          [ { role := .user, physicalHeader := some "hdr-u0" }
          , { role := .assistant, source := some tag1, physicalHeader := some "hdr-a1",
              blockIndices := [0], blockCount := 1 } ]
        sourced := [] }
    , { name := "equal_assistant_occurrences_keep_distinct_tags_unhydrated"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 1 }
          , { role := .user }
          , { role := .assistant, source := some tag2, blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] }
          , { sourceIndex := 1, role := .user }
          , { sourceIndex := 2, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "hydrated_assistant_composes_noncontiguous_physical_block_indices"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [2, 4, 7], blockCount := 3 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 2, blockIndices := [0, 2] } ] }
    , { name := "hydrated_assistant_narrowed_to_single_tail_block"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [2, 4], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [1] } ] }
    , { name := "hydrated_assistant_narrowed_to_empty_blocks"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [1, 3], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 0, blockIndices := [] } ] }
    , { name := "unhydrated_associated_assistant_drops_first_block_keeps_tag"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [1] } ] }
    , { name := "unassociated_assistant_and_dropped_earlier_assistant"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-1",
              blockIndices := [0], blockCount := 1 }
          , { role := .assistant, blockCount := 1 }
          , { role := .assistant, source := some tag2, physicalHeader := some "hdr-2",
              blockIndices := [3, 5], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 1, role := .assistant, blockCount := 1, blockIndices := [0] }
          , { sourceIndex := 2, role := .assistant, blockCount := 1, blockIndices := [1] } ] }
    , { name := "non_assistant_filters_stub_receipts_and_keeps_header"
        originals :=
          [ { role := .user, physicalHeader := some "hdr-u",
              stubReceiptKeys := ["call-1", "call-2", "call-3"] }
          , { role := .system, physicalHeader := some "hdr-s" } ]
        sourced :=
          [ { sourceIndex := 0, role := .user, keptToolResultKeys := ["call-2", "other"] }
          , { sourceIndex := 1, role := .system } ] }
    , { name := "rejects_non_assistant_original_with_source_even_when_unreferenced"
        originals :=
          [ { role := .user, source := some tag1 } ]
        sourced := [] }
    , { name := "rejects_system_original_with_source"
        originals :=
          [ { role := .system, source := some tag1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .system } ] }
    , { name := "rejects_tagged_assistant_projected_as_user"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .user } ] }
    , { name := "rejects_unassociated_user_projected_as_assistant"
        originals :=
          [ { role := .user } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "rejects_unassociated_assistant_projected_as_user"
        originals :=
          [ { role := .assistant, physicalHeader := some "hdr-a",
              blockIndices := [0], blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .user } ] }
    , { name := "rejects_user_projected_as_system"
        originals :=
          [ { role := .user } ]
        sourced :=
          [ { sourceIndex := 0, role := .system } ] }
    , { name := "rejects_duplicated_source_index"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] }
          , { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "rejects_reordered_source_indices"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 1 }
          , { role := .assistant, source := some tag2, blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 1, role := .assistant, blockCount := 1, blockIndices := [0] }
          , { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "rejects_out_of_range_source_index"
        originals :=
          [ { role := .user } ]
        sourced :=
          [ { sourceIndex := 1, role := .user } ] }
    , { name := "rejects_misaligned_assistant_block_indices"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [0, 1], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 2, blockIndices := [0] } ] }
    , { name := "rejects_reordered_hydrated_assistant_blocks"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [2, 4], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 2, blockIndices := [1, 0] } ] }
    , { name := "rejects_duplicated_hydrated_assistant_block_index"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [2, 4], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 2, blockIndices := [0, 0] } ] }
    , { name := "rejects_reordered_unhydrated_assistant_blocks"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 2, blockIndices := [1, 0] } ] }
    , { name := "rejects_out_of_range_hydrated_assistant_block_index"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [2, 4], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [2] } ] }
    , { name := "rejects_out_of_range_unhydrated_assistant_block_index"
        originals :=
          [ { role := .assistant, source := some tag1, blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [1] } ] }
    , { name := "rejects_invalid_original_block_index_length"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [0], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "rejects_non_increasing_original_block_indices"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [3, 3], blockCount := 2 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 1, blockIndices := [0] } ] }
    , { name := "rejects_original_with_header_but_empty_block_indices_when_nonempty"
        originals :=
          [ { role := .assistant, source := some tag1, physicalHeader := some "hdr-a",
              blockIndices := [], blockCount := 1 } ]
        sourced :=
          [ { sourceIndex := 0, role := .assistant, blockCount := 0, blockIndices := [] } ] } ]
  model := fun i =>
    match taggedFromSourced (i.originals.map LoadedRowWire.toModel)
        (i.sourced.map SourcedRowWire.toModel) with
    | .error err => { error := some err, rows := [] }
    | .ok rows => { error := none, rows := rows.map TaggedRowWire.ofModel }

end Conformance.RequestAssembly
