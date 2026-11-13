import Proofs.Session.CanonicalOutput.Reconstruction

namespace CanonicalOutput

/-- A shape read relies on the immutable row's writer having validated its own
UTF-8 run partition. Erasing payload preserves every field used to compare
records, except bytes that must be read again for a physical replay. -/
def erasePayload (record : Segment) : Segment :=
  { record with flush := record.flush.map fun flush =>
      { flush with payload := List.replicate (flush.runs.map Run.bytes).sum 65 } }

structure OpenShape where
  segments : Nat
  streamBytes : List Nat
  lastCreatedAt : Option Time
  deriving DecidableEq, Repr

def inspectOpenShape (records : List Segment) (coordinate : Coordinate) (writer : Writer) :
    Except ExtentError OpenShape := do
  if !writerMatchesSource coordinate writer then .error .invalidWriter
  else
    let selected := (sourceRecords records coordinate).map erasePayload
    if !(selected.all fun record => record.writer == writer) then .error .invalidWriter
    else if !(selected.all fun record => exactIdentityAt selected record) then
      .error .malformedRuns
    else if !(selected.all fun record => record.flush.isSome || record.close.isSome) then
      .error .malformedRuns
    else if !timestampsNondecreasing selected then .error .malformedRuns
    else
      let data := (selected.filter fun record => record.flush.isSome).dedup
      let count := data.length
      let flushes ← (List.range count).mapM (flushAt data writer)
      let streams ← consumeFlushes flushes []
      let last := if data.isEmpty then none else some ((data.map Segment.createdAt).foldl max 0)
      pure ⟨count, streams.map (fun stream => stream.2.length), last⟩

theorem erasePayload_idempotent (record : Segment) :
    erasePayload (erasePayload record) = erasePayload record := by
  cases record with
  | mk id coordinate writer flush close createdAt =>
    cases flush <;> simp [erasePayload]

/-- A prefix summary is independent of payload text. The full-record replay
comparison remains a different operation and must retain those bytes. -/
theorem shape_summary_ignores_payload (records : List Segment)
    (coordinate : Coordinate) (writer : Writer) :
    inspectOpenShape (records.map erasePayload) coordinate writer =
      inspectOpenShape records coordinate writer := by
  have selected : (sourceRecords (records.map erasePayload) coordinate).map erasePayload =
      (sourceRecords records coordinate).map erasePayload := by
    induction records with
    | nil => rfl
    | cons record rest ih =>
      have coordinate_eq : (erasePayload record).coordinate = record.coordinate := rfl
      simp only [sourceRecords, List.map_cons, List.filter_cons] at ih ⊢
      rw [coordinate_eq]
      by_cases same : record.coordinate == coordinate
      · simp [same, erasePayload_idempotent, ih]
      · simp [same, ih]
  simp only [inspectOpenShape, selected]

/-- Full inspection can use the shape summary only after every immutable row's
payload has been validated; the native adapter also validates RFC3339 times. -/
def inspectValidatedOpenShape (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) (payloadsValid : Bool) : Except ExtentError OpenShape :=
  if payloadsValid then inspectOpenShape records coordinate writer else .error .malformedRuns

theorem validated_prefix_agrees (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) :
    inspectValidatedOpenShape records coordinate writer true =
      inspectOpenShape records coordinate writer := rfl

end CanonicalOutput
