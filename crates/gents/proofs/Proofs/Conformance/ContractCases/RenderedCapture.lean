import Proofs.Conformance.Family
import Proofs.Conformance.ContractCases.Types
import Proofs.Shared.RenderedCapture

/-!
# RenderedCapture contract cases

Witness rows for persist-before-send at the provider boundary (#840).

**Every expected value in this file is computed by running the Lean model.**
`captureOutcome`, `durableAfter`, `sendPermitted`, and
`providerRequestsObserved` are literally `RenderedCapture.Scenario.outcome`,
`.durableAfter`, `.sendPermitted`, and `.providerRequests`. Nothing here is
transcribed by hand, so a change to `RenderedCapture.capture` changes the rows
and breaks the Rust fence rather than quietly disagreeing with it.
-/

namespace Conformance.ContractCases

open RenderedCapture

structure RenderedCaptureCase where
  name : String
  nodeDid : Nat
  sessionId : Nat
  requestId : Nat
  turnIndex : Nat
  attempt : Nat
  /-- Opaque canonical-request identity. Equal values mean equal canonical JSON. -/
  request : Nat
  priorBinding : Option Nat
  captureOutcome : String
  captureDurable : Bool
  postStage : String
  sendPermitted : Bool
  providerRequestsObserved : Nat
  durableAfter : Option Nat
  finalStage : String
  deriving Repr

structure RenderedCaptureKeyCase where
  name : String
  leftNodeDid : Nat
  leftSessionId : Nat
  leftRequestId : Nat
  leftTurnIndex : Nat
  leftAttempt : Nat
  rightNodeDid : Nat
  rightSessionId : Nat
  rightRequestId : Nat
  rightTurnIndex : Nat
  rightAttempt : Nat
  sameFact : Bool
  deriving Repr

structure RenderedCaptureManifestBlock where
  artifact : ArtifactRef
  pinnedWitness : FieldCommitWitness
  storedWitness : Option FieldCommitWitness
  deriving Lean.ToJson

inductive RenderedCaptureStoredKind where
  | legacyFull
  | full
  | delta
  | manifest
  deriving Lean.ToJson

structure RenderedCaptureBlockRef where
  artifact : ArtifactRef
  witness : FieldCommitWitness
  deriving Lean.ToJson

structure RenderedCaptureStoreEntry where
  artifact : ArtifactRef
  commit : FieldCommitWitness
  kind : RenderedCaptureStoredKind
  bytes : List Nat
  baseRef : ArtifactRef
  baseWitness : FieldCommitWitness
  prefixLen : Nat
  suffixLen : Nat
  blocks : List RenderedCaptureBlockRef
  deriving Lean.ToJson

structure RenderedCaptureStorageInput where
  name : String
  encoding : String
  request : Nat
  baseWitness : Option Nat
  baseDepth : Nat
  maxDepth : Nat
  baseVerified : Bool
  /-- The block references the row's manifest document (ref 1 / witness 10)
  names, read out of the row's store; empty for the other encodings. -/
  manifestBlocks : List RenderedCaptureManifestBlock
  manifestRoot : Option RenderedCaptureManifestBlock
  store : List RenderedCaptureStoreEntry
  deriving Lean.ToJson

structure RenderedCaptureStorageOutput where
  decodedRequest : Option Nat
  sendPermitted : Bool
  deriving Lean.ToJson

private def storedEntry (artifact commit : Nat) (kind : RenderedCaptureStoredKind)
    (bytes : List Nat) : RenderedCaptureStoreEntry :=
  { artifact, commit, kind, bytes, baseRef := 0, baseWitness := 0, prefixLen := 0
  , suffixLen := 0, blocks := [] }

private def legacyEntry (artifact commit : Nat) (bytes : List Nat) :
    RenderedCaptureStoreEntry :=
  storedEntry artifact commit .legacyFull bytes

private def fullEntry (artifact commit : Nat) (bytes : List Nat) :
    RenderedCaptureStoreEntry :=
  storedEntry artifact commit .full bytes

private def deltaEntry (artifact commit baseRef baseWitness prefixLen suffixLen : Nat)
    (middle : List Nat) : RenderedCaptureStoreEntry :=
  { storedEntry artifact commit .delta middle with
    baseRef, baseWitness, prefixLen, suffixLen }

private def manifestEntry (artifact commit : Nat) (blocks : List (Nat × Nat)) :
    RenderedCaptureStoreEntry :=
  { storedEntry artifact commit .manifest [] with
    blocks := blocks.map fun (artifact, witness) => { artifact, witness } }

private def storedVersion (entry : RenderedCaptureStoreEntry) : StoredVersion :=
  { commit := entry.commit
  , encoded :=
      match entry.kind with
      | .legacyFull => .legacyFull entry.bytes
      | .full => .full entry.bytes
      | .delta =>
          .delta entry.baseRef entry.baseWitness entry.prefixLen entry.suffixLen entry.bytes
      | .manifest => .manifest (entry.blocks.map fun block => (block.artifact, block.witness))
  }

private def storeOf (entries : List RenderedCaptureStoreEntry) : ArtifactStore :=
  fun artifact =>
    (entries.find? fun entry => entry.artifact == artifact).map storedVersion

private def manifestBlocks (store : ArtifactStore) : List RenderedCaptureManifestBlock :=
  match store 1 with
  | some { encoded := .manifest blocks, .. } =>
      blocks.map fun (artifact, pinnedWitness) =>
        { artifact, pinnedWitness, storedWitness := (store artifact).map (·.commit) }
  | _ => []

private def storageInput (name encoding : String) (entries : List RenderedCaptureStoreEntry)
    (request : Nat) (baseWitness : Option Nat) (baseDepth maxDepth : Nat)
    (baseVerified : Bool) : RenderedCaptureStorageInput :=
  let store := storeOf entries
  let root := if encoding == "manifest_tree" then
    match manifestBlocks store with
    | [root] => some root
    | _ => none
    else none
  let children := match root with
    | some block => manifestBlocks (fun ref => if ref = 1 then store block.artifact else store ref)
    | none => manifestBlocks store
  { name, encoding, request, baseWitness, baseDepth, maxDepth, baseVerified
  , manifestBlocks := children
  , manifestRoot := root
  , store := entries
  }

@[conformance_family]
def renderedCaptureStorageResolution :
    Conformance.Family RenderedCaptureStorageInput RenderedCaptureStorageOutput where
  inputs :=
    [ storageInput "legacy_full_remains_readable" "legacy_full"
        [legacyEntry 1 10 [100]]
        100 none 0 8 false
    , storageInput "versioned_full_is_lossless" "full"
        [fullEntry 1 10 [101]]
        101 none 0 8 false
    , storageInput "verified_delta_is_lossless" "delta"
        [deltaEntry 1 10 2 41 0 0 [102], fullEntry 2 41 [99]]
        102 (some 41) 0 8 true
    , storageInput "missing_or_changed_base_blocks_send" "delta"
        [deltaEntry 1 10 2 42 0 0 [103], fullEntry 2 999 [99]]
        103 (some 42) 0 8 false
    , storageInput "over_depth_delta_blocks_send" "delta"
        [deltaEntry 1 10 2 43 0 0 [104], fullEntry 2 43 [99]]
        104 (some 43) 0 0 true
    , storageInput "manifest_of_full_blocks_is_lossless" "manifest"
        [manifestEntry 1 10 [(2, 41), (3, 42)], fullEntry 2 41 [105], fullEntry 3 42 []]
        105 none 0 3 false
    , storageInput "manifest_missing_block_blocks_send" "manifest"
        [manifestEntry 1 10 [(2, 41), (3, 42)], fullEntry 2 41 [106]]
        106 none 0 3 false
    , storageInput "manifest_block_witness_mismatch_blocks_send" "manifest"
        [manifestEntry 1 10 [(2, 41), (3, 42)], fullEntry 2 41 [107], fullEntry 3 999 []]
        107 none 0 3 false
    , storageInput "empty_manifest_decode_fails_blocks_send" "manifest"
        [manifestEntry 1 10 []]
        108 none 0 3 false
    , storageInput "manifest_over_fuel_blocks_send" "manifest"
        [manifestEntry 1 10 [(2, 43), (3, 44)], fullEntry 2 43 [109], fullEntry 3 44 []]
        109 none 0 2 false
    , storageInput "shared_manifest_tree_is_lossless" "manifest_tree"
        [ manifestEntry 1 10 [(4, 44)], manifestEntry 4 44 [(2, 41), (3, 42)]
        , fullEntry 2 41 [110], fullEntry 3 42 [] ]
        110 none 0 8 false
    , storageInput "shared_manifest_changed_node_blocks_send" "manifest_tree"
        [ manifestEntry 1 10 [(4, 44)], manifestEntry 4 999 [(2, 41), (3, 42)]
        , fullEntry 2 41 [111], fullEntry 3 42 [] ]
        111 none 0 8 false
    , storageInput "shared_manifest_missing_leaf_blocks_send" "manifest_tree"
        [ manifestEntry 1 10 [(4, 44)], manifestEntry 4 44 [(2, 41), (3, 42)]
        , fullEntry 2 41 [112] ]
        112 none 0 8 false
    ]
  model := fun i =>
    let decoded := resolveRequest (storeOf i.store) (i.maxDepth + 1) 1 10
    { decodedRequest := decoded.map CanonicalRequest.value
    , sendPermitted := decoded == some { value := i.request }
    }

theorem renderedCaptureStorageResolution_pinned :
    renderedCaptureStorageResolution.inputs.map
      (fun i => (i.name, (renderedCaptureStorageResolution.model i).decodedRequest,
        (renderedCaptureStorageResolution.model i).sendPermitted)) =
      [ ("legacy_full_remains_readable", some 100, true)
      , ("versioned_full_is_lossless", some 101, true)
      , ("verified_delta_is_lossless", some 102, true)
      , ("missing_or_changed_base_blocks_send", none, false)
      , ("over_depth_delta_blocks_send", none, false)
      , ("manifest_of_full_blocks_is_lossless", some 105, true)
      , ("manifest_missing_block_blocks_send", none, false)
      , ("manifest_block_witness_mismatch_blocks_send", none, false)
      , ("empty_manifest_decode_fails_blocks_send", none, false)
      , ("manifest_over_fuel_blocks_send", none, false)
      , ("shared_manifest_tree_is_lossless", some 110, true)
      , ("shared_manifest_changed_node_blocks_send", none, false)
      , ("shared_manifest_missing_leaf_blocks_send", none, false)
      ] := by
  rfl

theorem renderedCaptureStorageResolution_no_fail_open :
    renderedCaptureStorageResolution.inputs.all
      (fun i =>
        !(renderedCaptureStorageResolution.model i).sendPermitted
          || (renderedCaptureStorageResolution.model i).decodedRequest == some i.request)
      = true := by
  rfl

private def contractNodeDid : Nat := 7
private def contractSessionId : Nat := 11
private def contractRequestId : Nat := 23

private def contractKey (turnIndex attempt : Nat) : CaptureKey :=
  { nodeDid := contractNodeDid
  , sessionId := contractSessionId
  , requestId := contractRequestId
  , turnIndex := turnIndex
  , attempt := attempt
  }

private def renderedCaptureCase
    (name : String) (turnIndex attempt request : Nat)
    (priorBinding : Option Nat) : RenderedCaptureCase :=
  let scenario : Scenario :=
    { key := contractKey turnIndex attempt
    , request := { value := request }
    , priorBinding := priorBinding.map (fun value => { value := value })
    }
  { name := name
  , nodeDid := contractNodeDid
  , sessionId := contractSessionId
  , requestId := contractRequestId
  , turnIndex := turnIndex
  , attempt := attempt
  , request := request
  , priorBinding := priorBinding
  , captureOutcome := (Scenario.outcome scenario).toContract
  , captureDurable := (Scenario.outcome scenario).durable
  , postStage := (Scenario.postStage scenario).toContract
  , sendPermitted := Scenario.sendPermitted scenario
  , providerRequestsObserved := Scenario.providerRequests scenario
  , durableAfter := (Scenario.durableAfter scenario).map CanonicalRequest.value
  , finalStage := (Scenario.finalStage scenario).toContract
  }

def renderedCaptureCases : List RenderedCaptureCase :=
  [ renderedCaptureCase "fresh_capture_then_send" 0 0 100 none
  , renderedCaptureCase "idempotent_recapture_then_send" 0 0 100 (some 100)
  , renderedCaptureCase "rebound_key_is_an_integrity_violation" 0 0 100 (some 101)
  , renderedCaptureCase "transport_retry_same_request_new_attempt" 0 1 100 none
  , renderedCaptureCase "repair_retry_different_request_new_attempt" 0 1 102 none
  ]

theorem renderedCaptureCases_pinned :
    renderedCaptureCases.map
        (fun row =>
          (row.name, row.captureOutcome, row.captureDurable, row.postStage,
            row.sendPermitted, row.providerRequestsObserved, row.durableAfter,
            row.finalStage)) =
      [ ("fresh_capture_then_send", "fresh", true, "durablyCaptured", true, 1,
          some 100, "sent")
      , ("idempotent_recapture_then_send", "idempotent", true, "durablyCaptured",
          true, 1, some 100, "sent")
      , ("rebound_key_is_an_integrity_violation", "rejected", false, "assembled",
          false, 0, some 101, "assembled")
      , ("transport_retry_same_request_new_attempt", "fresh", true,
          "durablyCaptured", true, 1, some 100, "sent")
      , ("repair_retry_different_request_new_attempt", "fresh", true,
          "durablyCaptured", true, 1, some 102, "sent")
      ] := by
  rfl

theorem renderedCaptureCases_no_fail_open :
    renderedCaptureCases.all
      (fun row =>
        (!row.sendPermitted || (row.durableAfter == some row.request &&
            row.captureDurable && row.providerRequestsObserved == 1)) &&
        (row.sendPermitted || row.providerRequestsObserved == 0)) = true := by
  rfl

private def renderedCaptureKeyCase
    (name : String) (left right : CaptureKey) : RenderedCaptureKeyCase :=
  { name := name
  , leftNodeDid := left.nodeDid
  , leftSessionId := left.sessionId
  , leftRequestId := left.requestId
  , leftTurnIndex := left.turnIndex
  , leftAttempt := left.attempt
  , rightNodeDid := right.nodeDid
  , rightSessionId := right.sessionId
  , rightRequestId := right.requestId
  , rightTurnIndex := right.turnIndex
  , rightAttempt := right.attempt
  , sameFact := decide (left = right)
  }

/-- One pair per key component, plus the identical pair. -/
def renderedCaptureKeyCases : List RenderedCaptureKeyCase :=
  [ renderedCaptureKeyCase "identical_tuple_is_one_fact"
      (contractKey 0 0) (contractKey 0 0)
  , renderedCaptureKeyCase "attempt_separates_facts"
      (contractKey 0 0) (contractKey 0 1)
  , renderedCaptureKeyCase "turn_index_separates_facts"
      (contractKey 0 0) (contractKey 1 0)
  , renderedCaptureKeyCase "node_did_separates_facts"
      (contractKey 0 0) { contractKey 0 0 with nodeDid := contractNodeDid + 1 }
  , renderedCaptureKeyCase "session_id_separates_facts"
      (contractKey 0 0) { contractKey 0 0 with sessionId := contractSessionId + 1 }
  , renderedCaptureKeyCase "request_doc_id_separates_facts"
      (contractKey 0 0) { contractKey 0 0 with requestId := contractRequestId + 1 }
  ]

theorem renderedCaptureKeyCases_pinned :
    renderedCaptureKeyCases.map (fun row => (row.name, row.sameFact)) =
      [ ("identical_tuple_is_one_fact", true)
      , ("attempt_separates_facts", false)
      , ("turn_index_separates_facts", false)
      , ("node_did_separates_facts", false)
      , ("session_id_separates_facts", false)
      , ("request_doc_id_separates_facts", false)
      ] := by
  rfl

/-! ## Capture scope labels and the consumer ordering (#1066)

The producer writes the loop discriminator as `"{kind}.{seq}"`. These cases fix
the label vocabulary a consumer's parser must accept, the reject vectors it
must error on (never default), and the identity ordering
`(kind rank, seq, turn, attempt)`.
-/

inductive CaptureScopeKind
  | inference
  | compaction
  | compactionFallback
  | title
  | oneshot
  | accessReview
  deriving Repr, DecidableEq

def CaptureScopeKind.label : CaptureScopeKind → String
  | .inference => "inference"
  | .compaction => "compaction"
  | .compactionFallback => "compaction_fallback"
  | .title => "title"
  | .oneshot => "oneshot"
  | .accessReview => "access_review"

/-- Declaration-order rank across kinds; not a temporal order. -/
def CaptureScopeKind.rank : CaptureScopeKind → Nat
  | .inference => 0
  | .compaction => 1
  | .compactionFallback => 2
  | .title => 3
  | .oneshot => 4
  | .accessReview => 5

def allCaptureScopeKinds : List CaptureScopeKind :=
  [.inference, .compaction, .compactionFallback, .title, .oneshot, .accessReview]

structure CaptureScopeCase where
  label : String
  kind : String
  seq : Nat
  valid : Bool
  deriving Repr

private def validScopeCase (kind : CaptureScopeKind) (seq : Nat) : CaptureScopeCase :=
  { label := kind.label ++ "." ++ toString seq
  , kind := kind.label
  , seq := seq
  , valid := true
  }

private def invalidScopeCase (label : String) : CaptureScopeCase :=
  { label := label, kind := "", seq := 0, valid := false }

def captureScopeCases : List CaptureScopeCase :=
  (allCaptureScopeKinds.map (validScopeCase · 1))
    ++ [ validScopeCase .inference 2
       , validScopeCase .inference 10
       , validScopeCase .compaction 2
       ]
    ++ [ invalidScopeCase ""
       , invalidScopeCase "inference"
       , invalidScopeCase "inference."
       , invalidScopeCase ".1"
       , invalidScopeCase "mystery.1"
       , invalidScopeCase "inference.0x2"
       , invalidScopeCase "inference.1.2"
       , invalidScopeCase "inference. 2"
       ]

theorem captureScopeCases_valid_labels_distinct :
    ((captureScopeCases.filter (·.valid)).map (·.label)).Nodup := by
  decide

structure CaptureOrderCase where
  name : String
  leftLabel : String
  leftTurn : Nat
  leftAttempt : Nat
  rightLabel : String
  rightTurn : Nat
  rightAttempt : Nat
  leftBeforeRight : Bool
  deriving Repr

private def orderTuple
    (kind : CaptureScopeKind) (seq turn attempt : Nat) : Nat × Nat × Nat × Nat :=
  (kind.rank, seq, turn, attempt)

private def tupleLt : Nat × Nat × Nat × Nat → Nat × Nat × Nat × Nat → Bool
  | (a1, a2, a3, a4), (b1, b2, b3, b4) =>
    if a1 ≠ b1 then a1 < b1
    else if a2 ≠ b2 then a2 < b2
    else if a3 ≠ b3 then a3 < b3
    else a4 < b4

private def orderCase (name : String)
    (leftKind : CaptureScopeKind) (leftSeq leftTurn leftAttempt : Nat)
    (rightKind : CaptureScopeKind) (rightSeq rightTurn rightAttempt : Nat) :
    CaptureOrderCase :=
  { name := name
  , leftLabel := (validScopeCase leftKind leftSeq).label
  , leftTurn := leftTurn
  , leftAttempt := leftAttempt
  , rightLabel := (validScopeCase rightKind rightSeq).label
  , rightTurn := rightTurn
  , rightAttempt := rightAttempt
  , leftBeforeRight :=
      tupleLt (orderTuple leftKind leftSeq leftTurn leftAttempt)
        (orderTuple rightKind rightSeq rightTurn rightAttempt)
  }

def captureOrderCases : List CaptureOrderCase :=
  [ orderCase "numeric_seq_dominates_lexical" .inference 2 3 1 .inference 10 0 0
  , orderCase "attempt_breaks_turn_tie" .inference 1 0 0 .inference 1 0 1
  , orderCase "turn_dominates_attempt" .inference 1 0 5 .inference 1 1 0
  , orderCase "kind_rank_dominates_seq" .inference 9 9 9 .compaction 1 0 0
  , orderCase "fallback_ranks_after_compaction" .compaction 5 0 0 .compactionFallback 1 0 0
  , orderCase "oneshot_follows_title" .title 1 0 0 .oneshot 1 0 0
  , orderCase "access_review_preserves_existing_kind_order" .oneshot 9 9 9 .accessReview 1 0 0
  , orderCase "equal_keys_do_not_order" .inference 1 2 3 .inference 1 2 3
  ]

theorem captureOrderCases_pinned :
    captureOrderCases.map (fun row => (row.name, row.leftBeforeRight)) =
      [ ("numeric_seq_dominates_lexical", true)
      , ("attempt_breaks_turn_tie", true)
      , ("turn_dominates_attempt", true)
      , ("kind_rank_dominates_seq", true)
      , ("fallback_ranks_after_compaction", true)
      , ("oneshot_follows_title", true)
      , ("access_review_preserves_existing_kind_order", true)
      , ("equal_keys_do_not_order", false)
      ] := by
  rfl

end Conformance.ContractCases
