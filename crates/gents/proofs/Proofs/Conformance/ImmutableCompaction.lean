import Proofs.Session.Compaction.ImmutableEntry
import Proofs.Session.CanonicalOutput.MessageKeyRetry
import Proofs.Conformance.Family

namespace Conformance.ImmutableCompaction

open Lean
open Compaction.ImmutableEntry
open CanonicalOutput.MessageKeyRetry

structure CompactionEntryCaseWire where
  requestId : Nat
  summary : Nat
  messagesCompacted : Nat
  cursor : Nat
  originalTokens : Nat
  compactedTokens : Nat
  filesRead : List Nat
  filesModified : List Nat
  deriving ToJson

structure CompactionWriteInput where
  name : String
  storedSequences : List Nat
  priorEntries : List CompactionEntryCaseWire
  expectedGenerationLen : Nat
  candidate : CompactionEntryCaseWire
  replaceSourceMessage : Bool
  deriving ToJson

structure CompactionWriteOutput where
  outcome : String
  pinnedSequences : List Nat
  deriving ToJson

private def storedMessages (sequences : List Nat) : List PinnedSourceMessage :=
  sequences.map fun seq => ⟨seq * 10, seq, seq * 100⟩

private def buildChain (stored : List PinnedSourceMessage) :
    Option Nat → List CompactionEntryCaseWire → List CompactionFact
  | _, [] => []
  | priorCursor, entry :: rest =>
      let manifest := sliceForRange priorCursor entry.cursor stored
      let fact : CompactionFact :=
        { requestDocId := entry.requestId
          summary := entry.summary
          messagesCompacted := entry.messagesCompacted
          compactedThroughSequence := entry.cursor
          originalTokens := entry.originalTokens
          compactedTokens := entry.compactedTokens
          filesRead := entry.filesRead
          filesModified := entry.filesModified
          sourceMessages := manifest }
      fact :: buildChain stored (some entry.cursor) rest

private def evalCompactionWrite (i : CompactionWriteInput) : CompactionWriteOutput :=
  let initialStored := storedMessages i.storedSequences
  let chain := buildChain initialStored none i.priorEntries
  let effectiveStored :=
    if i.replaceSourceMessage then
      initialStored.map fun m =>
        if m.sequence == i.candidate.cursor then
          { m with docId := m.docId + 1, commitCid := m.commitCid + 1 }
        else m
    else
      initialStored
  let priorCursor :=
    if i.expectedGenerationLen == 0 then none
    else (chain[i.expectedGenerationLen - 1]?).map (·.compactedThroughSequence)
  let candidateManifest := sliceForRange priorCursor i.candidate.cursor effectiveStored
  let candidateFact : CompactionFact :=
    { requestDocId := i.candidate.requestId
      summary := i.candidate.summary
      messagesCompacted := i.candidate.messagesCompacted
      compactedThroughSequence := i.candidate.cursor
      originalTokens := i.candidate.originalTokens
      compactedTokens := i.candidate.compactedTokens
      filesRead := i.candidate.filesRead
      filesModified := i.candidate.filesModified
      sourceMessages := candidateManifest }
  let outcome := decideWrite chain i.expectedGenerationLen candidateFact effectiveStored
  let pinned :=
    match outcome with
    | .create | .idempotentReplay => candidateManifest.map (·.sequence)
    | _ => []
  { outcome := outcome.toContract, pinnedSequences := pinned }

private def baseEntry : CompactionEntryCaseWire :=
  { requestId := 1, summary := 10, messagesCompacted := 2, cursor := 2,
    originalTokens := 100, compactedTokens := 20, filesRead := [1], filesModified := [2] }

private def laterEntry : CompactionEntryCaseWire :=
  { requestId := 2, summary := 20, messagesCompacted := 1, cursor := 3,
    originalTokens := 80, compactedTokens := 15, filesRead := [3], filesModified := [] }

/-- Create-only `CompactionEntry` persistence: fresh creation pins exact source
`AgentMessage` document versions, exact redelivery is idempotent even after
later compaction generations land, a mismatched retry by the same request fails
closed as a typed integrity conflict (including when a pinned source message
version changed), a concurrent retry by a different request reports a stale
generation, and invalid/regressed cursors are refused. -/
@[conformance_family]
def writeDecision : Family CompactionWriteInput CompactionWriteOutput where
  inputs :=
    [ { name := "fresh_create_pins_source_versions"
        storedSequences := [1, 2, 3]
        priorEntries := []
        expectedGenerationLen := 0
        candidate := baseEntry
        replaceSourceMessage := false }
    , { name := "second_generation_pins_incremental_slice"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 1
        candidate := laterEntry
        replaceSourceMessage := false }
    , { name := "exact_redelivery_is_idempotent"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := baseEntry
        replaceSourceMessage := false }
    , { name := "exact_redelivery_after_later_generation"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry, laterEntry]
        expectedGenerationLen := 0
        candidate := baseEntry
        replaceSourceMessage := false }
    , { name := "mismatched_summary_same_request_conflict"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := { baseEntry with summary := 99 }
        replaceSourceMessage := false }
    , { name := "mismatched_cursor_same_request_conflict"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := { baseEntry with cursor := 3 }
        replaceSourceMessage := false }
    , { name := "mismatched_tokens_same_request_conflict"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := { baseEntry with compactedTokens := 99 }
        replaceSourceMessage := false }
    , { name := "mismatched_files_same_request_conflict"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := { baseEntry with filesRead := [1, 9] }
        replaceSourceMessage := false }
    , { name := "mismatched_source_version_same_request_conflict"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := baseEntry
        replaceSourceMessage := true }
    , { name := "different_request_race_stale_generation"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 0
        candidate := { baseEntry with requestId := 2, summary := 22 }
        replaceSourceMessage := false }
    , { name := "cursor_regression_refused"
        storedSequences := [1, 2, 3]
        priorEntries := [baseEntry]
        expectedGenerationLen := 1
        candidate := { laterEntry with cursor := 2 }
        replaceSourceMessage := false }
    , { name := "cursor_not_stored_refused"
        storedSequences := [1, 2]
        priorEntries := []
        expectedGenerationLen := 0
        candidate := { baseEntry with cursor := 5 }
        replaceSourceMessage := false } ]
  model := evalCompactionWrite

structure MessageFactWire where
  messageKey : String
  requestDocId : Nat
  executionGeneration : Nat
  sourceId : Nat
  payloadDigest : Nat
  deriving ToJson

def MessageFactWire.toModel (w : MessageFactWire) : MessageFact :=
  { messageKey := w.messageKey
    requestDocId := w.requestDocId
    executionGeneration := w.executionGeneration
    sourceId := w.sourceId
    payloadDigest := w.payloadDigest }

structure MessageKeyRetryInput where
  name : String
  existing : List MessageFactWire
  candidate : MessageFactWire
  deriving ToJson

structure MessageKeyRetryOutput where
  outcome : String
  deriving ToJson

private def baseMessage : MessageFactWire :=
  { messageKey := "key-1", requestDocId := 1, executionGeneration := 1,
    sourceId := 1, payloadDigest := 10 }

/-- Create-and-compare under one `AgentMessage.message_key`: fresh keys create
once, exact retries replay idempotently, and mismatched payload, mismatched
source, or duplicate twin rows fail closed as a typed integrity conflict. -/
@[conformance_family]
def messageKeyRetry : Family MessageKeyRetryInput MessageKeyRetryOutput where
  inputs :=
    [ { name := "fresh_message_create"
        existing := []
        candidate := baseMessage }
    , { name := "exact_same_key_retry_idempotent"
        existing := [baseMessage]
        candidate := baseMessage }
    , { name := "mismatched_payload_same_key_conflict"
        existing := [baseMessage]
        candidate := { baseMessage with payloadDigest := 20 } }
    , { name := "mismatched_source_same_key_conflict"
        existing := [baseMessage]
        candidate := { baseMessage with sourceId := 2 } }
    , { name := "duplicate_key_twins_conflict"
        existing := [baseMessage, baseMessage]
        candidate := baseMessage } ]
  model := fun i =>
    { outcome := (decideMessageWrite (i.existing.map MessageFactWire.toModel)
        i.candidate.toModel).toContract }

end Conformance.ImmutableCompaction
