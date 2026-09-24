import Proofs.StreamingResponse.State

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

theorem partial_reasoning_has_no_recovery_header :
    recoveryText [(0, body), (1, signature), (2, encrypted), (3, redacted)] =
      .ok [] := by native_decide

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

theorem invalid_signature_run_cannot_be_published :
    validateReasoningSignature [invalidUtf8] [] ⟨⟨100, 0⟩, .full⟩ none =
      .error (.reconstruction (.extent .malformedRuns)) := by native_decide

structure AuditCase where
  name : String
  input : Observation
  expected : Except OpenError Streams
  deriving Repr

def auditCases : List AuditCase :=
  [ { name := "reasoning_audit_eof", input := observation [observed],
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
  , signatureCase "invalid_signature_bytes" [invalidUtf8]
      ⟨⟨100, 0⟩, .full⟩ none
  , signatureCase "provider_inline_signature_without_stream" [bodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ (some "S")
  , signatureCase "provider_unsigned_body_without_stream" [bodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ none
  , signatureCase "authored_inline_signature_without_stream" [authoredBodyOnlyCompleted]
      ⟨⟨100, 0⟩, .full⟩ (some "inline") ]

end StreamingResponse.ReasoningAudit
