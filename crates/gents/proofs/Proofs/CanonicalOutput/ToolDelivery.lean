import Proofs.CanonicalOutput.Message
import Proofs.ToolExecution.Executable
import Proofs.RequestExecutionLease.State

/-! Tool output retains its existing lifecycle authority. These transactions
do not admit a tool invocation: they operate only on its already-owned call.
The local mutation gate, ACP and genesis identity remain native boundaries. -/
namespace CanonicalOutput.ToolDelivery

structure World where
  /-- Physical request membership read from this exact tool-call document by
  its authenticated owner. ToolCallContext.requestId is the older abstract
  lifecycle label, not evidence that a logical ID grants document access. -/
  request : DocId
  session : SessionId
  parent : RequestExecutionLease.World Nat
  tool : ToolExecution.ToolCallContext
  /-- Exact physical tool document, owner-bound to the lifecycle context above.
  A provider/logical call ID never substitutes for this identity. -/
  toolDocument : DocId
  records : List Segment
  messages : List MessageEnvelope
  nextSequence : Nat
  delivered : Option DocId := none

inductive Error where
  | ownership
  | lifecycle
  | conflict
  | malformed
  | publication
  deriving DecidableEq, Repr

def coordinate (world : World) : Coordinate := ⟨world.request, .tool world.toolDocument⟩

def openSource (world : World) : Bool := (closures world.records (coordinate world)).isEmpty

def ownedRecord (world : World) (record : Segment) : Bool :=
  record.coordinate == coordinate world && record.writer == .tool world.toolDocument &&
    record.createdAt == world.tool.currentTime &&
    (sourceRecords world.records (coordinate world)).all (fun previous =>
      previous.createdAt ≤ record.createdAt)

def identityAvailable (world : World) (record : Segment) : Bool :=
  !(world.records.any fun old => old.id == record.id)

def sourceTimestampsValid (records : List Segment) : Bool :=
  records.all fun left => records.all fun right =>
    match left.flush, right.flush with
    | some a, some b => if a.ordinal ≤ b.ordinal then left.createdAt ≤ right.createdAt else true
    | _, _ => true

def validOpenData (records : List Segment) (source : Coordinate) (writer : Writer) : Bool :=
  let data := ((sourceRecords records source).filter (fun record => record.flush.isSome)).dedup
  sourceTimestampsValid data &&
  match (List.range data.length).mapM (flushAt data writer) with
  | .error _ => false
  | .ok flushes => match consumeFlushes flushes [] with
    | .error _ => false
    | .ok streams => streams.all fun stream => stream.1.kind == .toolOutput || stream.1.kind == .media

def closedRecordValid (world : World) (record : Segment) : Bool :=
  record.coordinate == coordinate world && record.writer == .tool world.toolDocument &&
    (world.records.filter (fun old => old.id == record.id)).all (fun old => old == record) &&
    (match record.close with
      | some (.closed _ count _) =>
          sourceTimestampsValid (extent world.records record.coordinate count) &&
            (extent world.records record.coordinate count).all
              (fun previous => previous.createdAt ≤ record.createdAt)
      | _ => false) &&
    match uniqueRecord Error.malformed .conflict (closures world.records (coordinate world)) with
    | .error _ => false
    | .ok only => only == record &&
        match reconstructExtent world.records record with
        | .error _ => false
        | .ok streams => streams.all fun stream =>
            stream.1.kind == .toolOutput || stream.1.kind == .media

def replayValid (world : World) (record : Segment) : Bool :=
  record.coordinate == coordinate world && record.writer == .tool world.toolDocument &&
    record.flush.isSome && record.close.isNone &&
    (world.records.filter (fun old => old.id == record.id)).all (fun old => old == record) &&
    match (closures world.records (coordinate world)).dedup with
    | [] => validOpenData world.records (coordinate world) (.tool world.toolDocument)
    | [closing] => closedRecordValid world closing
    | _ => false

def append (world : World) (record : Segment) : Except Error World :=
  if record ∈ world.records && replayValid world record then .ok world
  else if !identityAvailable world record then .error .conflict
  else if !ownedRecord world record then .error .ownership
  else if world.tool.state != .running || world.tool.deadlineExceeded ||
      !openSource world then .error .lifecycle
  else if record.flush.isNone || record.close.isSome ||
      !validOpenData (world.records ++ [record]) (coordinate world) (.tool world.toolDocument) then
    .error .malformed
  else .ok { world with records := world.records ++ [record] }

def exactExtent (records : List Segment) (source : Coordinate) (record : Segment) : Bool :=
  match record.close with
  | some (.closed _ count _) =>
      count == (((sourceRecords records source).filter
        (fun candidate => candidate.flush.isSome)).dedup).length
  | _ => false

/-- Closing bytes and the terminal tool transition are one commit. Every normal,
failed, cancelled, timed-out, or empty output closes before delivery. -/
def close (world : World) (action : ToolExecution.ToolCallContext.Action)
    (record : Segment) : Except Error World := do
  if record ∈ world.records ∧ isTerminal world.tool.state ∧ closedRecordValid world record then
    .ok world
  else if !identityAvailable world record then .error .conflict
  else if !ownedRecord world record then .error .ownership
  else if !openSource world then .error .conflict
  else
    let next ← match ToolExecution.ToolCallContext.step? world.tool action with
      | none => .error .lifecycle
      | some next => .ok next
    if !isTerminal next.state then .error .lifecycle
    else
      let records := world.records ++ [record]
      if !exactExtent records (coordinate world) record ||
          !sourceTimestampsValid (sourceRecords records (coordinate world)) then .error .malformed
      else match reconstructExtent records record with
      | .error _ => .error .malformed
      | .ok streams =>
          if !(streams.all fun stream => stream.1.kind == .toolOutput || stream.1.kind == .media)
          then .error .malformed
          else .ok { world with tool := next, records := records }

def sourceClosed (world : World) : Bool :=
  match uniqueRecord Error.malformed .conflict (closures world.records (coordinate world)) with
  | .error _ => false
  | .ok record => closedRecordValid world record

/-- Foreground results and complete background notification wrappers use the
same native reconstruction. Header completeness need not equal tool completeness.
Delivery is exactly selected once; no request lease or terminal output changes. -/
def publicationReplayValid (world : World) (message : MessageEnvelope) : Bool :=
  isTerminal world.tool.state && sourceClosed world &&
    message.header.publication == .toolDelivery world.toolDocument &&
    message.header.request == some world.request && message.header.session == world.session &&
    (world.messages.filter (fun old => old.header.id == message.header.id ||
      (old.header.session == message.header.session &&
        (old.key == message.key || old.sequence == message.sequence)))).all
          (fun old => old == message) &&
    match reconstructMessage world.records [] message with
    | .ok _ => true
    | .error _ => false

def publish (world : World) (message : MessageEnvelope) : Except Error World :=
  if world.delivered = some message.header.id ∧ message ∈ world.messages ∧
      publicationReplayValid world message then .ok world
  else if world.delivered.isSome then .error .conflict
  else if !isTerminal world.tool.state || !sourceClosed world then .error .lifecycle
  else if message.header.publication != .toolDelivery world.toolDocument ||
      message.header.request != some world.request || message.header.session != world.session ||
      message.createdAt != world.tool.currentTime ||
      message.sequence != world.nextSequence ||
      world.messages.any (fun old => old.header.id == message.header.id ||
        (old.header.session == message.header.session && old.key == message.key) ||
        (old.header.session == message.header.session && old.sequence == message.sequence)) then
    .error .publication
  else match reconstructMessage world.records [] message with
    | .error _ => .error .publication
    | .ok _ => .ok { world with
        messages := world.messages ++ [message]
        nextSequence := world.nextSequence + 1
        delivered := some message.header.id }

theorem append_preserves_parent (before after : World) (record : Segment)
    (h : append before record = .ok after) : after.parent = before.parent := by
  unfold append at h
  split at h <;> first | (cases h; rfl) | skip
  all_goals split at h <;> first | contradiction | skip
  all_goals split at h <;> first | contradiction | skip
  all_goals split at h <;> first | contradiction | skip
  all_goals split at h <;> first | contradiction | (cases h; rfl)

theorem publication_preserves_parent (before after : World) (message : MessageEnvelope)
    (h : publish before message = .ok after) : after.parent = before.parent := by
  unfold publish at h
  split at h
  · cases h; rfl
  · split at h
    · contradiction
    · split at h
      · contradiction
      · split at h
        · contradiction
        · cases hm : reconstructMessage before.records [] message with
          | error error => simp [hm] at h
          | ok native => simp [hm] at h; cases h; rfl

theorem publication_replay (world : World) (message : MessageEnvelope)
    (hdelivered : world.delivered = some message.header.id)
    (hmessage : message ∈ world.messages)
    (hvalid : publicationReplayValid world message = true) :
    publish world message = .ok world := by
  simp [publish, hdelivered, hmessage, hvalid]

theorem close_preserves_parent (before after : World)
    (action : ToolExecution.ToolCallContext.Action) (record : Segment)
    (h : close before action record = .ok after) : after.parent = before.parent := by
  unfold close at h
  split at h
  · cases h; rfl
  · split at h
    · contradiction
    · split at h
      · contradiction
      · split at h
        · contradiction
        · cases ht : ToolExecution.ToolCallContext.step? before.tool action with
          | none => simp [ht, Bind.bind, Except.bind] at h
          | some next =>
              simp only [ht, Bind.bind, Except.bind] at h
              split at h
              · contradiction
              · split at h
                · contradiction
                · cases he : reconstructExtent (before.records ++ [record]) record with
                  | error error => simp [he] at h
                  | ok streams =>
                      simp only [he] at h
                      split at h
                      · contradiction
                      · cases h; rfl

namespace Examples

def runningAfterParentTerminal : World :=
  { request := 10, session := 1
    parent := { RequestExecutionLease.initial Nat with
      request := .completed
      lease := .terminal 7 .completed }
    tool := { callId := 300, requestId := 10, state := .running, operation := .nativeCommand, deadline := 20, currentTime := 5, persistence := .committed, awaitMode := .background }
    toolDocument := 300
    records := [], messages := [], nextSequence := 3 }

def finalOutput : Segment :=
  { id := 400, coordinate := ⟨10, .tool 300⟩, writer := .tool 300
    flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .toolOutput }⟩], [65]⟩
    close := some (.closed .«partial» 1 [1]), createdAt := 5 }

def notification : MessageEnvelope :=
  { header := { id := 500, session := 1, request := some 10, origin := none, refs := [⟨400, 0⟩], outcome := .complete, role := .user, publication := .toolDelivery 300 }
    key := "tool-result-300", sequence := 3, nativeId := none, createdAt := 5
    blocks := [.text ⟨⟨400, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩] }

def failureThenNotification : Bool :=
  match close runningAfterParentTerminal (.cancelDuringRun .interrupted) finalOutput with
  | .error _ => false
  | .ok closed => match publish closed notification with
    | .error _ => false
    | .ok delivered =>
        delivered.parent.request == .completed &&
        delivered.parent.lease == .terminal 7 .completed &&
        delivered.messages == [notification] && delivered.delivered == some 500

example : failureThenNotification = true := by native_decide

def silentCancelled : Segment := { finalOutput with
  flush := none
  close := some (.closed .«partial» 0 []) }

def emptyOutputStillCloses : Bool :=
  match close runningAfterParentTerminal (.cancelDuringRun .interrupted) silentCancelled with
  | .ok closed => sourceClosed closed
  | .error _ => false

example : emptyOutputStillCloses = true := by native_decide

/-- Equal logical labels do not authorize another physical tool document. -/
example : (match close { runningAfterParentTerminal with toolDocument := 301 }
    (.cancelDuringRun .interrupted) finalOutput with
    | .error .ownership => true | _ => false) = true := by native_decide

example : (match close { runningAfterParentTerminal with toolDocument := 301 }
    (.cancelDuringRun .interrupted)
    { finalOutput with coordinate := ⟨10, .tool 301⟩, writer := .tool 301 } with
    | .ok _ => true | _ => false) = true := by native_decide

example : (match append
    { runningAfterParentTerminal with records := [finalOutput, { finalOutput with id := 401 }] }
    finalOutput with
    | .error .conflict => true | _ => false) = true := by native_decide

def regressedPrefix : List Segment :=
  [{ finalOutput with close := none },
   { finalOutput with
     id := 401, close := none, createdAt := 4
     flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩ }]

example : validOpenData regressedPrefix ⟨10, .tool 300⟩ (.tool 300) = false := by
  native_decide

example : (match close
    { runningAfterParentTerminal with
      records := regressedPrefix
      tool := { runningAfterParentTerminal.tool with currentTime := 6 } }
    (.cancelDuringRun .interrupted)
    { finalOutput with
      id := 402, flush := none, createdAt := 6
      close := some (.closed .partial 2 [2]) } with
    | .error .malformed => true | _ => false) = true := by native_decide

end Examples

end CanonicalOutput.ToolDelivery
