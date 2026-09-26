import Proofs.CanonicalOutput.Execution.Examples
import Proofs.StreamingResponse.ReasoningAudit

/-! Compaction and fallback captures are request-owned audit facts, never
assistant turns. Their two native capture-scope kinds are distinct coordinates. -/
namespace CanonicalOutput.Execution.AuxiliaryCases

open CanonicalOutput
open CanonicalOutput.Execution.Examples
open StreamingResponse

def source (kind : AuxiliaryKind) : Source := .auxiliary kind 1 0 0

def observed (kind : AuxiliaryKind) : Segment :=
  { ReasoningAudit.observed with coordinate := ⟨10, source kind⟩ }

def close (kind : AuxiliaryKind) (outcome : Outcome) : Segment :=
  { (observed kind) with id := 101, flush := none, close := some (.closed outcome 1 [1, 1, 1, 1]) }

def recoveryClose (kind : AuxiliaryKind) : Segment :=
  { (close kind .«partial») with createdAt := 20 }

def staleWriterClose (kind : AuxiliaryKind) (outcome : Outcome) : Segment :=
  { close kind outcome with writer := .request 8 }

def claimedWorld : World :=
  { world 5 with lease := { lease 5 with request := .claimed } }

def begunWorld? : Option World := do
  let begun ← RequestExecutionLease.step? claimedWorld.lease
    (.begin .mutationWriteGate 7)
  pure { claimedWorld with lease := begun }

def claimedAppendAccepted (kind : AuxiliaryKind) : Bool :=
  succeeds (appendRaw claimedWorld 7 (observed kind))

def begunAppendAccepted (kind : AuxiliaryKind) : Bool :=
  match begunWorld? with
  | none => false
  | some begun => succeeds (appendRaw begun 7 (observed kind))

def staleWriterCloseReplayAccepted (kind : AuxiliaryKind) (outcome : Outcome) : Bool :=
  succeeds (closeAuxiliary (world 5 [observed kind, close kind outcome]) 8
    (staleWriterClose kind outcome))

def emptyHeaderAccepted (kind : AuxiliaryKind) (outcome : Outcome) : Bool :=
  succeeds (acceptAndPublish (world 5 [observed kind]) 7
    (close kind outcome) (emptyAssistant 200 5) [])

def nativePublicationClose (kind : AuxiliaryKind) : Segment := close kind .complete

def nativePublicationMessage : MessageEnvelope :=
  { emptyAssistant 200 5 with
    header := { (emptyAssistant 200 5).header with
      refs := [⟨101, 0⟩, ⟨101, 2⟩, ⟨101, 3⟩] },
    blocks := [.reasoning none
      [.text ⟨⟨101, 0⟩, .full⟩ (some "S"),
       .encrypted ⟨⟨101, 2⟩, .full⟩,
       .redacted ⟨⟨101, 3⟩, .full⟩]] }

def nativePublicationAccepted (kind : AuxiliaryKind) : Bool :=
  succeeds (acceptAndPublish (world 5 [observed kind]) 7
    (nativePublicationClose kind) nativePublicationMessage [])

def observation (kind : AuxiliaryKind) (records : List Segment) : Observation :=
  { ReasoningAudit.observation records with
    target := ⟨⟨10, source kind⟩, .request 7, none⟩,
    owner := ⟨some (10, 7), []⟩,
    requestTerminal := false }

theorem compaction_and_fallback_are_distinct :
    source .compaction != source .compactionFallback := by native_decide

theorem both_kinds_append_under_request_lease :
    (#[.compaction, .compactionFallback] : Array AuxiliaryKind).all
      (fun kind => succeeds (appendRaw (world 5) 7 (observed kind))) = true := by
  native_decide

theorem claimed_rejects_auxiliary_until_existing_begin :
    (#[.compaction, .compactionFallback] : Array AuxiliaryKind).all
      (fun kind => !claimedAppendAccepted kind && begunWorld?.isSome &&
        begunAppendAccepted kind) = true := by
  native_decide

theorem stale_writer_cannot_replay_auxiliary_close :
    (#[.compaction, .compactionFallback] : Array AuxiliaryKind).all
      (fun kind => !staleWriterCloseReplayAccepted kind .complete) = true := by
  native_decide

theorem empty_header_cannot_publish_auxiliary :
    (#[.compaction, .compactionFallback] : Array AuxiliaryKind).all
      (fun kind => !emptyHeaderAccepted kind .complete) = true := by
  native_decide

theorem native_reasoning_header_cannot_publish_auxiliary :
    (#[.compaction, .compactionFallback] : Array AuxiliaryKind).all
      (fun kind => !nativePublicationAccepted kind) = true := by
  native_decide

theorem complete_and_partial_close_without_publication :
    (#[.complete, .«partial»] : Array Outcome).all (fun outcome =>
      match closeAuxiliary (world 5 [observed .compaction]) 7
          (close .compaction outcome) with
      | .ok post => post.messages.isEmpty && post.transcript.messages.isEmpty &&
          post.segments.length == 2
      | .error _ => false) = true := by
  native_decide

def auditExact (kind : AuxiliaryKind) : Bool :=
  match reconstructAuditPrefix (observation kind [observed kind]) none with
  | .ok streams => streams == ReasoningAudit.exactAudit
  | .error _ => false

structure Case where
  name : String
  kind : AuxiliaryKind
  outcome : Outcome

def cases : List Case :=
  [{ name := "compaction_complete", kind := .compaction, outcome := .complete },
   { name := "compaction_partial", kind := .compaction, outcome := .«partial» },
   { name := "fallback_complete", kind := .compactionFallback, outcome := .complete },
   { name := "fallback_partial", kind := .compactionFallback, outcome := .«partial» }]

theorem auxiliary_audit_is_exact_but_not_live_published :
    auditExact .compaction = true ∧
    project (observation .compaction [observed .compaction]) = .absent := by
  constructor <;> native_decide

theorem fallback_audit_is_exact_but_not_live_published :
    auditExact .compactionFallback = true ∧
    project (observation .compactionFallback
        [observed .compactionFallback]) = .absent := by
  constructor <;> native_decide

theorem recovery_closes_auxiliary_without_header :
    (match recoverExpiredBatch (world 20 [observed .compaction]) 7 8 5 30
        [⟨recoveryClose .compaction, none⟩] with
      | .ok post => post.messages.isEmpty && post.transcript.messages.isEmpty &&
          post.segments.length == 2
      | .error _ => false) = true := by
  native_decide

theorem recovery_header_cannot_promote_auxiliary :
    succeeds (recoverExpiredBatch (world 20 [observed .compaction]) 7 8 5 30
      [⟨recoveryClose .compaction,
        some (recoveryMessage 201 101 0 20)⟩]) = false := by
  native_decide

theorem accepted_header_cannot_promote_auxiliary :
    succeeds (acceptAndPublish (world 5 [observed .compaction]) 7
      (close .compaction .complete) (emptyAssistant 200 5) []) = false := by
  native_decide

end CanonicalOutput.Execution.AuxiliaryCases
