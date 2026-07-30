import Proofs.Basic

/-!
# Background Tool Output: live ring buffer, paging, and read dispatch (#937)

`read_tool_output` (`background_tools.rs`) serves partial output for a native
background tool from one of three sources, decided by durable state and a
volatile registry:

* **terminal row** → the persisted tool completion (stdout + stderr combined
  behind one byte cursor);
* **running row with a live snapshot** → the in-memory ring buffer
  (`LiveToolOutputRegistry`), which retains only the most recent
  `LIVE_STREAM_CAPACITY_BYTES` — the *tail* — and exposes
  `first_offset`/`total_bytes_seen` so a reader can detect eviction;
* **running row with no snapshot** → empty output. The registry is volatile:
  after a daemon restart a still-`running` row has no buffer, which is
  exactly why startup recovery interrupts native background tools
  (`Recovery.restartDisposition`).

The previous witness (`r4c.read_tool_output.dispatch_by_state`) pinned the
opposite — `running_source = "none"` with a note that the ring buffer "was
never built" — while production had long been serving live snapshots. The
drift was invisible because the witness was string-pinned, never
runtime-driven. This module is the corrected model; the witness values and
the paging conformance rows are computed from it.

Model granularity: offsets are byte counts (`Nat`). Three Rust behaviors sit
below this model as representation details (`boundary`): UTF-8
character-boundary snapping of slice edges, the `from_utf8_lossy` rendering
of a live ring window that may start mid-codepoint after eviction, and the
one-byte progress guard for a sub-codepoint byte budget (unreachable in
production — `validated_max_bytes` floors at 256). The conformance rows use
ASCII payloads and positive budgets, where all three are inert.
-/

namespace Subagent
namespace ToolOutput

/-! ## Retained-window slice (mirrors `read_retained_output_slice`) -/

/-- What a reader sees of one output stream: the retained byte window
    `[firstOffset, firstOffset + retainedLen)` out of `totalBytes` produced
    so far. For persisted terminal output `firstOffset = 0` and
    `totalBytes = retainedLen` (nothing is ever evicted); for a live ring
    snapshot `firstOffset = totalBytes - retainedLen`. -/
structure RetainedWindow where
  firstOffset : Nat
  retainedLen : Nat
  totalBytes : Nat
  deriving DecidableEq, Repr

def RetainedWindow.retainedEnd (window : RetainedWindow) : Nat :=
  window.firstOffset + window.retainedLen

/-- Rust normalizes `total_bytes` up to the retained end
    (`total_bytes.max(retained_end)`). -/
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

/-- Byte-granularity model of `read_retained_output_slice`: clamp the cursor
    into the retained window, take at most `maxBytes`, and report the
    contiguous continuation cursor plus eviction/total metadata. -/
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

/-- P1: pages are contiguous from a live cursor — when the requested offset
    is inside the retained window, the slice starts exactly there and the
    continuation cursor is `offset + returned`, so repeated reads have no
    gap and no overlap. -/
theorem readSlice_contiguous_from_live_cursor
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_low : window.firstOffset ≤ offset)
    (h_high : offset ≤ window.retainedEnd) :
    (readSlice window offset maxBytes).start = offset ∧
      (readSlice window offset maxBytes).nextOffset =
        offset + (readSlice window offset maxBytes).sliceLen := by
  simp [readSlice, Nat.max_eq_left h_low, Nat.min_eq_left h_high]

/-- P2: eviction is detectable — a cursor below the retained window is
    snapped up to `firstAvailableOffset`, which the reader can compare
    against its requested offset to see that
    `[offset, firstAvailableOffset)` was dropped. -/
theorem readSlice_eviction_detectable
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_evicted : offset < window.firstOffset) :
    (readSlice window offset maxBytes).start = window.firstOffset ∧
      offset < (readSlice window offset maxBytes).firstAvailableOffset := by
    have h_max : max offset window.firstOffset = window.firstOffset :=
      Nat.max_eq_right (Nat.le_of_lt h_evicted)
    have h_min :
        min window.firstOffset window.retainedEnd = window.firstOffset :=
      Nat.min_eq_left (Nat.le_add_right _ _)
    constructor
    · simp [readSlice, RetainedWindow.retainedEnd, h_max, h_min]
    · simpa [readSlice] using h_evicted

/-- P3: the slice never leaves the retained window. -/
theorem readSlice_within_retained
    (window : RetainedWindow) (offset maxBytes : Nat) :
    window.firstOffset ≤ (readSlice window offset maxBytes).start ∧
      (readSlice window offset maxBytes).nextOffset ≤ window.retainedEnd := by
  constructor
  · simp [readSlice, RetainedWindow.retainedEnd]
    omega
  · simp [readSlice]
    omega

/-- P4: a cursor at or past the retained end returns an empty slice parked
    at the retained end (no spinning, no fabricated bytes). -/
theorem readSlice_past_end_empty
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_past : window.retainedEnd ≤ offset) :
    (readSlice window offset maxBytes).sliceLen = 0 ∧
      (readSlice window offset maxBytes).nextOffset = window.retainedEnd := by
  have h_max : max offset window.firstOffset = offset :=
    Nat.max_eq_left (Nat.le_trans (Nat.le_add_right _ _) h_past)
  simp [readSlice, h_max, Nat.min_eq_right h_past]

/-- P5: `hasMore` is exactly "the continuation cursor has not reached the
    total produced bytes". -/
theorem readSlice_hasMore_iff
    (window : RetainedWindow) (offset maxBytes : Nat) :
    (readSlice window offset maxBytes).hasMore = true ↔
      (readSlice window offset maxBytes).nextOffset <
        (readSlice window offset maxBytes).totalBytes := by
  simp [readSlice]

/-- P6: progress — a positive budget against a non-exhausted retained window
    always returns at least one byte, so pagination cannot wedge. -/
theorem readSlice_progress
    (window : RetainedWindow) (offset maxBytes : Nat)
    (h_budget : 0 < maxBytes)
    (h_remaining : (readSlice window offset maxBytes).start <
      window.retainedEnd) :
    0 < (readSlice window offset maxBytes).sliceLen := by
  simp [readSlice] at h_remaining ⊢
  omega

/-! ## Ring tail retention (mirrors `RingBuffer::append`) -/

/-- Abstract state of one live ring: how many bytes are retained (bounded by
    capacity) out of how many were ever produced. -/
structure RingState where
  capacity : Nat
  retainedLen : Nat
  totalSeen : Nat
  deriving DecidableEq, Repr

def RingState.WellFormed (ring : RingState) : Prop :=
  ring.retainedLen ≤ ring.capacity ∧ ring.retainedLen ≤ ring.totalSeen

def RingState.firstOffset (ring : RingState) : Nat :=
  ring.totalSeen - ring.retainedLen

/-- Appending `count` bytes: totals always advance; the retained window
    grows until capacity and then holds (front eviction keeps the tail). -/
def RingState.append (ring : RingState) (count : Nat) : RingState :=
  { ring with
      totalSeen := ring.totalSeen + count
      retainedLen := min (ring.retainedLen + count) ring.capacity
  }

theorem RingState.append_wellFormed
    (ring : RingState) (count : Nat) (h_wf : ring.WellFormed) :
    (ring.append count).WellFormed := by
  obtain ⟨h_cap, h_total⟩ := h_wf
  constructor
  · exact Nat.min_le_right _ _
  · simp [RingState.append]
    omega

/-- Tail retention never rewinds the reader's floor: the first available
    offset is monotone under appends, so a page that was evicted stays
    evicted (and detectably so, by P2). -/
theorem RingState.append_firstOffset_monotone
    (ring : RingState) (count : Nat) (h_wf : ring.WellFormed) :
    ring.firstOffset ≤ (ring.append count).firstOffset := by
  obtain ⟨h_cap, h_total⟩ := h_wf
  simp [RingState.firstOffset, RingState.append]
  omega

/-- The snapshot a ring serves to `readSlice`. -/
def RingState.window (ring : RingState) : RetainedWindow :=
  { firstOffset := ring.firstOffset
  , retainedLen := ring.retainedLen
  , totalBytes := ring.totalSeen
  }

/-- A well-formed ring's snapshot is internally consistent: the retained end
    equals the total produced, so `readSlice` reports `hasMore = false` once
    the cursor reaches the live tail. -/
theorem RingState.window_retainedEnd_eq_total
    (ring : RingState) (h_wf : ring.WellFormed) :
    ring.window.retainedEnd = ring.totalSeen := by
  obtain ⟨_, h_total⟩ := h_wf
  simp [RingState.window, RetainedWindow.retainedEnd, RingState.firstOffset]
  omega

/-! ## Read dispatch (mirrors `handle_read_tool_output`) -/

/-- Where one `read_tool_output` call sources its bytes. -/
inductive ReadSource where
  | liveRingBuffer
  | persistedToolCompletion
  | noLiveBuffer
  deriving DecidableEq, Repr

namespace ReadSource

def toContract : ReadSource → String
  | .liveRingBuffer => "live_ring_buffer"
  | .persistedToolCompletion => "persisted_tool_completion"
  | .noLiveBuffer => "none"

end ReadSource

/-- Dispatch on the durable row state (`terminal`) and the volatile registry
    (`hasLiveSnapshot`). -/
def readDispatch (terminal hasLiveSnapshot : Bool) : ReadSource :=
  if terminal then .persistedToolCompletion
  else if hasLiveSnapshot then .liveRingBuffer
  else .noLiveBuffer

/-- D1: a terminal row always serves the persisted completion — never the
    (possibly stale) live buffer. -/
theorem terminal_reads_persisted (hasLiveSnapshot : Bool) :
    readDispatch true hasLiveSnapshot = .persistedToolCompletion := rfl

/-- D2: a running row with a live snapshot serves the ring buffer. -/
theorem running_snapshot_reads_live :
    readDispatch false true = .liveRingBuffer := rfl

/-- D3: a running row with no snapshot — the post-restart shape, since the
    registry is volatile — serves empty output from no live source. This is
    the durable/volatile seam that motivates the restart interrupt
    (`Recovery.restartDisposition`): the work's partial output cannot
    survive the process. -/
theorem restart_running_reads_empty :
    readDispatch false false = .noLiveBuffer := rfl

/-- D4: dispatch is exhaustive and mutually exclusive over its three
    sources. -/
theorem readDispatch_exhaustive (terminal hasLiveSnapshot : Bool) :
    (readDispatch terminal hasLiveSnapshot = .persistedToolCompletion ↔
        terminal = true) ∧
      (readDispatch terminal hasLiveSnapshot = .liveRingBuffer ↔
        (terminal = false ∧ hasLiveSnapshot = true)) ∧
      (readDispatch terminal hasLiveSnapshot = .noLiveBuffer ↔
        (terminal = false ∧ hasLiveSnapshot = false)) := by
  cases terminal <;> cases hasLiveSnapshot <;> simp [readDispatch]

end ToolOutput
end Subagent
