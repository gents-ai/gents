import Proofs.Session.StreamingResponse.State

namespace StreamingResponse.ReasoningAudit

open CanonicalOutput

local instance {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    DecidableEq (Except ε α)
  | .error a, .error b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.error.inj e))
  | .error _, .ok _ => .isFalse nofun
  | .ok _, .error _ => .isFalse nofun
  | .ok a, .ok b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.ok.inj e))

def coordinate : Coordinate := ⟨10, .provider 0 0 0⟩
def body : Declaration := ⟨0, 0, .reasoning, none, none⟩
def signature : Declaration := ⟨0, 0, .signature, none, none⟩
def encrypted : Declaration := ⟨0, 1, .encrypted, none, none⟩
def redacted : Declaration := ⟨0, 2, .redacted, none, none⟩

def observed : Segment :=
  { id := 100, coordinate, writer := .request 7,
    flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 1, some encrypted⟩, ⟨3, 1, some redacted⟩],
      [65, 83, 69, 82]⟩,
    close := none, createdAt := 5 }

def retraction : Segment :=
  { observed with id := 101, flush := none, close := some .retracted }

def partialClose : Segment :=
  { observed with id := 102, flush := none, close := some (.closed Outcome.«partial» 1 [1, 1, 1, 1]) }

def observation (records : List Segment) : Observation :=
  { request := 10, session := 1, records, messages := [],
    deniedHeaders := [], deniedSegments := [], dependencyDenials := [],
    owner := ⟨none, []⟩, target := ⟨coordinate, .request 7, none⟩,
    requestTerminal := true, terminalSelection := none }

def exactAudit : Streams :=
  [(body, [65]), (signature, [83]), (encrypted, [69]), (redacted, [82])]

def signatureContinuation : Segment :=
  { observed with id := 104, flush := some ⟨1, [⟨1, 1, none⟩], [84]⟩, createdAt := 6 }

def afterGap : Segment :=
  { observed with id := 105, flush := some ⟨2, [⟨1, 1, none⟩], [85]⟩, createdAt := 7 }

def continuedAudit : Streams :=
  [(body, [65]), (signature, [83, 84]), (encrypted, [69]), (redacted, [82])]

def completed : Segment :=
  { observed with close := some (.closed .complete 1 [1, 1, 1, 1]) }

def bodyOnlyCompleted : Segment :=
  { observed with
    flush := some ⟨0, [⟨0, 1, some body⟩], [65]⟩
    close := some (.closed .complete 1 [1]) }

def authoredBodyOnlyCompleted : Segment :=
  { bodyOnlyCompleted with coordinate := ⟨10, .authored 0⟩ }

theorem body_and_signature_share_native_part :
    consumeRuns [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩]
      [65, 83] [] = .ok [(body, [65]), (signature, [83])] := by native_decide

theorem two_body_kinds_conflict :
    consumeRuns [⟨0, 1, some body⟩,
      ⟨1, 1, some { body with kind := .summary }⟩] [65, 66] [] =
        .error .malformedRuns := by native_decide

theorem two_signature_streams_conflict :
    consumeRuns [⟨0, 1, some signature⟩, ⟨1, 1, some signature⟩]
      [65, 66] [] = .error .malformedRuns := by native_decide

theorem eof_audit_keeps_all_received_types_and_bytes :
    reconstructAuditPrefix (observation [observed]) none = .ok exactAudit := by
  native_decide

theorem signature_delta_continuation_is_retained_without_closure :
    reconstructAuditPrefix (observation [observed, signatureContinuation]) none =
      .ok continuedAudit := by native_decide

theorem absent_replica_ordinal_keeps_only_the_known_prefix :
    reconstructAuditPrefix (observation [observed, afterGap]) none =
      .ok exactAudit := by native_decide

theorem partial_audit_keeps_all_received_types_and_bytes :
    reconstructAuditPrefix (observation [observed, partialClose]) none = .ok exactAudit := by
  native_decide

theorem partial_reasoning_without_stop_has_no_recovery_header :
    recoveryPayloads exactAudit = .ok [] := by native_decide

def stoppedObserved : Segment :=
  { observed with
    flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩], [65, 83]⟩ }

def stoppedClose : Segment :=
  { observed with id := 110, flush := none, close := some (.closed .«partial» 1 [1, 1, 0]) }

def duplicateStops : Segment :=
  { observed with flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩,
       ⟨3, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩], [65, 83]⟩ }

theorem duplicate_stops_cannot_validate :
    (match reconstructAuditPrefix (observation [duplicateStops]) none with
      | .error _ => true | .ok _ => false) = true := by
  native_decide

def recoveredReasoning : MessageEnvelope :=
  { header :=
      { id := 111, session := 1, request := some 10, origin := none
        refs := [⟨110, 0⟩], outcome := .«partial», role := .assistant
        publication := .requestRecovery 7 }
    key := "recovered-reasoning", sequence := 0, nativeId := none
    blocks := [.reasoning none [.text ⟨⟨110, 0⟩, .full⟩ (some "S")]]
    createdAt := 7 }

theorem partial_stopped_reasoning_reconstructs_with_exact_signature :
    (reconstructMessage [stoppedObserved, stoppedClose] [] recoveredReasoning).isOk = true := by
  native_decide

def unstoppedRecovery : MessageEnvelope :=
  { recoveredReasoning with
    header := { recoveredReasoning.header with refs := [⟨102, 0⟩] }
    blocks := [.reasoning none [.text ⟨⟨102, 0⟩, .full⟩ (some "S")]] }

theorem partial_reasoning_without_stop_cannot_publish :
    (reconstructMessage [observed, partialClose] [] unstoppedRecovery).isOk = false := by
  native_decide

theorem retracted_audit_keeps_all_received_types_and_bytes :
    reconstructAuditPrefix (observation [observed, retraction]) none = .ok exactAudit := by
  native_decide

theorem incomplete_or_retracted_is_not_replayable :
    reconstructPayload [observed] [] ⟨100, 0⟩ =
      .error (.lookup .invalidReference) ∧
    reconstructExtent [observed, retraction] retraction = .error .invalidClosure := by
  native_decide

theorem conflicting_observation_is_not_audited_as_one_winner :
    reconstructAuditPrefix
      (observation [observed, { observed with id := 103 }]) none =
        .error .conflicted := by native_decide

def samePositionBodyConflict : Segment :=
  { observed with flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some { encrypted with part := 0 }⟩],
      [65, 69]⟩ }

def samePositionSignatureConflict : Segment :=
  { observed with flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 1, some signature⟩], [65, 83, 84]⟩ }

theorem body_kinds_at_same_native_position_fail_audit :
    (match reconstructAuditPrefix (observation [samePositionBodyConflict]) none with
      | .error _ => true | .ok _ => false) = true := by
  native_decide

theorem signature_streams_at_same_native_position_fail_audit :
    (match reconstructAuditPrefix (observation [samePositionSignatureConflict]) none with
      | .error _ => true | .ok _ => false) = true := by
  native_decide

theorem published_signature_agrees_with_retained_bytes :
    validateReasoningSignature [completed] [] ⟨⟨100, 0⟩, .full⟩ (some "S") =
      .ok () := by native_decide

theorem published_signature_disagreement_is_rejected :
    validateReasoningSignature [completed] [] ⟨⟨100, 0⟩, .full⟩ (some "other") =
      .error .signatureMismatch := by native_decide

theorem absent_header_signature_disagrees_with_retained_signature :
    validateReasoningSignature [completed] [] ⟨⟨100, 0⟩, .full⟩ none =
      .error .signatureMismatch := by native_decide

theorem provider_inline_signature_requires_retained_stream :
    validateReasoningSignature [bodyOnlyCompleted] [] ⟨⟨100, 0⟩, .full⟩ (some "S") =
      .error .signatureMismatch := by native_decide

theorem provider_unsigned_reasoning_without_signature_stream_is_valid :
    validateReasoningSignature [bodyOnlyCompleted] [] ⟨⟨100, 0⟩, .full⟩ none =
      .ok () := by native_decide

theorem authored_inline_signature_needs_no_provider_stream :
    validateReasoningSignature [authoredBodyOnlyCompleted] [] ⟨⟨100, 0⟩, .full⟩
      (some "inline") = .ok () := by native_decide

def invalidUtf8 : Segment :=
  { completed with flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 1, some encrypted⟩, ⟨3, 1, some redacted⟩],
      [65, 255, 69, 82]⟩ }

/-- Native segment payloads are UTF-8 strings, so production never writes this
run; the model rejects it as malformed. -/
theorem invalid_signature_run_cannot_be_published :
    validateReasoningSignature [invalidUtf8] [] ⟨⟨100, 0⟩, .full⟩ none =
      .error (.reconstruction (.extent .malformedRuns)) := by native_decide

def sigFragA : Segment :=
  { observed with
    id := 120,
    flush := some ⟨0, [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩], [65, 83]⟩,
    createdAt := 5 }

def sigFragB : Segment :=
  { observed with
    id := 121,
    flush := some ⟨1, [⟨1, 1, none⟩], [49]⟩,
    createdAt := 6 }

def sigFragStop : Segment :=
  { observed with
    id := 122,
    flush := some ⟨1,
      [⟨1, 1, none⟩, ⟨2, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩], [49]⟩,
    createdAt := 6 }

def interruptedSecondBlock : Segment :=
  { observed with
    id := 123,
    flush := some ⟨2,
      [⟨3, 1, some { body with block := 1 }⟩,
       ⟨4, 1, some { signature with block := 1 }⟩], [66, 50]⟩,
    createdAt := 7 }

def foreignAttemptStop : Segment :=
  { stoppedObserved with
    id := 124,
    coordinate := ⟨10, .provider 0 0 1⟩,
    createdAt := 6 }

def foreignRequestStop : Segment :=
  { stoppedObserved with
    id := 125,
    coordinate := ⟨11, .provider 0 0 0⟩,
    createdAt := 6 }

def redactedBlock : Declaration := ⟨0, 0, .redacted, none, none⟩

def redactedStopped : Segment :=
  { observed with
    id := 126,
    flush := some ⟨0,
      [⟨0, 1, some redactedBlock⟩,
       ⟨1, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩], [82]⟩,
    createdAt := 5 }

def redactedUnstopped : Segment :=
  { observed with
    id := 127,
    flush := some ⟨0, [⟨0, 1, some redactedBlock⟩], [82]⟩,
    createdAt := 5 }

def textBlock2 : Declaration := ⟨2, 0, .text, none, none⟩

def stoppedThenGapThenText : Segment :=
  { observed with
    id := 128,
    flush := some ⟨0,
      [⟨0, 1, some body⟩, ⟨1, 1, some signature⟩,
       ⟨2, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩,
       ⟨3, 1, some { body with block := 1 }⟩,
       ⟨4, 1, some textBlock2⟩], [65, 83, 66, 84]⟩,
    createdAt := 5 }

def lateStopAfterRecovery : Segment :=
  { observed with
    id := 129,
    flush := some ⟨1, [⟨2, 0, some ⟨0, 0, .reasoningStop, none, none⟩⟩], []⟩,
    createdAt := 11 }

structure RecoveryEntry where
  stream : Nat
  blockIndex : Nat
  kind : String
  signature : Option String
  deriving DecidableEq, Repr

structure RecoveryPlan where
  closeStreamBytes : List Nat
  retained : List RecoveryEntry
  deriving DecidableEq, Repr

private def recoverySignature (streams : Streams) (declaration : Declaration) :
    Except ExtentError (Option String) :=
  match streams.filter fun entry =>
      entry.1.block == declaration.block && entry.1.part == declaration.part &&
        entry.1.kind == .signature with
  | [] => .ok none
  | [(_, bytes)] =>
      match utf8? bytes with
      | some text => .ok (some text)
      | none => .error .malformedRuns
  | _ => .error .malformedRuns

private def recoveryEntry (streams : Streams) (entry : Nat × Declaration) :
    Except ExtentError RecoveryEntry :=
  match entry.2.kind with
  | .text => .ok ⟨entry.1, entry.2.block, "text", none⟩
  | .reasoning => do
      let sig ← recoverySignature streams entry.2
      .ok ⟨entry.1, entry.2.block, "reasoning", sig⟩
  | .redacted => .ok ⟨entry.1, entry.2.block, "redacted", none⟩
  | _ => .error .malformedRuns

/-- Pure recovery plan for one open provider source, composing the canonical
extent and `recoveryPayloads` owners without a second journal. -/
def planRecovery (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) : Except ExtentError RecoveryPlan := do
  let matching := sourceRecords records coordinate
  if matching.isEmpty then .error .invalidClosure
  else if matching.any fun r => r.close.isSome then .error .invalidClosure
  else if !writerMatchesSource coordinate writer then .error .invalidWriter
  else if matching.any fun r => r.writer != writer then .error .invalidWriter
  else if matching.any fun r => !exactIdentityAt matching r then .error .malformedRuns
  else if matching.any fun r => r.flush.isNone then .error .malformedRuns
  else if !timestampsNondecreasing matching then .error .malformedRuns
  else
    let count := (matching.filterMap fun r => r.flush.map (·.ordinal)).eraseDups.length
    let flushes ← (List.range count).mapM (flushAt matching writer)
    let streams ← consumeFlushes flushes []
    let selected ← recoveryPayloads streams
    let retained ← selected.mapM (recoveryEntry streams)
    .ok { closeStreamBytes := streams.map fun s => s.2.length, retained }

theorem completed_then_interrupted_audit_retains_both_blocks :
    reconstructAuditPrefix
      (observation [sigFragA, sigFragStop, interruptedSecondBlock]) none =
      .ok [(body, [65]), (signature, [83, 49]),
        (⟨0, 0, .reasoningStop, none, none⟩, []),
        ({ body with block := 1 }, [66]),
        ({ signature with block := 1 }, [50])] := by
  native_decide

theorem completed_then_interrupted_recovery_selects_only_stopped_block :
    planRecovery [sigFragA, sigFragStop, interruptedSecondBlock] coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 2, 0, 1, 1],
            retained := [⟨0, 0, "reasoning", some "S1"⟩] } := by
  native_decide

theorem signature_fragments_without_stop_are_audit_only :
    reconstructAuditPrefix (observation [sigFragA, sigFragB]) none =
      .ok [(body, [65]), (signature, [83, 49])] ∧
    planRecovery [sigFragA, sigFragB] coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 2], retained := [] } := by
  native_decide

theorem foreign_source_stop_cannot_complete_target_block :
    planRecovery [sigFragA, sigFragB, foreignAttemptStop, foreignRequestStop]
      coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 2], retained := [] } := by
  native_decide

theorem duplicate_stopped_segment_arrival_is_idempotent_for_recovery :
    planRecovery [sigFragA, sigFragStop, sigFragStop] coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 2, 0],
            retained := [⟨0, 0, "reasoning", some "S1"⟩] } := by
  native_decide

theorem stopped_redacted_recovers_and_unstopped_redacted_is_audit_only :
    planRecovery [redactedStopped] coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 0], retained := [⟨0, 0, "redacted", none⟩] } ∧
    planRecovery [redactedUnstopped] coordinate (.request 7) =
      .ok { closeStreamBytes := [1], retained := [] } := by
  native_decide

theorem interleaved_unstopped_block_preserves_order_and_indices :
    planRecovery [stoppedThenGapThenText] coordinate (.request 7) =
      .ok { closeStreamBytes := [1, 1, 0, 1, 1],
            retained := [⟨0, 0, "reasoning", some "S"⟩, ⟨4, 2, "text", none⟩] } := by
  native_decide

structure RecoveryCase where
  name : String
  input : Observation
  deriving Repr

def recoveryCases : List RecoveryCase :=
  [ { name := "completed_then_interrupted_block",
      input := observation [sigFragA, sigFragStop, interruptedSecondBlock] }
  , { name := "signature_fragments_without_stop",
      input := observation [sigFragA, sigFragB] }
  , { name := "signature_fragments_before_stop",
      input := observation [sigFragA, sigFragStop] }
  , { name := "foreign_attempt_stop_does_not_complete_target",
      input := observation [sigFragA, sigFragB, foreignAttemptStop] }
  , { name := "foreign_request_stop_does_not_complete_target",
      input := observation [sigFragA, sigFragB, foreignRequestStop] }
  , { name := "duplicate_stopped_segment_arrival",
      input := observation [sigFragA, sigFragStop, sigFragStop] }
  , { name := "duplicate_stop_declarations_rejected",
      input := observation [duplicateStops] }
  , { name := "late_signature_after_stop_rejected",
      input := observation [stoppedObserved, signatureContinuation] }
  , { name := "stopped_redacted_recovers",
      input := observation [redactedStopped] }
  , { name := "unstopped_redacted_is_audit_only",
      input := observation [redactedUnstopped] }
  , { name := "interleaved_unstopped_block_keeps_order_and_indices",
      input := observation [stoppedThenGapThenText] } ]

structure AuditCase where
  name : String
  input : Observation
  expected : Except OpenError Streams
  deriving Repr

def auditCases : List AuditCase :=
  [ { name := "reasoning_audit_stopped", input := observation [stoppedObserved],
      expected := reconstructAuditPrefix (observation [stoppedObserved]) none }
  , { name := "reasoning_audit_completed_then_interrupted",
      input := observation [sigFragA, sigFragStop, interruptedSecondBlock],
      expected := reconstructAuditPrefix
        (observation [sigFragA, sigFragStop, interruptedSecondBlock]) none }
  , { name := "reasoning_audit_signature_fragments_without_stop",
      input := observation [sigFragA, sigFragB],
      expected := reconstructAuditPrefix (observation [sigFragA, sigFragB]) none }
  , { name := "reasoning_audit_foreign_source_stop_ignored",
      input := observation [sigFragA, sigFragB, foreignAttemptStop, foreignRequestStop],
      expected := reconstructAuditPrefix
        (observation [sigFragA, sigFragB, foreignAttemptStop, foreignRequestStop]) none }
  , { name := "reasoning_audit_duplicate_arrival", input := observation [stoppedObserved, stoppedObserved],
      expected := reconstructAuditPrefix (observation [stoppedObserved, stoppedObserved]) none }
  , { name := "reasoning_audit_duplicate_stops", input := observation [duplicateStops],
      expected := reconstructAuditPrefix (observation [duplicateStops]) none }
  , { name := "reasoning_audit_stopped_late_signature", input :=
        observation [stoppedObserved, signatureContinuation],
      expected := reconstructAuditPrefix (observation [stoppedObserved, signatureContinuation]) none }
  , { name := "reasoning_audit_eof", input := observation [observed],
      expected := reconstructAuditPrefix (observation [observed]) none }
  , { name := "reasoning_audit_signature_delta", input :=
        observation [observed, signatureContinuation],
      expected := reconstructAuditPrefix
        (observation [observed, signatureContinuation]) none }
  , { name := "reasoning_audit_replica_gap_prefix", input :=
        observation [observed, afterGap],
      expected := reconstructAuditPrefix (observation [observed, afterGap]) none }
  , { name := "reasoning_audit_partial", input := observation [observed, partialClose],
      expected := reconstructAuditPrefix (observation [observed, partialClose]) none }
  , { name := "reasoning_audit_retracted", input := observation [observed, retraction],
      expected := reconstructAuditPrefix (observation [observed, retraction]) none }
  , { name := "reasoning_audit_conflict", input :=
        observation [observed, { observed with id := 103 }],
      expected := reconstructAuditPrefix
        (observation [observed, { observed with id := 103 }]) none }
  , { name := "reasoning_audit_body_position_conflict", input :=
        observation [samePositionBodyConflict],
      expected := reconstructAuditPrefix (observation [samePositionBodyConflict]) none }
  , { name := "reasoning_audit_signature_position_conflict", input :=
        observation [samePositionSignatureConflict],
      expected := reconstructAuditPrefix (observation [samePositionSignatureConflict]) none } ]

structure SignatureCase where
  name : String
  records : List Segment
  payload : PayloadSpec
  signature : Option String
  accepted : Bool
  deriving Repr

def signatureCase (name : String) (records : List Segment)
    (payload : PayloadSpec) (signature : Option String) : SignatureCase :=
  { name, records, payload, signature,
    accepted := (validateReasoningSignature records [] payload signature).isOk }

def signatureCases : List SignatureCase :=
  [ signatureCase "retained_signature_matches" [completed]
      ⟨⟨100, 0⟩, .full⟩ (some "S")
  , signatureCase "retained_signature_disagrees" [completed]
      ⟨⟨100, 0⟩, .full⟩ (some "other")
  , signatureCase "retained_signature_missing_in_header" [completed]
      ⟨⟨100, 0⟩, .full⟩ none
  , signatureCase "provider_inline_signature_without_stream" [bodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ (some "S")
  , signatureCase "provider_unsigned_body_without_stream" [bodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ none
  , signatureCase "authored_inline_signature_without_stream" [authoredBodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ (some "inline") ]

end StreamingResponse.ReasoningAudit
