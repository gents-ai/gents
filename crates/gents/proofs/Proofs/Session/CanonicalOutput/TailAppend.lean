import Proofs.Session.CanonicalOutput.OpenShape
import Proofs.Session.CanonicalOutput.ToolDelivery

/-!
# Bounded tail planning for tool-output appends

Appending a raw tool-output chunk only needs the bounded tail of the open
source (the highest-ordinal window, closure witness, and retry-identity witness)
when every flush persists its immutable cumulative `streamBytesAfter`.
Full reconstruction (`reconstructExtent`) and `inspectOpenShape` still validate
every segment's `streamBytesAfter` against the running stream sums at close and
read time.
-/

namespace CanonicalOutput

def toolOutputDeclaration : Declaration :=
  ⟨0, 0, .toolOutput, none, none⟩

def expectedToolRun (ordinal payloadBytes : Nat) : List Run :=
  [⟨0, payloadBytes, if ordinal == 0 then some toolOutputDeclaration else none⟩]

def validToolContinuationRuns : List Run → Option Nat
  | [] => some 0
  | run :: rest =>
      if run.stream == 0 && run.declaration.isNone && run.bytes > 0 then
        (validToolContinuationRuns rest).map (fun tail => run.bytes + tail)
      else none

def validSingleToolFlush (ordinal : Nat) (runs : List Run) : Option Nat :=
  match runs with
  | [] => none
  | first :: rest =>
      if ordinal == 0 then
        if first.stream == 0 && first.declaration == some toolOutputDeclaration then
          (validToolContinuationRuns rest).map (fun tail => first.bytes + tail)
        else none
      else
        validToolContinuationRuns (first :: rest)

inductive TailAppendKind where
  | fresh
  | replay
  deriving DecidableEq, Repr

structure TailAppendPlan where
  kind : TailAppendKind
  docId : Option DocId
  ordinal : Nat
  startByte : Nat
  endByte : Nat
  deriving DecidableEq, Repr

inductive TailAppendError where
  | emptyPayload
  | invalidWriter
  | closedSource
  | ambiguousRetry
  | conflictingRetry
  | malformedTail
  | conflictingTwin
  | timestampRegressed
  deriving DecidableEq, Repr

private def maxOrdinalOf (records : List Segment) : Nat :=
  (records.filterMap (fun r => r.flush.map Flush.ordinal)).foldl max 0

private def atOrdinal (records : List Segment) (ordinal : Nat) : List Segment :=
  records.filter (fun r => r.flush.any (fun f => f.ordinal == ordinal))

/-- Validates a non-empty deduplicated tail window (up to 3 highest-ordinal rows),
returning `(maxOrdinal, cumulativeBytes, latestCreatedAt)`. -/
def validateCleanTail (data : List Segment) : Except TailAppendError (Nat × Nat × Time) := do
  if data.isEmpty then .error .malformedTail
  else
    let maxOrd := maxOrdinalOf data
    match atOrdinal data maxOrd with
    | [latest] =>
        match latest.flush, latest.streamBytesAfter with
        | some latestFlush, some [tailBytes] =>
            match validSingleToolFlush maxOrd latestFlush.runs with
            | none => .error .malformedTail
            | some latestBytes =>
                if maxOrd == 0 then
                  if latestBytes != tailBytes then .error .malformedTail
                  else .ok (0, tailBytes, latest.createdAt)
                else
                  match atOrdinal data (maxOrd - 1) with
                  | [prev] =>
                      match prev.flush, prev.streamBytesAfter with
                      | some prevFlush, some [prevBytes] =>
                          match validSingleToolFlush (maxOrd - 1) prevFlush.runs with
                          | none => .error .malformedTail
                          | some _ =>
                              if latest.createdAt < prev.createdAt then
                                .error .timestampRegressed
                              else if prevBytes + latestBytes != tailBytes then
                                .error .malformedTail
                              else
                                .ok (maxOrd, tailBytes, latest.createdAt)
                      | _, _ => .error .malformedTail
                  | [] => .error .malformedTail
                  | _ :: _ :: _ => .error .conflictingTwin
        | _, _ => .error .malformedTail
    | [] => .error .malformedTail
    | _ :: _ :: _ => .error .conflictingTwin

/-- Pure bounded decision for planning the next tool-output append from:
- `tail`: bounded highest-ordinal window (`order: { ordinal: DESC }, limit: 3`)
- `closureWitness`: bounded closure lookup (`is_close: { _eq: true }, limit: 2`)
- `sameAttempt`: bounded retry-identity lookup (`created_at: { _eq: $t }, limit: 2`) -/
def planToolAppendFromTail
    (tail closureWitness sameAttempt : List Segment)
    (coordinate : Coordinate) (writer : Writer)
    (createdAt : Time) (payloadBytes : Nat) : Except TailAppendError TailAppendPlan := do
  if payloadBytes == 0 then .error .emptyPayload
  else if !writerMatchesSource coordinate writer then .error .invalidWriter
  else
    let tailScoped := (sourceRecords tail coordinate).map erasePayload
    let closeScoped := sourceRecords closureWitness coordinate
    let retryScoped := (sourceRecords sameAttempt coordinate).map erasePayload
    if !closeScoped.isEmpty ||
        tailScoped.any (fun r => r.close.isSome) ||
        retryScoped.any (fun r => r.close.isSome) then
      .error .closedSource
    else
      let observed := tailScoped ++ retryScoped
      if !(observed.all fun r => r.writer == writer) then .error .invalidWriter
      else if !(observed.all fun r => exactIdentityAt observed r) then .error .conflictingTwin
      else if !(observed.all fun r => r.flush.isSome) then .error .malformedTail
      else
        let retryCandidates :=
          (retryScoped ++ tailScoped.filter (fun r => r.createdAt == createdAt)).dedup
        match retryCandidates with
        | [] =>
            let data := tailScoped.dedup
            if data.isEmpty then
              .ok ⟨.fresh, none, 0, 0, payloadBytes⟩
            else
              let (maxOrd, tailBytes, latestTime) ← validateCleanTail data
              if createdAt < latestTime then .error .timestampRegressed
              else .ok ⟨.fresh, none, maxOrd + 1, tailBytes, tailBytes + payloadBytes⟩
        | [existing] =>
            match existing.flush, existing.streamBytesAfter with
            | some existingFlush, some [endByte] =>
                let existingOrd := existingFlush.ordinal
                let data := tailScoped.dedup
                if (atOrdinal data existingOrd).any (fun r => r != existing) then
                  .error .conflictingTwin
                else if existingFlush.runs != expectedToolRun existingOrd payloadBytes then
                  .error .conflictingRetry
                else if endByte < payloadBytes then
                  .error .malformedTail
                else
                  let startByte := endByte - payloadBytes
                  let prior := data.filter (fun r => r != existing)
                  if existingOrd == 0 then
                    if startByte != 0 then .error .malformedTail
                    else if prior.isEmpty then
                      .ok ⟨.replay, some existing.id, 0, 0, endByte⟩
                    else
                      let (_, _, latestTime) ← validateCleanTail data
                      if latestTime < existing.createdAt then .error .timestampRegressed
                      else .ok ⟨.replay, some existing.id, 0, 0, endByte⟩
                  else
                    let maxOrd := max (maxOrdinalOf data) existingOrd
                    if existingOrd == maxOrd then
                      match atOrdinal prior (existingOrd - 1) with
                      | [prev] =>
                          match prev.flush, prev.streamBytesAfter with
                          | some prevFlush, some [prevBytes] =>
                              if (validSingleToolFlush (existingOrd - 1) prevFlush.runs).isNone then
                                .error .malformedTail
                              else if existing.createdAt < prev.createdAt then
                                .error .timestampRegressed
                              else if prevBytes != startByte then
                                .error .malformedTail
                              else
                                .ok ⟨.replay, some existing.id, existingOrd, startByte, endByte⟩
                          | _, _ => .error .malformedTail
                      | [] => .error .malformedTail
                      | _ :: _ :: _ => .error .conflictingTwin
                    else if existingOrd < maxOrd then
                      let (_, _, latestTime) ← validateCleanTail data
                      if latestTime < existing.createdAt then .error .timestampRegressed
                      else .ok ⟨.replay, some existing.id, existingOrd, startByte, endByte⟩
                    else .error .malformedTail
            | _, _ => .error .malformedTail
        | _ :: _ :: _ => .error .ambiguousRetry

/-- The bounded window of at most 3 highest-ordinal rows for `coordinate`,
modeling `order: { ordinal: DESC }, limit: 3`. -/
def tailWindow (records : List Segment) (coordinate : Coordinate) : List Segment :=
  let src := sourceRecords records coordinate
  let data := src.filter (fun r => r.flush.isSome)
  if data.isEmpty then src.take 3
  else
    let maxOrd := maxOrdinalOf data
    let top := atOrdinal data maxOrd
    let prev := if maxOrd == 0 then [] else atOrdinal data (maxOrd - 1)
    let prev2 := if maxOrd < 2 then [] else atOrdinal data (maxOrd - 2)
    (top ++ prev ++ prev2).take 3

def toolAppendSegment (id : DocId) (coordinate : Coordinate) (writer : Writer)
    (ordinal startByte : Nat) (payload : List UInt8) (createdAt : Time) : Segment :=
  { id := id
    coordinate := coordinate
    writer := writer
    flush := some ⟨ordinal, expectedToolRun ordinal payload.length, payload⟩
    close := none
    createdAt := createdAt
    streamBytesAfter := some [startByte + payload.length] }

theorem empty_payload_rejects_tail_append
    (tail closureWitness sameAttempt : List Segment)
    (coordinate : Coordinate) (writer : Writer) (createdAt : Time) :
    planToolAppendFromTail tail closureWitness sameAttempt coordinate writer createdAt 0 =
      .error .emptyPayload := by
  simp [planToolAppendFromTail]

theorem invalid_writer_rejects_tail_append
    (tail closureWitness sameAttempt : List Segment)
    (coordinate : Coordinate) (writer : Writer) (createdAt : Time) (payloadBytes : Nat)
    (hpos : payloadBytes > 0)
    (hwriter : writerMatchesSource coordinate writer = false) :
    planToolAppendFromTail tail closureWitness sameAttempt coordinate writer createdAt payloadBytes =
      .error .invalidWriter := by
  have hne : (payloadBytes == 0) = false := by
    simp [Nat.ne_of_gt hpos]
  simp [planToolAppendFromTail, hne, hwriter]

theorem closed_source_rejects_tail_append
    (tail closureWitness sameAttempt : List Segment)
    (coordinate : Coordinate) (writer : Writer) (createdAt : Time) (payloadBytes : Nat)
    (closing : Segment)
    (hpos : payloadBytes > 0)
    (hwriter : writerMatchesSource coordinate writer = true)
    (hmem : closing ∈ sourceRecords closureWitness coordinate) :
    planToolAppendFromTail tail closureWitness sameAttempt coordinate writer createdAt payloadBytes =
      .error .closedSource := by
  have hne : (payloadBytes == 0) = false := by simp [Nat.ne_of_gt hpos]
  have hnonempty : (sourceRecords closureWitness coordinate).isEmpty = false := by
    cases h : sourceRecords closureWitness coordinate with
    | nil => simp [h] at hmem
    | cons _ _ => rfl
  simp [planToolAppendFromTail, hne, hwriter, hnonempty]

theorem conflicting_tail_twin_rejects_append
    (first twin : Segment) (coordinate : Coordinate) (writer : Writer)
    (createdAt : Time) (payloadBytes ordinal : Nat)
    (hpos : payloadBytes > 0)
    (hwriter : writerMatchesSource coordinate writer = true)
    (hcoord1 : first.coordinate = coordinate)
    (hcoord2 : twin.coordinate = coordinate)
    (hwr1 : first.writer = writer)
    (hwr2 : twin.writer = writer)
    (hclose1 : first.close = none)
    (hclose2 : twin.close = none)
    (hid : first.id ≠ twin.id)
    (htime1 : first.createdAt ≠ createdAt)
    (htime2 : twin.createdAt ≠ createdAt)
    (hflush1 : ∃ f1, first.flush = some f1 ∧ f1.ordinal = ordinal)
    (hflush2 : ∃ f2, twin.flush = some f2 ∧ f2.ordinal = ordinal) :
    planToolAppendFromTail [first, twin] [] [] coordinate writer createdAt payloadBytes =
      .error .conflictingTwin := by
  rcases hflush1 with ⟨f1, hf1, ho1⟩
  rcases hflush2 with ⟨f2, hf2, ho2⟩
  have hne : (payloadBytes == 0) = false := by simp [Nat.ne_of_gt hpos]
  have heq_ne : erasePayload first ≠ erasePayload twin := by
    intro h
    have := congrArg Segment.id h
    exact hid this
  have hid_sym : twin.id ≠ first.id := Ne.symm hid
  simp [planToolAppendFromTail, sourceRecords, hne, hwriter, hcoord1, hcoord2,
    hwr1, hwr2, hclose1, hclose2, exactIdentityAt, erasePayload, hf1, hf2,
    hid, hid_sym, htime1, htime2, heq_ne, validateCleanTail, maxOrdinalOf, atOrdinal, ho1, ho2,
    Bind.bind, Except.bind]

theorem regressed_timestamp_rejects_tail_append
    (latest : Segment) (coordinate : Coordinate) (writer : Writer)
    (createdAt : Time) (payloadBytes bytes : Nat)
    (hpos : payloadBytes > 0)
    (hwriter : writerMatchesSource coordinate writer = true)
    (hcoord : latest.coordinate = coordinate)
    (hwr : latest.writer = writer)
    (hclose : latest.close = none)
    (hflush : latest.flush = some ⟨0, expectedToolRun 0 bytes, List.replicate bytes 65⟩)
    (hbytes : latest.streamBytesAfter = some [bytes])
    (hreg : createdAt < latest.createdAt) :
    planToolAppendFromTail [latest] [] [] coordinate writer createdAt payloadBytes =
      .error .timestampRegressed := by
  have hne : (payloadBytes == 0) = false := by simp [Nat.ne_of_gt hpos]
  have htime : latest.createdAt ≠ createdAt := Nat.ne_of_gt hreg
  simp [planToolAppendFromTail, sourceRecords, hne, hwriter, hcoord, hwr, hclose,
    exactIdentityAt, erasePayload, hflush, hbytes, htime, validateCleanTail, maxOrdinalOf,
    atOrdinal, validSingleToolFlush, expectedToolRun, validToolContinuationRuns, hreg,
    Bind.bind, Except.bind]

theorem missing_stream_bytes_after_rejects_tail_append
    (latest : Segment) (coordinate : Coordinate) (writer : Writer)
    (createdAt : Time) (payloadBytes bytes : Nat)
    (hpos : payloadBytes > 0)
    (hwriter : writerMatchesSource coordinate writer = true)
    (hcoord : latest.coordinate = coordinate)
    (hwr : latest.writer = writer)
    (hclose : latest.close = none)
    (hflush : latest.flush = some ⟨0, expectedToolRun 0 bytes, List.replicate bytes 65⟩)
    (hnone : latest.streamBytesAfter = none)
    (htime : latest.createdAt ≠ createdAt) :
    planToolAppendFromTail [latest] [] [] coordinate writer createdAt payloadBytes =
      .error .malformedTail := by
  have hne : (payloadBytes == 0) = false := by simp [Nat.ne_of_gt hpos]
  simp [planToolAppendFromTail, sourceRecords, hne, hwriter, hcoord, hwr, hclose,
    exactIdentityAt, erasePayload, hflush, hnone, htime, validateCleanTail, maxOrdinalOf,
    atOrdinal, Bind.bind, Except.bind]

/-- A canonical tool-output prefix whose segments for `coordinate` are exactly a
contiguous list of open single-stream tool flushes `[0..n-1]`, each carrying its
immutable cumulative `streamBytesAfter` and nondecreasing timestamps. -/
structure ValidToolPrefix (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time) : Prop where
  writerMatches : writerMatchesSource coordinate writer = true
  exactLength : (sourceRecords records coordinate).length = n
  nodupIds : (records.map Segment.id).Nodup
  allOpen : ∀ r ∈ sourceRecords records coordinate,
    r.writer = writer ∧ r.close = none ∧ r.flush.isSome
  atIndex : ∀ (i : Nat) (hi : i < (sourceRecords records coordinate).length),
    let r := (sourceRecords records coordinate)[i]
    ∃ payload bytesBefore,
      payload.length > 0 ∧
      r.flush = some ⟨i, expectedToolRun i payload.length, payload⟩ ∧
      r.streamBytesAfter = some [bytesBefore + payload.length] ∧
      (String.fromUTF8? (ByteArray.mk payload.toArray)).isSome = true ∧
      (i = 0 → bytesBefore = 0) ∧
      (i + 1 = n → bytesBefore + payload.length = totalBytes ∧ some r.createdAt = lastTime) ∧
      (∀ (j : Nat) (hj : j < (sourceRecords records coordinate).length),
        i ≤ j → r.createdAt ≤ (sourceRecords records coordinate)[j].createdAt) ∧
      (∀ (hnext : i + 1 < (sourceRecords records coordinate).length),
        ∃ nextPayload,
          (sourceRecords records coordinate)[i + 1].flush =
            some ⟨i + 1, expectedToolRun (i + 1) nextPayload.length, nextPayload⟩ ∧
          (sourceRecords records coordinate)[i + 1].streamBytesAfter =
            some [bytesBefore + payload.length + nextPayload.length])
  emptyTotals : n = 0 → totalBytes = 0 ∧ lastTime = none

theorem empty_is_valid_tool_prefix (request call : DocId) :
    ValidToolPrefix [] ⟨request, .tool call⟩ (.tool call) 0 0 none where
  writerMatches := by simp [writerMatchesSource]
  exactLength := rfl
  nodupIds := List.nodup_nil
  allOpen := by intro r hr; simp [sourceRecords] at hr
  atIndex := by intro i hi; simp [sourceRecords] at hi
  emptyTotals := fun _ => ⟨rfl, rfl⟩

private lemma utf8DecodeChar_ascii65 (a : ByteArray) (i : Nat)
    (hi : i < a.size) (hbyte : a.data[i]'hi = (65 : UInt8)) :
    String.utf8DecodeChar? a i = some 'A' := by
  have hget : a[i]? = some (65 : UInt8) := by
    change a.data[i]? = some (65 : UInt8)
    rw [Array.getElem?_eq_getElem hi, hbyte]
  unfold String.utf8DecodeChar?
  simp only [hget, Bind.bind, Option.bind]
  rfl

private lemma utf8_replicate_A (k : Nat) :
    (String.fromUTF8? (ByteArray.mk (List.replicate k (65 : UInt8)).toArray)).isSome = true := by
  set a : ByteArray := ByteArray.mk (List.replicate k (65 : UInt8)).toArray
  have hsize : a.size = k := by simp [a, ByteArray.size]
  have hloop : ∀ d i, i + d = k → String.validateUTF8.loop a i = some () := by
    intro d
    induction d with
    | zero =>
      intro i hi
      rw [String.validateUTF8.loop.eq_def]
      have hnotlt : ¬ (i < a.size) := by omega
      simp [hnotlt]
    | succ d ih =>
      intro i hi
      rw [String.validateUTF8.loop.eq_def]
      have hlt : i < a.size := by omega
      have hbyte : a.data[i]'hlt = (65 : UInt8) := by
        simp [a]
      have hdec := utf8DecodeChar_ascii65 a i hlt hbyte
      have hsz : ('A').utf8Size = 1 := by native_decide
      simp only [hlt, ↓reduceIte, hdec, hsz, Bind.bind, Option.bind]
      exact ih (i + 1) (by omega)
  have hval : String.validateUTF8 a = true := by
    simp [String.validateUTF8, hloop k 0 (Nat.zero_add k)]
  simp [String.fromUTF8?, hval]

private lemma sourceRecords_append_same (records : List Segment) (coordinate : Coordinate)
    (seg : Segment) (hcoord : seg.coordinate = coordinate) :
    sourceRecords (records ++ [seg]) coordinate = sourceRecords records coordinate ++ [seg] := by
  simp [sourceRecords, List.filter_append, hcoord]

private lemma consumeRuns_expectedToolRun_zero (payload : List UInt8)
    (hutf8 : (String.fromUTF8? (ByteArray.mk payload.toArray)).isSome = true) :
    consumeRuns (expectedToolRun 0 payload.length) payload [] =
      .ok [(toolOutputDeclaration, payload)] := by
  have htake : payload.take payload.length = payload := by simp
  have hdrop : payload.drop payload.length = [] := by simp
  have hnone : (String.fromUTF8? (ByteArray.mk payload.toArray)).isNone = false := by
    cases h : String.fromUTF8? (ByteArray.mk payload.toArray) <;> simp_all
  simp [expectedToolRun, consumeRuns, htake, hdrop, hnone,
    declarationPositionFresh, declarationWellFormed, toolOutputDeclaration,
    reasoningStopped, validStopDeclaration, Bind.bind, Except.bind]

private lemma consumeRuns_expectedToolRun_succ (k : Nat) (payload prev : List UInt8)
    (hpos : payload.length > 0)
    (hutf8 : (String.fromUTF8? (ByteArray.mk payload.toArray)).isSome = true) :
    consumeRuns (expectedToolRun (k + 1) payload.length) payload [(toolOutputDeclaration, prev)] =
      .ok [(toolOutputDeclaration, prev ++ payload)] := by
  have htake : payload.take payload.length = payload := by simp
  have hdrop : payload.drop payload.length = [] := by simp
  have hnone : (String.fromUTF8? (ByteArray.mk payload.toArray)).isNone = false := by
    cases h : String.fromUTF8? (ByteArray.mk payload.toArray) <;> simp_all
  have hne : payload.length ≠ 0 := Nat.ne_of_gt hpos
  simp [expectedToolRun, consumeRuns, htake, hdrop, hnone, hne,
    reasoningStopped, toolOutputDeclaration, List.modify, Bind.bind, Except.bind]

private lemma filter_atIndex_unique (xs : List Segment) (k : Nat)
    (hord : ∀ (i : Nat) (hi : i < xs.length),
      xs[i].flush.any (fun f => f.ordinal == i) = true)
    (hk : k < xs.length) :
    xs.filter (fun r => r.flush.any (fun f => f.ordinal == k)) = [xs[k]] := by
  have hgen : ∀ (ys : List Segment) (base : Nat),
      (∀ (i : Nat) (hi : i < ys.length),
        ys[i].flush.any (fun f => f.ordinal == base + i) = true) →
      ∀ m, ys.filter (fun r => r.flush.any (fun f => f.ordinal == m)) =
        if h : base ≤ m ∧ m - base < ys.length then [ys[m - base]'(h.2)] else [] := by
    intro ys
    induction ys with
    | nil =>
      intro base _ m
      simp
    | cons y rest ih =>
      intro base hall m
      have hy : y.flush.any (fun f => f.ordinal == base) = true := by
        simpa using hall 0 (Nat.zero_lt_succ _)
      have hrest : ∀ (i : Nat) (hi : i < rest.length),
          rest[i].flush.any (fun f => f.ordinal == (base + 1) + i) = true := by
        intro i hi
        have := hall (i + 1) (Nat.succ_lt_succ hi)
        simpa [Nat.add_assoc, Nat.add_comm 1 i] using this
      have ih_m := ih (base + 1) hrest m
      simp only [List.filter_cons, ih_m]
      by_cases hmb : m = base
      · subst hmb
        have hnot : ¬ (m + 1 ≤ m ∧ m - (m + 1) < rest.length) := by omega
        simp [hy, hnot]
      · have hyf : y.flush.any (fun f => f.ordinal == m) = false := by
          cases hf : y.flush with
          | none => simp [hf] at hy
          | some f =>
            simp only [hf, Option.any_some, beq_iff_eq] at hy
            simp only [hf, Option.any_some, beq_eq_false_iff_ne]
            intro heq
            exact hmb (heq.symm.trans hy)
        simp only [hyf, Bool.false_eq_true, ↓reduceIte]
        by_cases hlt : base < m
        · have hge : base ≤ m := Nat.le_of_lt hlt
          have hge1 : base + 1 ≤ m := hlt
          have hsub : m - base = (m - (base + 1)) + 1 := by omega
          by_cases hlen : m - (base + 1) < rest.length
          · have hlen' : m - base < rest.length + 1 := by omega
            simp [hge1, hlen, hge, hlen', hsub]
          · have hlen' : ¬ (m - base < rest.length + 1) := by omega
            simp [hge1, hlen, hlen']
        · have hnot1 : ¬ (base + 1 ≤ m ∧ m - (base + 1) < rest.length) := by omega
          have hnot2 : ¬ (base ≤ m ∧ m - base < rest.length + 1) := by omega
          simp [hnot1, hnot2]
  have := hgen xs 0 (by simpa using hord) k
  simpa [hk] using this

private lemma foldl_max_ge (xs : List Nat) (acc : Nat) :
    acc ≤ xs.foldl max acc := by
  induction xs generalizing acc with
  | nil => exact Nat.le_refl _
  | cons x rest ih =>
    simp only [List.foldl_cons]
    exact Nat.le_trans (Nat.le_max_left acc x) (ih (max acc x))

private lemma foldl_max_mem_le (xs : List Nat) (acc x : Nat) (hx : x ∈ xs) :
    x ≤ xs.foldl max acc := by
  induction xs generalizing acc with
  | nil => simp at hx
  | cons y rest ih =>
    simp only [List.foldl_cons]
    rcases List.mem_cons.mp hx with rfl | hmem
    · exact Nat.le_trans (Nat.le_max_right acc x) (foldl_max_ge rest (max acc x))
    · exact ih (max acc y) hmem

private lemma foldl_max_upper (xs : List Nat) (acc bound : Nat)
    (hacc : acc ≤ bound) (hxs : ∀ x ∈ xs, x ≤ bound) :
    xs.foldl max acc ≤ bound := by
  induction xs generalizing acc with
  | nil => exact hacc
  | cons x rest ih =>
    simp only [List.foldl_cons]
    exact ih (max acc x) (Nat.max_le.mpr ⟨hacc, hxs x (by simp)⟩)
      (fun y hy => hxs y (by simp [hy]))

private lemma nodup_ids_exactIdentityAt (xs : List Segment)
    (h : (xs.map Segment.id).Nodup) :
    xs.all (exactIdentityAt xs) = true := by
  rw [List.all_eq_true]
  intro r hr
  simp only [exactIdentityAt, List.all_eq_true, List.mem_filter, beq_iff_eq]
  intro other ⟨hother, hid⟩
  obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hr
  obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hother
  have hij : i = j := by
    have hnodup := List.pairwise_iff_getElem.mp h
    by_contra hne
    rcases Nat.lt_or_gt_of_ne hne with hlt | hgt
    · exact (hnodup i j (by simpa using hi) (by simpa using hj) hlt) (by simpa using hid.symm)
    · exact (hnodup j i (by simpa using hj) (by simpa using hi) hgt) (by simpa using hid)
  subst hij
  rfl

private lemma nodup_ids_filter (xs : List Segment) (p : Segment → Bool)
    (h : (xs.map Segment.id).Nodup) : ((xs.filter p).map Segment.id).Nodup := by
  induction xs with
  | nil => exact List.nodup_nil
  | cons x rest ih =>
    simp only [List.map_cons, List.nodup_cons] at h
    simp only [List.filter_cons]
    split
    · simp only [List.map_cons, List.nodup_cons]
      refine ⟨fun hmem => h.1 ?_, ih h.2⟩
      obtain ⟨y, hy, hyid⟩ := List.mem_map.mp hmem
      exact List.mem_map.mpr ⟨y, (List.mem_filter.mp hy).1, hyid⟩
    · exact ih h.2

private lemma map_id_erasePayload (xs : List Segment) :
    (xs.map erasePayload).map Segment.id = xs.map Segment.id := by
  induction xs with
  | nil => rfl
  | cons x rest ih => simp [erasePayload, ih]

private lemma valid_prefix_source_nodup_ids (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime) :
    ((sourceRecords records coordinate).map Segment.id).Nodup :=
  nodup_ids_filter records _ h.nodupIds

private lemma valid_prefix_closures_empty (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime) :
    closures records coordinate = [] := by
  simp only [closures, List.filter_eq_nil_iff]
  intro r hr
  have hclose := (h.allOpen r hr).2.1
  simp [hclose]

private lemma valid_prefix_ordinals (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime)
    (i : Nat) (hi : i < (sourceRecords records coordinate).length) :
    (sourceRecords records coordinate)[i].flush.any (fun f => f.ordinal == i) = true := by
  obtain ⟨payload, _, _, hflush, _, _, _, _, _, _⟩ := h.atIndex i hi
  simp [hflush]

private lemma valid_prefix_maxOrdinal (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (m totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer (m + 1) totalBytes lastTime) :
    maxOrdinalOf (sourceRecords records coordinate) = m := by
  have hlen : (sourceRecords records coordinate).length = m + 1 := h.exactLength
  have hle : maxOrdinalOf (sourceRecords records coordinate) ≤ m := by
    apply foldl_max_upper _ 0 m (Nat.zero_le m)
    intro ord hord
    obtain ⟨r, hr, hrf⟩ := List.mem_filterMap.mp hord
    obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hr
    obtain ⟨payload, _, _, hflush, _, _, _, _, _, _⟩ := h.atIndex i hi
    simp [hflush] at hrf
    omega
  have hge : m ≤ maxOrdinalOf (sourceRecords records coordinate) := by
    have hm : m < (sourceRecords records coordinate).length := by omega
    obtain ⟨payload, _, _, hflush, _, _, _, _, _, _⟩ := h.atIndex m hm
    apply foldl_max_mem_le
    exact List.mem_filterMap.mpr ⟨(sourceRecords records coordinate)[m]'hm, List.getElem_mem hm, by
      simp [hflush]⟩
  exact Nat.le_antisymm hle hge

private lemma valid_prefix_tailWindow_eq (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime) :
    match n with
    | 0 => tailWindow records coordinate = []
    | 1 => ∃ s0, sourceRecords records coordinate = [s0] ∧ tailWindow records coordinate = [s0]
    | m + 2 =>
        tailWindow records coordinate =
          if m = 0 then
            [(sourceRecords records coordinate)[1]'(by rw [h.exactLength]; omega),
             (sourceRecords records coordinate)[0]'(by rw [h.exactLength]; omega)]
          else
            [(sourceRecords records coordinate)[m + 1]'(by rw [h.exactLength]; omega),
             (sourceRecords records coordinate)[m]'(by rw [h.exactLength]; omega),
             (sourceRecords records coordinate)[m - 1]'(by rw [h.exactLength]; omega)] := by
  have hdata : (sourceRecords records coordinate).filter (fun r => r.flush.isSome) =
      sourceRecords records coordinate := by
    apply List.filter_eq_self.mpr
    intro r hr
    exact (h.allOpen r hr).2.2
  cases n with
  | zero =>
    have hnil : sourceRecords records coordinate = [] := List.eq_nil_of_length_eq_zero h.exactLength
    simp [tailWindow, hdata, hnil]
  | succ k =>
    have hnonempty : (sourceRecords records coordinate).isEmpty = false := by
      cases hs : sourceRecords records coordinate with
      | nil => have := h.exactLength; simp [hs] at this
      | cons _ _ => rfl
    have hmax := valid_prefix_maxOrdinal records coordinate writer k totalBytes lastTime h
    have hord := valid_prefix_ordinals records coordinate writer (k + 1) totalBytes lastTime h
    have htop : atOrdinal (sourceRecords records coordinate) k =
        [(sourceRecords records coordinate)[k]'(by rw [h.exactLength]; omega)] :=
      filter_atIndex_unique (sourceRecords records coordinate) k hord (by rw [h.exactLength]; omega)
    cases k with
    | zero =>
      dsimp only
      obtain ⟨s0, hs⟩ := List.length_eq_one_iff.mp h.exactLength
      refine ⟨s0, hs, ?_⟩
      have htop0 : atOrdinal [s0] 0 = [s0] := by simpa [hs] using htop
      have hmax0 : maxOrdinalOf [s0] = 0 := by simpa [hs] using hmax
      rw [tailWindow, hdata, hs]
      simp [hmax0, htop0]
    | succ m =>
      have hprev : atOrdinal (sourceRecords records coordinate) m =
          [(sourceRecords records coordinate)[m]'(by rw [h.exactLength]; omega)] :=
        filter_atIndex_unique (sourceRecords records coordinate) m hord (by rw [h.exactLength]; omega)
      have hm1_ne0 : (m + 1 == 0) = false := by simp
      have hm1_sub1 : m + 1 - 1 = m := Nat.succ_sub_one m
      by_cases hm : m = 0
      · subst hm
        simp [tailWindow, hdata, hnonempty, hmax, htop, hprev]
      · have hprev2 : atOrdinal (sourceRecords records coordinate) (m - 1) =
            [(sourceRecords records coordinate)[m - 1]'(by rw [h.exactLength]; omega)] :=
          filter_atIndex_unique (sourceRecords records coordinate) (m - 1) hord
            (by rw [h.exactLength]; omega)
        have hnotlt : ¬ (m + 1 < 2) := by omega
        have hsub2 : m + 1 - 2 = m - 1 := by omega
        simp [tailWindow, hdata, hnonempty, hmax, hm1_ne0, hm1_sub1, hnotlt, hsub2,
          htop, hprev, hprev2, hm]

/-- Bounded tail planning agrees with full prefix accounting (`inspectOpenShape`)
for every valid open tool-output prefix of arbitrary length `n`. -/
theorem planToolAppendFromTail_agrees_with_inspectOpenShape
    (records : List Segment) (coordinate : Coordinate) (writer : Writer)
    (n totalBytes : Nat) (lastTime : Option Time)
    (createdAt : Time) (payloadBytes : Nat)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime)
    (hpos : payloadBytes > 0)
    (htime : ∀ t, lastTime = some t → t ≤ createdAt)
    (hfreshTime : ∀ r ∈ sourceRecords records coordinate, r.createdAt ≠ createdAt) :
    planToolAppendFromTail
      (tailWindow records coordinate) (closures records coordinate) []
      coordinate writer createdAt payloadBytes =
      .ok ⟨.fresh, none, n, totalBytes, totalBytes + payloadBytes⟩ := by
  have hne : (payloadBytes == 0) = false := by simp [Nat.ne_of_gt hpos]
  have hcl : closures records coordinate = [] :=
    valid_prefix_closures_empty records coordinate writer n totalBytes lastTime h
  have htw := valid_prefix_tailWindow_eq records coordinate writer n totalBytes lastTime h
  have hnodup := valid_prefix_source_nodup_ids records coordinate writer n totalBytes lastTime h
  cases n with
  | zero =>
    obtain ⟨rfl, rfl⟩ := h.emptyTotals rfl
    simp [planToolAppendFromTail, hne, h.writerMatches, hcl, htw, sourceRecords]
  | succ k =>
    cases k with
    | zero =>
      rcases htw with ⟨s0, hs0, htail⟩
      have hmem0 : s0 ∈ sourceRecords records coordinate := by simp [hs0]
      have hcoord0 : s0.coordinate = coordinate := by
        have := (List.mem_filter.mp hmem0).2
        simpa using this
      have hopen0 := h.allOpen s0 hmem0
      have htime0 : s0.createdAt ≠ createdAt := hfreshTime s0 hmem0
      have hidx0 : 0 < (sourceRecords records coordinate).length := by rw [hs0]; simp
      obtain ⟨p0, b0, hp0pos, hf0, hbytes0, _, hb0, htot0, _, _⟩ := h.atIndex 0 hidx0
      have hs0eq : (sourceRecords records coordinate)[0]'hidx0 = s0 := by simp [hs0]
      rw [hs0eq] at hf0 hbytes0 htot0
      have hb0z : b0 = 0 := hb0 rfl
      subst hb0z
      obtain ⟨htotBytes, hlastEq⟩ := htot0 rfl
      simp only [Nat.zero_add] at hbytes0 htotBytes
      subst htotBytes
      have hleTime : s0.createdAt ≤ createdAt := htime s0.createdAt hlastEq.symm
      have hnotlt : ¬ (createdAt < s0.createdAt) := Nat.not_lt.mpr hleTime
      simp [planToolAppendFromTail, hne, h.writerMatches, hcl, htail, sourceRecords,
        hcoord0, hopen0.1, hopen0.2.1, exactIdentityAt, erasePayload, hf0, hbytes0,
        htime0, validateCleanTail, maxOrdinalOf, atOrdinal, validSingleToolFlush,
        expectedToolRun, validToolContinuationRuns, hnotlt, Bind.bind, Except.bind]
    | succ m =>
      have hlen : (sourceRecords records coordinate).length = m + 2 := h.exactLength
      have h1lt : m + 1 < (sourceRecords records coordinate).length := by omega
      have h0lt : m < (sourceRecords records coordinate).length := by omega
      let latest := (sourceRecords records coordinate)[m + 1]'h1lt
      let prev := (sourceRecords records coordinate)[m]'h0lt
      have hcoord1 : latest.coordinate = coordinate := by
        simpa using (List.mem_filter.mp (List.getElem_mem h1lt)).2
      have hcoord0 : prev.coordinate = coordinate := by
        simpa using (List.mem_filter.mp (List.getElem_mem h0lt)).2
      have hopen1 := h.allOpen latest (List.getElem_mem h1lt)
      have hopen0 := h.allOpen prev (List.getElem_mem h0lt)
      have htime1 : latest.createdAt ≠ createdAt := hfreshTime latest (List.getElem_mem h1lt)
      have htime0 : prev.createdAt ≠ createdAt := hfreshTime prev (List.getElem_mem h0lt)
      obtain ⟨p1, b1, hp1pos, hf1, hbytes1, _, _, htot1, _, _⟩ := h.atIndex (m + 1) h1lt
      obtain ⟨p0, b0, hp0pos, hf0, hbytes0, _, _, _, hmono0, hnext0⟩ := h.atIndex m h0lt
      obtain ⟨p1', hf1', hbytes1'⟩ := hnext0 h1lt
      have hp1eq : p1' = p1 := by
        have := congrArg (Option.map Flush.payload) (hf1'.symm.trans hf1)
        simpa using this
      subst p1'
      have hb1eq : b1 + p1.length = b0 + p0.length + p1.length := by
        have := hbytes1.symm.trans hbytes1'
        simpa using this
      obtain ⟨htotBytes, hlastEq⟩ := htot1 rfl
      have hleTime : latest.createdAt ≤ createdAt := htime latest.createdAt hlastEq.symm
      have hnotlt : ¬ (createdAt < latest.createdAt) := Nat.not_lt.mpr hleTime
      have hprevle : prev.createdAt ≤ latest.createdAt := hmono0 (m + 1) h1lt (Nat.le_succ m)
      have hprevnotlt : ¬ (latest.createdAt < prev.createdAt) := Nat.not_lt.mpr hprevle
      have hid10 : latest.id ≠ prev.id := by
        have hp := List.pairwise_iff_getElem.mp hnodup
        have hne' := hp m (m + 1)
          (by rw [List.length_map, hlen]; omega) (by rw [List.length_map, hlen]; omega)
          (Nat.lt_succ_self m)
        simpa [latest, prev] using Ne.symm hne'
      have hid01 : prev.id ≠ latest.id := Ne.symm hid10
      have heq10 : erasePayload latest ≠ erasePayload prev := fun heq =>
        hid10 (by simpa [erasePayload] using congrArg Segment.id heq)
      have hv1 : validSingleToolFlush (m + 1) (expectedToolRun (m + 1) p1.length) = some p1.length := by
        simp [validSingleToolFlush, expectedToolRun, validToolContinuationRuns, hp1pos]
      have hv0 : validSingleToolFlush m (expectedToolRun m p0.length) = some p0.length := by
        cases m with
        | zero => simp [validSingleToolFlush, expectedToolRun, validToolContinuationRuns]
        | succ m' => simp [validSingleToolFlush, expectedToolRun, validToolContinuationRuns, hp0pos]
      have hf1 : latest.flush = some ⟨m + 1, expectedToolRun (m + 1) p1.length, p1⟩ := hf1
      have hbytes1 : latest.streamBytesAfter = some [b1 + p1.length] := hbytes1
      have hf0 : prev.flush = some ⟨m, expectedToolRun m p0.length, p0⟩ := hf0
      have hbytes0 : prev.streamBytesAfter = some [b0 + p0.length] := hbytes0
      have hsum : b0 + p0.length + p1.length = totalBytes := hb1eq.symm.trans htotBytes
      by_cases hm : m = 0
      · subst hm
        simp only [↓reduceIte] at htw
        have hmax2 : max (max 0 1) 0 = 1 := by omega
        have htw' : tailWindow records coordinate = [latest, prev] := htw
        simp [planToolAppendFromTail, hne, h.writerMatches, hcl, htw', sourceRecords,
          hcoord1, hcoord0, erasePayload, hopen1.2.1, hopen0.2.1, hopen1.1, hopen0.1,
          exactIdentityAt, hid10, hid01, hf1, hf0, htime1, htime0, heq10,
          validateCleanTail, maxOrdinalOf, atOrdinal, hmax2,
          hbytes1, hbytes0, hv1, hv0, hprevnotlt, hnotlt, htotBytes, hsum,
          Bind.bind, Except.bind]
      · have hm2lt : m - 1 < (sourceRecords records coordinate).length := by omega
        let prev2 := (sourceRecords records coordinate)[m - 1]'hm2lt
        have hcoord2 : prev2.coordinate = coordinate := by
          simpa using (List.mem_filter.mp (List.getElem_mem hm2lt)).2
        have hopen2 := h.allOpen prev2 (List.getElem_mem hm2lt)
        have htime2 : prev2.createdAt ≠ createdAt := hfreshTime prev2 (List.getElem_mem hm2lt)
        obtain ⟨p2, _, _, hf2, _, _, _, _, _, _⟩ := h.atIndex (m - 1) hm2lt
        have hf2 : prev2.flush = some ⟨m - 1, expectedToolRun (m - 1) p2.length, p2⟩ := hf2
        have hid12 : latest.id ≠ prev2.id := by
          have hp := List.pairwise_iff_getElem.mp hnodup
          have hne' := hp (m - 1) (m + 1)
            (by rw [List.length_map, hlen]; omega) (by rw [List.length_map, hlen]; omega) (by omega)
          simpa [latest, prev2] using Ne.symm hne'
        have hid02 : prev.id ≠ prev2.id := by
          have hp := List.pairwise_iff_getElem.mp hnodup
          have hne' := hp (m - 1) m
            (by rw [List.length_map, hlen]; omega) (by rw [List.length_map, hlen]; omega) (by omega)
          simpa [prev, prev2] using Ne.symm hne'
        have hid21 : prev2.id ≠ latest.id := Ne.symm hid12
        have hid20 : prev2.id ≠ prev.id := Ne.symm hid02
        have heq12 : erasePayload latest ≠ erasePayload prev2 := fun heq =>
          hid12 (by simpa [erasePayload] using congrArg Segment.id heq)
        have heq02 : erasePayload prev ≠ erasePayload prev2 := fun heq =>
          hid02 (by simpa [erasePayload] using congrArg Segment.id heq)
        simp only [hm, ↓reduceIte] at htw
        have htw' : tailWindow records coordinate = [latest, prev, prev2] := htw
        have hmax3 : max (m + 1) (m - 1) = m + 1 := by omega
        have hm1ne : m ≠ m + 1 := by omega
        have hm2ne1 : m - 1 ≠ m + 1 := by omega
        have hm2ne0 : m - 1 ≠ m := by omega
        simp [planToolAppendFromTail, hne, h.writerMatches, hcl, htw', sourceRecords,
          hcoord1, hcoord0, hcoord2, erasePayload, hopen1.2.1, hopen0.2.1, hopen2.2.1,
          hopen1.1, hopen0.1, hopen2.1, exactIdentityAt, hid10, hid01, hid12, hid21, hid02, hid20,
          hf1, hf0, hf2, htime1, htime0, htime2, heq10, heq12, heq02,
          validateCleanTail, maxOrdinalOf, atOrdinal, hmax3, hm1ne, hm2ne1, hm2ne0,
          hbytes1, hbytes0, hv1, hv0, hprevnotlt, hnotlt, htotBytes, hsum,
          Bind.bind, Except.bind]

private lemma consumeFlushesWithBytes_append (fs1 fs2 : List (Flush × Option (List Nat)))
    (s0 s1 s2 : Streams)
    (h1 : consumeFlushesWithBytes fs1 s0 = .ok s1)
    (h2 : consumeFlushesWithBytes fs2 s1 = .ok s2) :
    consumeFlushesWithBytes (fs1 ++ fs2) s0 = .ok s2 := by
  induction fs1 generalizing s0 with
  | nil =>
    cases h1
    exact h2
  | cons pair rest ih =>
    rcases pair with ⟨fl, sb⟩
    simp only [List.cons_append, consumeFlushesWithBytes] at h1 ⊢
    by_cases hempty : fl.runs.isEmpty = true
    · simp [hempty] at h1
    · simp only [hempty, ↓reduceIte] at h1 ⊢
      cases hr : consumeRuns fl.runs fl.payload s0 with
      | error e => simp [hr, Bind.bind, Except.bind] at h1
      | ok mid =>
        simp only [hr, Bind.bind, Except.bind] at h1 ⊢
        cases sb with
        | none => exact ih mid h1
        | some decl =>
          by_cases hneq : (mid.map (fun s => s.2.length) != decl) = true
          · simp [hneq] at h1
          · simp only [hneq, ↓reduceIte] at h1 ⊢
            exact ih mid h1

private lemma valid_prefix_consumeFlushes (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime)
    (erased : Bool) :
    let base := sourceRecords records coordinate
    let data := if erased then base.map erasePayload else base
    ∃ flushes streams,
      (List.range n).mapM (flushWithBytesAt data writer) = .ok flushes ∧
      consumeFlushesWithBytes flushes [] = .ok streams ∧
      streams.map (fun s => s.2.length) = (if n = 0 then [] else [totalBytes]) ∧
      streams.all (fun s => s.1.kind == .toolOutput || s.1.kind == .media) = true := by
  let base := sourceRecords records coordinate
  let data := if erased then base.map erasePayload else base
  have hlen : data.length = n := by
    cases erased <;> simp [data, base, h.exactLength]
  have hord : ∀ (i : Nat) (hi : i < data.length),
      data[i].flush.any (fun f => f.ordinal == i) = true := by
    intro i hi
    have hi' : i < base.length := by cases erased <;> simpa [data] using hi
    have ho := valid_prefix_ordinals records coordinate writer n totalBytes lastTime h i hi'
    cases erased
    · simpa [data, base] using ho
    · have hget : data[i]'hi = erasePayload (base[i]'hi') := by
        dsimp only [data]
        simp
      rw [hget]
      cases hf : (base[i]'hi').flush with
      | none => simp [base, hf] at ho
      | some f => simpa [base, erasePayload, hf] using ho
  have hstep : ∀ k (hk : k ≤ n),
      ∃ flushes streams,
        (List.range k).mapM (flushWithBytesAt data writer) = .ok flushes ∧
        consumeFlushesWithBytes flushes [] = .ok streams ∧
        (k = 0 → streams = []) ∧
        (∀ (hpos : k > 0),
          ∃ buf, streams = [(toolOutputDeclaration, buf)] ∧
            (base[k - 1]'(by rw [h.exactLength]; omega)).streamBytesAfter = some [buf.length]) := by
    intro k
    induction k with
    | zero =>
      intro _
      exact ⟨[], [], rfl, rfl, fun _ => rfl, fun hpos => by omega⟩
    | succ m ih =>
      intro hle
      obtain ⟨prevFlushes, prevStreams, hmapPrev, hconsPrev, hzeroPrev, hposPrev⟩ :=
        ih (Nat.le_of_succ_le hle)
      have hm_data : m < data.length := by omega
      have hm_base : m < base.length := by rw [h.exactLength]; omega
      have hfilt := filter_atIndex_unique data m hord hm_data
      obtain ⟨pm, bm, hpmPos, hfm, hbytesm, hutf8m, hbm0, _, _, _⟩ := h.atIndex m hm_base
      have hwm : data[m].writer = writer := by
        have hw := (h.allOpen base[m] (List.getElem_mem hm_base)).1
        cases erased
        · simpa [data, base] using hw
        · have hget : data[m]'hm_data = erasePayload (base[m]'hm_base) := by
            dsimp only [data]
            simp
          rw [hget]
          simpa [base, erasePayload] using hw
      have hpay : ∃ pay,
          data[m].flush = some ⟨m, expectedToolRun m pm.length, pay⟩ ∧
          pay.length = pm.length ∧
          (String.fromUTF8? (ByteArray.mk pay.toArray)).isSome = true ∧
          data[m].streamBytesAfter = some [bm + pm.length] := by
        cases erased
        · exact ⟨pm, by simpa [data, base] using hfm, rfl, hutf8m, by simpa [data, base] using hbytesm⟩
        · have hget : data[m]'hm_data = erasePayload (base[m]'hm_base) := by
            dsimp only [data]
            simp
          refine ⟨List.replicate pm.length 65, ?_, by simp, utf8_replicate_A pm.length, ?_⟩
          · rw [hget]; simp [base, erasePayload, hfm, expectedToolRun]
          · rw [hget]; simp [base, erasePayload, hbytesm]
      obtain ⟨pay, hfm_data, hpayLen, hpayUtf8, hbytes_data⟩ := hpay
      have hflush_m : flushWithBytesAt data writer m =
          .ok (⟨m, expectedToolRun m pm.length, pay⟩, some [bm + pm.length]) := by
        simp [flushWithBytesAt, hfilt, uniqueRecord_singleton, hwm, hfm_data, hbytes_data]
      have hrange : List.range (m + 1) = List.range m ++ [m] := List.range_succ m
      have hmap_next : (List.range (m + 1)).mapM (flushWithBytesAt data writer) =
          .ok (prevFlushes ++ [(⟨m, expectedToolRun m pm.length, pay⟩, some [bm + pm.length])]) := by
        rw [hrange, List.mapM_append, hmapPrev]
        simp [hflush_m, Bind.bind, Except.bind, Pure.pure, Except.pure]
      cases m with
      | zero =>
        have hnil := hzeroPrev rfl
        subst hnil
        have hbmz : bm = 0 := hbm0 rfl
        subst hbmz
        have hruns0 : consumeRuns (expectedToolRun 0 pm.length) pay [] =
            .ok [(toolOutputDeclaration, pay)] := by
          rw [← hpayLen]
          exact consumeRuns_expectedToolRun_zero pay hpayUtf8
        have hne_runs0 : (expectedToolRun 0 pm.length).isEmpty = false := rfl
        have hsingle : consumeFlushesWithBytes
            [(⟨0, expectedToolRun 0 pm.length, pay⟩, some [0 + pm.length])] [] =
            .ok [(toolOutputDeclaration, pay)] := by
          simp [consumeFlushesWithBytes, hne_runs0, hruns0, hpayLen, Bind.bind, Except.bind]
        refine ⟨prevFlushes ++ [(⟨0, expectedToolRun 0 pm.length, pay⟩, some [0 + pm.length])],
          [(toolOutputDeclaration, pay)], hmap_next,
          consumeFlushesWithBytes_append _ _ _ _ _ hconsPrev hsingle, by simp, ?_⟩
        intro _
        exact ⟨pay, rfl, by simpa [base, hpayLen] using hbytesm⟩
      | succ m' =>
        obtain ⟨prevBuf, hprevEq, hprevBytes⟩ := hposPrev (Nat.succ_pos m')
        subst hprevEq
        have hm'lt : m' < base.length := by omega
        obtain ⟨pm', bm', _, _, hbytesm', _, _, _, _, hnextm'⟩ := h.atIndex m' hm'lt
        obtain ⟨pm_next, hfm_next, hbytes_next⟩ := hnextm' hm_base
        have hpm_eq : pm_next = pm := by
          have := congrArg (Option.map Flush.payload) (hfm_next.symm.trans hfm)
          simpa using this
        subst pm_next
        have hprevLen : prevBuf.length = bm' + pm'.length := by
          have := hprevBytes.symm.trans hbytesm'
          simpa using this
        have hbm_sum : bm + pm.length = prevBuf.length + pay.length := by
          have := hbytesm.symm.trans hbytes_next
          have h1 : bm + pm.length = bm' + pm'.length + pm.length := by simpa using this
          omega
        have hruns_s : consumeRuns (expectedToolRun (m' + 1) pm.length) pay
            [(toolOutputDeclaration, prevBuf)] =
            .ok [(toolOutputDeclaration, prevBuf ++ pay)] := by
          rw [← hpayLen]
          exact consumeRuns_expectedToolRun_succ m' pay prevBuf (by omega) hpayUtf8
        have hne_runs_s : (expectedToolRun (m' + 1) pm.length).isEmpty = false := rfl
        have hsingle : consumeFlushesWithBytes
            [(⟨m' + 1, expectedToolRun (m' + 1) pm.length, pay⟩, some [bm + pm.length])]
            [(toolOutputDeclaration, prevBuf)] =
            .ok [(toolOutputDeclaration, prevBuf ++ pay)] := by
          simp [consumeFlushesWithBytes, hne_runs_s, hruns_s, hbm_sum,
            List.length_append, Bind.bind, Except.bind]
        refine ⟨prevFlushes ++ [(⟨m' + 1, expectedToolRun (m' + 1) pm.length, pay⟩,
            some [bm + pm.length])],
          [(toolOutputDeclaration, prevBuf ++ pay)], hmap_next,
          consumeFlushesWithBytes_append _ _ _ _ _ hconsPrev hsingle, by simp, ?_⟩
        intro _
        exact ⟨prevBuf ++ pay, rfl, by simpa [base, List.length_append, ← hbm_sum] using hbytesm⟩
  obtain ⟨flushes, streams, hmap, hcons, hzero, hpos⟩ := hstep n (Nat.le_refl n)
  refine ⟨flushes, streams, hmap, hcons, ?_, ?_⟩
  · cases n with
    | zero =>
      have := hzero rfl
      simp [this]
    | succ m =>
      obtain ⟨buf, rfl, hbuf⟩ := hpos (Nat.succ_pos m)
      have hmlt : m < base.length := by rw [h.exactLength]; omega
      obtain ⟨pm, bm, _, _, hbytesm, _, _, htotm, _, _⟩ := h.atIndex m hmlt
      obtain ⟨htotBytes, _⟩ := htotm rfl
      have hlen_eq : buf.length = totalBytes := by
        have := hbuf.symm.trans hbytesm
        have h1 : buf.length = bm + pm.length := by simpa using this
        exact h1.trans htotBytes
      simp [hlen_eq]
  · cases n with
    | zero =>
      have := hzero rfl
      simp [this]
    | succ m =>
      obtain ⟨buf, rfl, _⟩ := hpos (Nat.succ_pos m)
      simp [toolOutputDeclaration]

/-- Full `inspectOpenShape` on any `ValidToolPrefix` yields the exact `(n, [totalBytes], lastTime)`
expected by `planToolAppendFromTail`. -/
theorem valid_tool_prefix_inspectOpenShape
    (records : List Segment) (coordinate : Coordinate) (writer : Writer)
    (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime) :
    inspectOpenShape records coordinate writer =
      .ok ⟨n, if n = 0 then [] else [totalBytes], lastTime⟩ := by
  set base := sourceRecords records coordinate
  set selected := base.map erasePayload
  have hnodup_ids : (selected.map Segment.id).Nodup := by
    rw [map_id_erasePayload]
    exact valid_prefix_source_nodup_ids records coordinate writer n totalBytes lastTime h
  have hnodup : selected.Nodup := List.Nodup.of_map Segment.id hnodup_ids
  have hwriters : selected.all (fun r => r.writer == writer) = true := by
    rw [List.all_eq_true]
    intro r hr
    obtain ⟨orig, horig, rfl⟩ := List.mem_map.mp hr
    simpa [erasePayload] using (h.allOpen orig horig).1
  have hexact : selected.all (fun r => exactIdentityAt selected r) = true :=
    nodup_ids_exactIdentityAt selected hnodup_ids
  have hflushes_some : selected.all (fun r =>
      r.flush.isSome || (r.close.isSome && r.streamBytesAfter.isNone)) = true := by
    rw [List.all_eq_true]
    intro r hr
    obtain ⟨orig, horig, rfl⟩ := List.mem_map.mp hr
    have hfl := (h.allOpen orig horig).2.2
    cases hfo : orig.flush with
    | none => simp [hfo] at hfl
    | some _ => simp [erasePayload, hfo]
  have htimes : timestampsNondecreasing selected = true := by
    simp only [timestampsNondecreasing, List.all_eq_true]
    intro left hleft right hright
    obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hleft
    obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hright
    have hi' : i < base.length := by simpa [selected] using hi
    have hj' : j < base.length := by simpa [selected] using hj
    obtain ⟨pi, _, _, hfi, _, _, _, _, hmonoi, _⟩ := h.atIndex i hi'
    obtain ⟨pj, _, _, hfj, _, _, _, _, _, _⟩ := h.atIndex j hj'
    have hgi : selected[i]'hi = erasePayload (base[i]'hi') := List.getElem_map ..
    have hgj : selected[j]'hj = erasePayload (base[j]'hj') := List.getElem_map ..
    rw [hgi, hgj]
    simp only [base, erasePayload, hfi, hfj, Option.map_some]
    by_cases hle : i ≤ j
    · simp [hle, hmonoi j hj' hle]
    · simp [hle]
  have hfilter_eq : selected.filter (fun r => r.flush.isSome) = selected := by
    apply List.filter_eq_self.mpr
    intro r hr
    obtain ⟨orig, horig, rfl⟩ := List.mem_map.mp hr
    have hfl := (h.allOpen orig horig).2.2
    cases hfo : orig.flush with
    | none => simp [hfo] at hfl
    | some _ => simp [erasePayload, hfo]
  have hdedup : selected.dedup = selected := List.Nodup.dedup hnodup
  have hlen : selected.length = n := by simp [selected, base, h.exactLength]
  obtain ⟨flushes, streams, hmap, hcons, hbytes, _⟩ :=
    valid_prefix_consumeFlushes records coordinate writer n totalBytes lastTime h true
  have hmap' : (List.range n).mapM (flushWithBytesAt selected writer) = .ok flushes := by
    simpa [selected, base] using hmap
  have hlast : (if selected.isEmpty then none else some ((selected.map Segment.createdAt).foldl max 0)) =
      lastTime := by
    cases n with
    | zero =>
      have hnil : selected = [] := List.eq_nil_of_length_eq_zero hlen
      obtain ⟨_, hlt⟩ := h.emptyTotals rfl
      simp [hnil, hlt]
    | succ m =>
      have hnonempty : selected.isEmpty = false := by
        cases hs : selected with
        | nil => simp [hs] at hlen
        | cons _ _ => rfl
      have hmlt : m < base.length := by rw [h.exactLength]; omega
      obtain ⟨_, _, _, _, _, _, _, htotm, _, _⟩ := h.atIndex m hmlt
      obtain ⟨_, hlastm⟩ := htotm rfl
      have hfold_eq : (selected.map Segment.createdAt).foldl max 0 = base[m].createdAt := by
        apply Nat.le_antisymm
        · apply foldl_max_upper _ 0 _ (Nat.zero_le _)
          intro t ht
          obtain ⟨r, hr, rfl⟩ := List.mem_map.mp ht
          obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hr
          have hi' : i < base.length := by simpa [selected] using hi
          obtain ⟨_, _, _, _, _, _, _, _, hmonoi, _⟩ := h.atIndex i hi'
          have hgi : selected[i]'hi = erasePayload (base[i]'hi') := List.getElem_map ..
          rw [hgi]
          have hle : i ≤ m := by omega
          simpa [erasePayload] using hmonoi m hmlt hle
        · apply foldl_max_mem_le
          exact List.mem_map.mpr ⟨erasePayload base[m],
            List.mem_map.mpr ⟨base[m], List.getElem_mem hmlt, rfl⟩, rfl⟩
      simp only [hnonempty, Bool.false_eq_true, ↓reduceIte, hfold_eq]
      exact hlastm
  unfold inspectOpenShape
  simp only [h.writerMatches, Bool.not_true, Bool.false_eq_true, ↓reduceIte]
  show (if !(selected.all fun record => record.writer == writer) then Except.error ExtentError.invalidWriter
    else if !(selected.all fun record => exactIdentityAt selected record) then Except.error ExtentError.malformedRuns
    else if !(selected.all fun record =>
        record.flush.isSome || (record.close.isSome && record.streamBytesAfter.isNone)) then
      Except.error ExtentError.malformedRuns
    else if !timestampsNondecreasing selected then Except.error ExtentError.malformedRuns
    else do
      let data := (selected.filter fun record => record.flush.isSome).dedup
      let count := data.length
      let flushes ← (List.range count).mapM (flushWithBytesAt data writer)
      let streams ← consumeFlushesWithBytes flushes []
      let last := if data.isEmpty then none else some ((data.map Segment.createdAt).foldl max 0)
      pure (OpenShape.mk count (streams.map (fun stream => stream.2.length)) last)) =
    Except.ok (OpenShape.mk n (if n = 0 then [] else [totalBytes]) lastTime)
  simp only [hwriters, hexact, hflushes_some, htimes, Bool.not_true, Bool.false_eq_true, ↓reduceIte,
    hfilter_eq, hdedup, hlen, hmap', hcons, hbytes, hlast,
    Bind.bind, Except.bind, Pure.pure, Except.pure]

/-- Every `ValidToolPrefix` also satisfies `ToolDelivery.validOpenData`. -/
theorem valid_tool_prefix_validOpenData
    (records : List Segment) (coordinate : Coordinate) (writer : Writer)
    (n totalBytes : Nat) (lastTime : Option Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime) :
    ToolDelivery.validOpenData records coordinate writer = true := by
  set base := sourceRecords records coordinate
  have hnodup_all : records.all (exactIdentityAt records) = true :=
    nodup_ids_exactIdentityAt records h.nodupIds
  have hexact : base.all (exactIdentityAt records) = true := by
    rw [List.all_eq_true]
    intro r hr
    exact (List.all_eq_true.mp hnodup_all) r (List.mem_filter.mp hr).1
  have hflush_or : base.all (fun r => r.flush.isSome || r.streamBytesAfter.isNone) = true := by
    rw [List.all_eq_true]
    intro r hr
    simp [(h.allOpen r hr).2.2]
  have hfilter_eq : base.filter (fun r => r.flush.isSome) = base := by
    apply List.filter_eq_self.mpr
    intro r hr
    exact (h.allOpen r hr).2.2
  have hnodup_base : base.Nodup :=
    List.Nodup.of_map Segment.id (valid_prefix_source_nodup_ids records coordinate writer n totalBytes lastTime h)
  have hdedup : base.dedup = base := List.Nodup.dedup hnodup_base
  have htimes : timestampsNondecreasing base = true := by
    simp only [timestampsNondecreasing, List.all_eq_true]
    intro left hleft right hright
    obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hleft
    obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hright
    obtain ⟨pi, _, _, hfi, _, _, _, _, hmonoi, _⟩ := h.atIndex i hi
    obtain ⟨pj, _, _, hfj, _, _, _, _, _, _⟩ := h.atIndex j hj
    simp only [base, hfi, hfj]
    by_cases hle : i ≤ j
    · simp [hle, hmonoi j hj hle]
    · simp [hle]
  obtain ⟨flushes, streams, hmap, hcons, _, hkinds⟩ :=
    valid_prefix_consumeFlushes records coordinate writer n totalBytes lastTime h false
  have hmap' : (List.range n).mapM (flushWithBytesAt base writer) = .ok flushes := by
    simpa [base] using hmap
  have hlen_base : base.length = n := h.exactLength
  unfold ToolDelivery.validOpenData
  dsimp only
  simp only [show sourceRecords records coordinate = base from rfl,
    hexact, hflush_or, hfilter_eq, hdedup, htimes, hlen_base, hmap', hcons,
    Bool.true_and]
  exact hkinds

/-- Inductive step: appending the segment planned by `planToolAppendFromTail`
preserves `ValidToolPrefix` (and hence both `inspectOpenShape` agreement and
`ToolDelivery.validOpenData`). -/
theorem tool_append_preserves_valid_prefix
    (records : List Segment) (coordinate : Coordinate) (writer : Writer)
    (n totalBytes : Nat) (lastTime : Option Time)
    (id : DocId) (payload : List UInt8) (createdAt : Time)
    (h : ValidToolPrefix records coordinate writer n totalBytes lastTime)
    (hid : id ∉ records.map Segment.id)
    (hpos : payload.length > 0)
    (hutf8 : (String.fromUTF8? (ByteArray.mk payload.toArray)).isSome = true)
    (htime : ∀ t, lastTime = some t → t ≤ createdAt) :
    ValidToolPrefix
      (records ++ [toolAppendSegment id coordinate writer n totalBytes payload createdAt])
      coordinate writer (n + 1) (totalBytes + payload.length) (some createdAt) := by
  set seg := toolAppendSegment id coordinate writer n totalBytes payload createdAt
  have hcoord : seg.coordinate = coordinate := rfl
  have hsrc : sourceRecords (records ++ [seg]) coordinate =
      sourceRecords records coordinate ++ [seg] :=
    sourceRecords_append_same records coordinate seg hcoord
  have hlen_old : (sourceRecords records coordinate).length = n := h.exactLength
  have hlen_new : (sourceRecords (records ++ [seg]) coordinate).length = n + 1 := by
    simp [hsrc, hlen_old]
  refine
    { writerMatches := h.writerMatches
      exactLength := hlen_new
      nodupIds := by
        simp only [List.map_append, List.map_cons, List.map_nil]
        exact List.nodup_append.mpr ⟨h.nodupIds, List.nodup_singleton _, by
          intro a ha hb
          simp only [List.mem_singleton] at hb
          subst hb
          exact hid ha⟩
      allOpen := by
        intro r hr
        rw [hsrc, List.mem_append, List.mem_singleton] at hr
        rcases hr with hold | rfl
        · exact h.allOpen r hold
        · simp [seg, toolAppendSegment]
      emptyTotals := fun h0 => by omega
      atIndex := ?_ }
  intro i hi
  by_cases hilt : i < n
  · have hi_old : i < (sourceRecords records coordinate).length := by omega
    have hget_i : (sourceRecords (records ++ [seg]) coordinate)[i]'hi =
        (sourceRecords records coordinate)[i]'hi_old := by
      simp [hsrc, List.getElem_append_left hi_old]
    obtain ⟨pi, bi, hpipos, hfi, hbytesi, hutf8i, hbi0, htoti, hmonoi, hnexti⟩ :=
      h.atIndex i hi_old
    refine ⟨pi, bi, hpipos, by simpa [hget_i] using hfi, by simpa [hget_i] using hbytesi,
      hutf8i, hbi0, by intro hcontra; omega, ?_, ?_⟩
    · intro j hj hij
      rw [hget_i]
      by_cases hjlt : j < n
      · have hj_old : j < (sourceRecords records coordinate).length := by omega
        have hget_j : (sourceRecords (records ++ [seg]) coordinate)[j]'hj =
            (sourceRecords records coordinate)[j]'hj_old := by
          simp [hsrc, List.getElem_append_left hj_old]
        rw [hget_j]
        exact hmonoi j hj_old hij
      · have hjeq : j = n := by omega
        subst hjeq
        have hget_n : (sourceRecords (records ++ [seg]) coordinate)[j]'hj = seg := by
          have hle : (sourceRecords records coordinate).length ≤ j := by omega
          simp [hsrc, List.getElem_append_right hle, hlen_old]
        rw [hget_n]
        have hn1 : j - 1 < (sourceRecords records coordinate).length := by omega
        obtain ⟨_, _, _, _, _, _, _, htotLast, _, _⟩ := h.atIndex (j - 1) hn1
        obtain ⟨_, hlastEq⟩ := htotLast (by omega)
        have hleLast : ((sourceRecords records coordinate)[i]'hi_old).createdAt ≤
            ((sourceRecords records coordinate)[j - 1]'hn1).createdAt :=
          hmonoi (j - 1) hn1 (by omega)
        have hlastLeNew : ((sourceRecords records coordinate)[j - 1]'hn1).createdAt ≤ createdAt :=
          htime _ hlastEq.symm
        exact Nat.le_trans hleLast hlastLeNew
    · intro hnext
      by_cases hlt1 : i + 1 < n
      · have hnext_old : i + 1 < (sourceRecords records coordinate).length := by omega
        have hget_next : (sourceRecords (records ++ [seg]) coordinate)[i + 1]'hnext =
            (sourceRecords records coordinate)[i + 1]'hnext_old := by
          simp [hsrc, List.getElem_append_left hnext_old]
        rw [hget_next]
        exact hnexti hnext_old
      · have heq1 : i + 1 = n := by omega
        have hget_next : (sourceRecords (records ++ [seg]) coordinate)[i + 1]'hnext = seg := by
          have hle : (sourceRecords records coordinate).length ≤ i + 1 := by omega
          have hsub : i + 1 - (sourceRecords records coordinate).length = 0 := by omega
          simp [hsrc, List.getElem_append_right hle, hsub]
        rw [hget_next]
        obtain ⟨htot_i, _⟩ := htoti heq1
        refine ⟨payload, ?_, ?_⟩
        · simp [seg, toolAppendSegment, heq1]
        · simp [seg, toolAppendSegment, htot_i]
  · have hieq : i = n := by omega
    subst hieq
    have hget_n : (sourceRecords (records ++ [seg]) coordinate)[i]'hi = seg := by
      have hle : (sourceRecords records coordinate).length ≤ i := by omega
      have hsub : i - (sourceRecords records coordinate).length = 0 := by omega
      simp [hsrc, List.getElem_append_right hle, hsub]
    refine ⟨payload, totalBytes, hpos, ?_, ?_, hutf8,
      fun h0 => (h.emptyTotals h0).1,
      fun _ => ⟨rfl, ?_⟩, ?_, fun hcontra => by omega⟩
    · rw [hget_n]; rfl
    · rw [hget_n]; rfl
    · rw [hget_n]; rfl
    · intro j hj hnj
      have hjeq : j = i := by omega
      subst hjeq
      exact Nat.le_refl _

end CanonicalOutput
