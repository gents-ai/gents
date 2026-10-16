import Proofs.CanonicalOutput.Execution.Projection

/-!
# Background Tool Output: canonical source projection and paging (#937)

`read_tool_output` reads the exact physical tool's canonical immutable segment
source. Open output is its validated dense prefix; closed output is its
committed extent, so late suffix facts are inert. Missing or conflicting facts
are failure, never invented empty output. UTF-8 character-boundary snapping of
slice edges and the one-byte progress guard for a sub-codepoint budget remain
native representation details; conformance rows use ASCII and positive budgets.
-/

namespace Background
namespace ToolOutput

/-! ## Retained-window slice (mirrors `read_retained_output_slice`) -/

structure RetainedWindow where
  firstOffset : Nat
  retainedLen : Nat
  totalBytes : Nat
  deriving DecidableEq, Repr

def RetainedWindow.retainedEnd (window : RetainedWindow) : Nat :=
  window.firstOffset + window.retainedLen

def RetainedWindow.normalizedTotal (window : RetainedWindow) : Nat :=
  max window.totalBytes window.retainedEnd

structure SliceResult where
  start : Nat
  sliceLen : Nat
  nextOffset : Nat
  firstAvailableOffset : Nat
  totalBytes : Nat
  hasMore : Bool
  deriving DecidableEq, Repr

def readSlice (window : RetainedWindow) (offset maxBytes : Nat) : SliceResult :=
  let start := min (max offset window.firstOffset) window.retainedEnd
  let sliceLen := min maxBytes (window.retainedEnd - start)
  let nextOffset := start + sliceLen
  { start := start
  , sliceLen := sliceLen
  , nextOffset := nextOffset
  , firstAvailableOffset := window.firstOffset
  , totalBytes := window.normalizedTotal
  , hasMore := decide (nextOffset < window.normalizedTotal)
  }

theorem readSlice_contiguous_from_live_cursor
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_low : window.firstOffset ≤ offset)
    (h_high : offset ≤ window.retainedEnd) :
    (readSlice window offset maxBytes).start = offset ∧
      (readSlice window offset maxBytes).nextOffset =
        offset + (readSlice window offset maxBytes).sliceLen := by
  simp [readSlice, Nat.max_eq_left h_low, Nat.min_eq_left h_high]

theorem readSlice_within_retained
    (window : RetainedWindow) (offset maxBytes : Nat) :
    window.firstOffset ≤ (readSlice window offset maxBytes).start ∧
      (readSlice window offset maxBytes).nextOffset ≤ window.retainedEnd := by
  constructor
  · simp [readSlice, RetainedWindow.retainedEnd]
  · simp [readSlice]
    omega

theorem readSlice_past_end_empty
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_past : window.retainedEnd ≤ offset) :
    (readSlice window offset maxBytes).sliceLen = 0 ∧
      (readSlice window offset maxBytes).nextOffset = window.retainedEnd := by
  have h_max : max offset window.firstOffset = offset :=
    Nat.max_eq_left (Nat.le_trans (Nat.le_add_right _ _) h_past)
  simp [readSlice, h_max, Nat.min_eq_right h_past]

theorem readSlice_hasMore_iff
    (window : RetainedWindow) (offset maxBytes : Nat) :
    (readSlice window offset maxBytes).hasMore = true ↔
      (readSlice window offset maxBytes).nextOffset <
        (readSlice window offset maxBytes).totalBytes := by
  simp [readSlice]

theorem readSlice_progress
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_budget : 0 < maxBytes)
    (h_remaining : (readSlice window offset maxBytes).start <
      window.retainedEnd) :
    0 < (readSlice window offset maxBytes).sliceLen := by
  simp [readSlice] at h_remaining ⊢
  omega

/-! ## Canonical physical-source projection -/

inductive SourceState where | open | closed deriving DecidableEq, Repr

structure Projection where
  state : SourceState
  bytes : List UInt8
  deriving DecidableEq, Repr

def outputBytes? (streams : CanonicalOutput.Streams) : Option (List UInt8) :=
  if streams.isEmpty then some []
  else match streams.filter (fun stream => stream.1.kind == .toolOutput) with
    | [stream] => some stream.2
    | _ => none

def project (world : CanonicalOutput.Execution.World)
    (document : CanonicalOutput.DocId) : Option Projection := do
  let tool ← CanonicalOutput.Execution.ownedToolByDocument? world document
  if !CanonicalOutput.Execution.acceptedHeaderBindsTool world tool then none
  let coordinate : CanonicalOutput.Coordinate := ⟨tool.requestDoc, .tool tool.document⟩
  let writer : CanonicalOutput.Writer := .tool tool.document
  match (CanonicalOutput.closures world.segments coordinate).dedup with
  | [] => do
      let streams ← (CanonicalOutput.Execution.reconstructOpenPrefix
        world.segments coordinate writer).toOption
      let bytes ← outputBytes? streams
      some ⟨.open, bytes⟩
  | [closing] => do
      if closing.writer != writer then none
      let streams ← (CanonicalOutput.reconstructExtent world.segments closing).toOption
      let bytes ← outputBytes? streams
      some ⟨.closed, bytes⟩
  | _ => none

def Projection.window (projection : Projection) : RetainedWindow :=
  { firstOffset := 0, retainedLen := projection.bytes.length,
    totalBytes := projection.bytes.length }

theorem canonical_window_starts_at_zero (projection : Projection) :
    projection.window.firstOffset = 0 := rfl

end ToolOutput
end Background
