import Proofs.CanonicalOutput.Message
import Proofs.ToolExecution.Executable

/-!
# Tool-owned canonical sources

This module validates one physical tool-owned source. It deliberately owns no
message collection, transcript cursor, request lease, or independent delivery
flag. The composed transaction in `CanonicalOutput.Execution.ToolDelivery`
applies these validators to the single shared execution world.
-/
namespace CanonicalOutput.ToolDelivery

inductive Error where
  | ownership
  | lifecycle
  | conflict
  | malformed
  | publication
  deriving DecidableEq, Repr

def coordinate (request document : DocId) : Coordinate :=
  ⟨request, .tool document⟩

def openSource (records : List Segment) (request document : DocId) : Bool :=
  (closures records (coordinate request document)).isEmpty

def identityAvailable (records : List Segment) (record : Segment) : Bool :=
  !(records.any fun old => old.id == record.id)

def exactIdentityAt (records : List Segment) (record : Segment) : Bool :=
  (records.filter (fun old => old.id == record.id)).all (fun old => old == record)

def ownedRecord (records : List Segment) (request document : DocId)
    (currentTime : Time) (record : Segment) : Bool :=
  record.coordinate == coordinate request document &&
    record.writer == .tool document && record.createdAt == currentTime &&
    (sourceRecords records (coordinate request document)).all
      (fun previous => previous.createdAt ≤ record.createdAt)

def sourceTimestampsValid (records : List Segment) : Bool :=
  records.all fun left => records.all fun right =>
    match left.flush, right.flush with
    | some a, some b => if a.ordinal ≤ b.ordinal then left.createdAt ≤ right.createdAt else true
    | _, _ => true

def validOpenData (records : List Segment) (source : Coordinate) (writer : Writer) : Bool :=
  let sourceRecords := sourceRecords records source
  let data := sourceRecords.filter (fun record => record.flush.isSome) |>.dedup
  sourceRecords.all (exactIdentityAt records) &&
    sourceTimestampsValid data &&
    match (List.range data.length).mapM (flushAt data writer) with
    | .error _ => false
    | .ok flushes => match consumeFlushes flushes [] with
      | .error _ => false
      | .ok streams => streams.all fun stream =>
          stream.1.kind == .toolOutput || stream.1.kind == .media

def exactExtent (records : List Segment) (source : Coordinate) (record : Segment) : Bool :=
  match record.close with
  | some (.closed _ count _) =>
      count == (((sourceRecords records source).filter
        (fun candidate => candidate.flush.isSome)).dedup).length
  | _ => false

def closedRecordValid (records : List Segment) (request document : DocId)
    (record : Segment) : Bool :=
  let source := coordinate request document
  record.coordinate == source && record.writer == .tool document &&
    exactIdentityAt records record &&
    (match record.close with
      | some (.closed _ count _) =>
          let selected := extent records source count
          selected.all (exactIdentityAt records) &&
            validOpenData selected source (.tool document) &&
            sourceTimestampsValid selected &&
            selected.all (fun previous => previous.createdAt ≤ record.createdAt)
      | _ => false) &&
    match uniqueRecord Error.malformed .conflict (closures records source) with
    | .error _ => false
    | .ok only => only == record &&
        match reconstructExtent records record with
        | .error _ => false
        | .ok streams => streams.all fun stream =>
            stream.1.kind == .toolOutput || stream.1.kind == .media

/-- A new closure fixes all tool bytes visible at its gate. Once committed,
`closedRecordValid` intentionally validates only that selected extent so later
beyond-extent delivery remains inert. -/
def freshClosedRecordValid (records : List Segment) (request document : DocId)
    (record : Segment) : Bool :=
  exactExtent records (coordinate request document) record &&
    validOpenData records (coordinate request document) (.tool document) &&
    closedRecordValid records request document record

def appendReplayValid (records : List Segment) (request document : DocId)
    (record : Segment) : Bool :=
  record.coordinate == coordinate request document && record.writer == .tool document &&
    record.flush.isSome && record.close.isNone && exactIdentityAt records record &&
    match (closures records (coordinate request document)).dedup with
    | [] => validOpenData records (coordinate request document) (.tool document)
    | [closing] => closedRecordValid records request document closing
    | _ => false

/-- Add one running tool flush to the authoritative segment collection. Output
does not alter either request or tool liveness. -/
def appendRecords (records : List Segment) (request document : DocId)
    (currentTime : Time) (record : Segment) : Except Error (List Segment) :=
  if record ∈ records && appendReplayValid records request document record then .ok records
  else if !identityAvailable records record then .error .conflict
  else if !ownedRecord records request document currentTime record then .error .ownership
  else if !openSource records request document then .error .lifecycle
  else if record.flush.isNone || record.close.isSome ||
      !validOpenData (records ++ [record]) (coordinate request document) (.tool document) then
    .error .malformed
  else .ok (records ++ [record])

/-- Install the exact terminal closure. Tool status is intentionally not
derived from `OutputOutcome`: lifecycle completion and output completeness are
separate axes. -/
def closeRecords (records : List Segment) (request document : DocId)
    (currentTime : Time) (record : Segment) : Except Error (List Segment) :=
  if record ∈ records && closedRecordValid records request document record then .ok records
  else if !identityAvailable records record then .error .conflict
  else if !ownedRecord records request document currentTime record then .error .ownership
  else if !openSource records request document then .error .conflict
  else
    let next := records ++ [record]
    if !freshClosedRecordValid next request document record then .error .malformed
    else .ok next

def sourceClosed (records : List Segment) (request document : DocId) : Bool :=
  match uniqueRecord Error.malformed .conflict
      (closures records (coordinate request document)) with
  | .error _ => false
  | .ok record => closedRecordValid records request document record

def terminalState : ToolExecution.ToolCallState → Bool
  | .completed | .failed | .timedOut | .cancelled => true
  | .pending | .running => false

namespace Examples

def first : Segment :=
  { id := 400, coordinate := coordinate 10 300, writer := .tool 300
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .toolOutput }⟩], [65]⟩
    close := none, createdAt := 5 }

def closing : Segment :=
  { id := 401, coordinate := coordinate 10 300, writer := .tool 300
    flush := none, close := some (.closed .partial 1 [1]), createdAt := 5 }

def regressed : Segment :=
  { id := 402, coordinate := coordinate 10 300, writer := .tool 300
    flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩
    close := none, createdAt := 4 }

example : validOpenData [first, regressed] (coordinate 10 300) (.tool 300) = false := by
  native_decide

def closingTwin : Segment := { closing with createdAt := 6 }

example : closedRecordValid [first, closing, closingTwin] 10 300 closing = false := by
  native_decide

/-- A flush delivered after the selected closure extent cannot rewind or
invalidate replay of that already-committed extent. -/
def laterBeyondExtent : Segment :=
  { id := 403, coordinate := coordinate 10 300, writer := .tool 300
    flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩
    close := none, createdAt := 6 }

example : closedRecordValid [first, closing, laterBeyondExtent] 10 300 closing = true := by
  native_decide

end Examples

end CanonicalOutput.ToolDelivery
