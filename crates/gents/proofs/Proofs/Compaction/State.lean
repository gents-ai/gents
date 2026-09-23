import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State
import Proofs.PromptAssembly.Executable
import Mathlib.Logic.Equiv.List

namespace Compaction

open Transcript (Sequence MessageId MessageRow MessageKind ToolResultKey
                 MessageRole StrictlyIncreasingMessages)

structure SummaryHandle where
  payload : Nat
  deriving DecidableEq, Repr

structure PromptView where
  sessionId        : SessionId
  messages         : List MessageRow
  summary          : Option SummaryHandle
  /-- Canonical immutable facts observed for each durable message id. The gate
  computes the projection, so callers cannot assert a Published tag directly. -/
  outputObservations : MessageId → Option StreamingResponse.Observation

namespace PromptView

def PairsClosedInMessages (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ caller, caller ∈ msgs ∧
        caller.role = .assistant ∧
        (∃ callIds, caller.kind = .assistantToolCalls callIds ∧ callId ∈ callIds)

/-- Every row announcing tool calls carries the assistant role.

A structural fact the transcript writer maintains — immutable assistant
publication sets `role := .assistant` alongside `kind := .assistantToolCalls`. Pair closure
needs it: `ActiveBlockValid` locates the *announcement* for a retained result,
and this is what makes that announcement an acceptable *caller*. -/
def AnnouncementsAreAssistant (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callIds, row.kind = .assistantToolCalls callIds → row.role = .assistant

/-- Premises for safe reduction of a row-level prompt view. Provider validity
and assistant announcement roles are separate obligations. The global provider
proof supplies validity only under its unique-call-id premise; it does not
establish arbitrary production content or repeated-call behavior. -/
structure ViewCoherent (v : PromptView) : Prop where
  pairs                  : PairsClosedInMessages v.messages
  ordered                : StrictlyIncreasingMessages v.messages
  blockValid             : PromptAssembly.ActiveBlockValid v.messages
  announcementsAssistant : AnnouncementsAreAssistant v.messages

/-- The selected prefix ends after an ordinary row. This is the persisted turn
boundary that makes the per-turn provider sanitizer independent of later tool
results, background messages, and provider call-id reuse. -/
def endsAtTurnBoundary (messages : List MessageRow) : Bool :=
  match messages.getLast? with
  | some row => row.kind == .ordinary
  | none => false

def encodeProviderKey (key : String) : Nat :=
  Encodable.encode (key.toList.map Char.toNat)

theorem charToNat_injective : Function.Injective Char.toNat := by
  intro left right h
  rw [← Char.ofNat_toNat left, ← Char.ofNat_toNat right, h]

/-- Model-only, collision-free symbolization. It is neither a durable ID nor a
runtime hash. The provider adapter still owes equality with its native string
key extraction at the boundary. -/
theorem encodeProviderKey_injective : Function.Injective encodeProviderKey := by
  intro left right h
  apply String.ext
  apply (List.map_injective_iff.mpr charToNat_injective)
  exact Encodable.encode_injective h

/-- Matches the production provider sanitizer key: prefer native `callId`, then
fall back to native `id`. -/
def providerCallSymbol (id : String) (callId : Option String) : Nat :=
  encodeProviderKey (callId.getD id)

def nativeToolCallSymbols (native : CanonicalOutput.ReconstructedMessage) : List Nat :=
  native.blocks.filterMap fun block => match block with
    | .toolCall _ id callId _ _ _ _ => some (providerCallSymbol id callId)
    | _ => none

def nativeToolResultFacts (native : CanonicalOutput.ReconstructedMessage) :
    List (CanonicalOutput.DocId × Nat) :=
  native.blocks.filterMap fun block => match block with
    | .toolResult physicalCall id callId _ =>
        some (physicalCall, providerCallSymbol id callId)
    | _ => none

def roleMatches (row : MessageRow) (message : CanonicalOutput.MessageEnvelope)
    (native : CanonicalOutput.ReconstructedMessage) : Bool :=
  match row.role, message.header.role, native.role with
  | .assistant, .assistant, .assistant | .user, .user, .user => true
  | _, _, _ => false

/-- Exact provider-row shape from reconstructed native blocks. Call symbols use
the native `callId.or(id)` key, encoded injectively above: equal native keys are
the same symbol even across distinct physical tool documents. Tool delivery
authorization remains bound separately to the exact physical document. A
header with multiple native result blocks is conservatively rejected because
one row cannot represent it; mixed text/result headers are also rejected because
the native sanitizer clears pending state on any non-result user content.
Assistant mixed content is represented only by its call-symbol set here, so
full content normalization remains part of the native adapter refinement.
`ToolResultKey.logicalResultId/payloadHash` remain
the existing typed result-owner fingerprint; this model binds its session but
does not invent a second durable result identity or hash native bytes.

Forked results keep native call IDs and exact origin content, but their
publication owner is the fork. `rowPublished` obtains the origin-validated
canonical projection before using this shape predicate. -/
def resultPublicationMatches (publication : CanonicalOutput.MessagePublication)
    (physical : CanonicalOutput.DocId) : Bool :=
  match publication with
  | .toolDelivery call => call == physical
  | .fork _ => true
  | _ => false

def kindMatches (row : MessageRow) (message : CanonicalOutput.MessageEnvelope)
    (native : CanonicalOutput.ReconstructedMessage) : Bool :=
  let calls := nativeToolCallSymbols native
  let results := nativeToolResultFacts native
  match row.kind with
  | .ordinary => calls.isEmpty && results.isEmpty
  | .assistantToolCalls callIds =>
      row.role == .assistant && !calls.isEmpty && decide calls.Nodup &&
        calls.toFinset == callIds && results.isEmpty
  | .toolResult call key =>
      row.role == .user && key.sessionId == row.sessionId && calls.isEmpty &&
        match native.blocks with
        | [.toolResult physicalCall id callId _] =>
            providerCallSymbol id callId == call &&
              resultPublicationMatches message.header.publication physicalCall
        | _ => false

/-- A row is eligible for compaction only when its exact immutable header and
all referenced payloads reconstruct. Immutability alone is insufficient:
loading, conflicted, live, and retained diagnostic observations are rejected. -/
def rowPublished (v : PromptView) (row : MessageRow) : Bool :=
  match v.outputObservations row.messageId with
  | some observation => match StreamingResponse.project observation with
    | .published message native =>
        message.header.id == row.messageId &&
          row.sessionId == v.sessionId && message.header.session == row.sessionId &&
          message.sequence == row.sequence && roleMatches row message native &&
          kindMatches row message native
    | _ => false
  | _ => false

/-- Executable stable-prefix gate. Besides reconstructed publication, the prefix
must already be a fixpoint of provider-input sanitization and end at a turn
boundary. Therefore reduction never blesses an orphan merely because its header
is immutable. -/
def safeToReduce (v : PromptView) : Prop :=
  endsAtTurnBoundary v.messages = true ∧
    PromptAssembly.sanitizeTurn v.messages = v.messages ∧
    v.messages.all (rowPublished v) = true

instance (v : PromptView) : Decidable (safeToReduce v) := by
  unfold safeToReduce
  infer_instance

end PromptView

end Compaction
