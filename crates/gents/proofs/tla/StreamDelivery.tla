---- MODULE StreamDelivery ----
(***************************************************************************)
(* Streamed provider text reaching a subscribed client while the provider  *)
(* pauses (#2131, #3803). The provider emits its chunks, then holds its     *)
(* completion behind a gate that only the client's observation opens, as   *)
(* the enrollment contract's fake provider does (`ChatAction::GatedSse`).  *)
(*                                                                          *)
(* Actors and their atomic steps, as the code has them:                    *)
(*  - Provider: emits each chunk into the inference stream, then waits on   *)
(*    the gate before it emits completion.                                  *)
(*  - Drive (`drive_inference_stream`, one task): at the top of every       *)
(*    iteration it asks the stream writer for `next_flush_deadline`         *)
(*    (drive.rs:85), then selects, biased, on the deadline before the next  *)
(*    stream item (drive.rs:154-188 before 189). A text item is recorded    *)
(*    and the writer's `pending_snapshot` decides whether it is due now:    *)
(*    the first visible bytes are, later bytes once the batch interval has  *)
(*    passed (streaming.rs:625-643, 841-860); a due verdict marks the       *)
(*    buffer flushed and the processor then appends the native partial    *)
(*    (stream_processor.rs:346-358). The deadline branch appends the same  *)
(*    way (`flush_pending`, stream_processor.rs:243-255). The decision is  *)
(*    the Lean owner `StreamBatch.plan`; `WriteDue` and `TimerArmed` below *)
(*    are its two fields.                                                   *)
(*  - Store: an append is one `AgentOutputSegment` commit through the       *)
(*    canonical append owner (`acknowledge_provider_flush`,                 *)
(*    streaming.rs:323-350); its commit notifies runtime subscribers.      *)
(*  - App store: the client app's observer publishes the claimed request    *)
(*    row (with its execution generation) at its own time.                  *)
(*  - Client reader (`wait_for_visible_content`): projects the app store's *)
(*    request row joined with the runtime transcript; while the projection *)
(*    lacks the content it waits for a change notification. Notifications  *)
(*    that arrive during a read stay queued.                                *)
(*  - Clock: the batch interval passes (`age` becomes "elapsed").          *)
(*                                                                          *)
(* Properties. `NoWithheldDuringPause`: the system never sits with a chunk  *)
(* received but not durable, the provider paused, the batch interval        *)
(* passed, and no flush enabled. `ChunksReachClient`: under weak fairness   *)
(* every chunk emitted before the pause reaches the client, which is the   *)
(* only way the gate opens, so the order the test asserts                   *)
(* (`ObservedBeforeCompletion`) follows. `NoSpuriousFailure`: the test     *)
(* never fails while the system is still able to deliver; a wall-clock     *)
(* budget (`Budget`) breaks it on every slow schedule because no fairness  *)
(* bounds the time any step takes.                                          *)
(*                                                                          *)
(* Each broken configuration removes one safeguard: `FlushTimer` (a buffer  *)
(* flushed only by the next chunk or completion), `AppendOnDue` (the        *)
(* processor ignores the writer's due verdict and flushes only the first    *)
(* visible bytes), `WakeOnAppStore` (the client waits only on runtime       *)
(* documents, the helper's lost wake-up before 8450ae6dd4) and `Budget`.   *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Chunks,         \* the chunks emitted before the pause, in order
  FlushTimer,     \* the drive waits on `next_flush_deadline`
  AppendOnDue,    \* a due write verdict is followed by the native append
  WakeOnAppStore, \* the client reader also wakes on the app store
  Budget          \* the test fails when a wall-clock budget elapses

Final == "final"
ChunkSet == {Chunks[i] : i \in 1..Len(Chunks)}

VARIABLES
  provider,  \* number of chunks emitted, or Len(Chunks)+1 once completion is emitted
  queue,     \* inference stream items produced and not yet taken by the drive
  drive,     \* "select" | "append" | "done"
  current,   \* chunks the writer has recorded
  persisted, \* chunks the writer's bookkeeping counts as flushed
  durable,   \* chunks in committed AgentOutputSegment rows
  age,       \* "fresh" | "elapsed": batch interval since the last flush
  terminal,  \* the runtime request is terminal
  appRow,    \* the app store holds the claimed request row
  appEvent,  \* an app store update the reader has not consumed
  rtEvent,   \* a runtime document notification the reader has not consumed
  client,    \* "read" | "wait" | "seen"
  seenSet,   \* chunks the client's projection has shown
  gate,      \* "closed" | "open"
  failed     \* the test reported a failure

vars == <<provider, queue, drive, current, persisted, durable, age, terminal,
          appRow, appEvent, rtEvent, client, seenSet, gate, failed>>

(* `StreamBatch.plan` for a visible-text buffer: `growth` is what the      *)
(* buffer gained since its last flush, `age` the batch clock.              *)
Growth == IF current = persisted THEN "none"
          ELSE IF persisted = {} THEN "firstVisible" ELSE "visible"
WriteDue(growth) ==
  CASE growth = "none" -> FALSE
    [] growth = "firstVisible" -> TRUE
    [] OTHER -> age = "elapsed"
TimerArmed == FlushTimer /\ Growth # "none"

TypeOK ==
  /\ provider \in 0..(Len(Chunks) + 1)
  /\ drive \in {"select", "append", "done"}
  /\ current \subseteq ChunkSet /\ persisted \subseteq current
  /\ durable \subseteq ChunkSet
  /\ age \in {"fresh", "elapsed"}
  /\ terminal \in BOOLEAN /\ appRow \in BOOLEAN /\ appEvent \in BOOLEAN
  /\ rtEvent \in BOOLEAN
  /\ client \in {"read", "wait", "seen"}
  /\ seenSet \subseteq ChunkSet
  /\ gate \in {"closed", "open"}
  /\ failed \in BOOLEAN

Init ==
  /\ provider = 0 /\ queue = <<>>
  /\ drive = "select"
  /\ current = {} /\ persisted = {} /\ durable = {}
  /\ age = "fresh"
  /\ terminal = FALSE
  /\ appRow = FALSE /\ appEvent = FALSE /\ rtEvent = FALSE
  /\ client = "read" /\ seenSet = {}
  /\ gate = "closed"
  /\ failed = FALSE

Paused == provider = Len(Chunks)

ProviderEmit ==
  /\ provider < Len(Chunks)
  /\ provider' = provider + 1
  /\ queue' = Append(queue, Chunks[provider + 1])
  /\ UNCHANGED <<drive, current, persisted, durable, age, terminal, appRow,
                 appEvent, rtEvent, client, seenSet, gate, failed>>

ProviderComplete ==
  /\ Paused /\ gate = "open"
  /\ provider' = provider + 1
  /\ queue' = Append(queue, Final)
  /\ UNCHANGED <<drive, current, persisted, durable, age, terminal, appRow,
                 appEvent, rtEvent, client, seenSet, gate, failed>>

DeadlineDue == TimerArmed /\ age = "elapsed"

(* drive.rs:154-188: the deadline branch wins over a ready stream item.    *)
DriveTimerFlush ==
  /\ drive = "select" /\ DeadlineDue
  /\ persisted' = current
  /\ age' = "fresh"
  /\ drive' = "append"
  /\ UNCHANGED <<provider, queue, current, durable, terminal, appRow,
                 appEvent, rtEvent, client, seenSet, gate, failed>>

DriveTakeChunk ==
  /\ drive = "select" /\ ~DeadlineDue
  /\ queue # <<>> /\ Head(queue) # Final
  /\ LET c == Head(queue)
         first == persisted = {}
         due == WriteDue(IF first THEN "firstVisible" ELSE "visible")
     IN /\ queue' = Tail(queue)
        /\ current' = current \cup {c}
        /\ IF due
             THEN /\ persisted' = current \cup {c}
                  /\ age' = "fresh"
                  /\ drive' = IF AppendOnDue \/ first THEN "append" ELSE "select"
             ELSE UNCHANGED <<persisted, age, drive>>
  /\ UNCHANGED <<provider, durable, terminal, appRow, appEvent, rtEvent,
                 client, seenSet, gate, failed>>

(* The native append commits everything the writer recorded.               *)
DriveAppend ==
  /\ drive = "append"
  /\ durable' = current
  /\ rtEvent' = TRUE
  /\ drive' = "select"
  /\ UNCHANGED <<provider, queue, current, persisted, age, terminal, appRow,
                 appEvent, client, seenSet, gate, failed>>

(* Completion publishes the turn and terminalizes the request.             *)
DriveComplete ==
  /\ drive = "select" /\ ~DeadlineDue
  /\ queue # <<>> /\ Head(queue) = Final
  /\ queue' = Tail(queue)
  /\ persisted' = current /\ durable' = current
  /\ terminal' = TRUE /\ rtEvent' = TRUE
  /\ drive' = "done"
  /\ UNCHANGED <<provider, current, age, appRow, appEvent, client, seenSet,
                 gate, failed>>

Tick ==
  /\ age = "fresh" /\ drive # "done"
  /\ age' = "elapsed"
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, terminal,
                 appRow, appEvent, rtEvent, client, seenSet, gate, failed>>

AppPublish ==
  /\ ~appRow
  /\ appRow' = TRUE /\ appEvent' = TRUE
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, rtEvent, client, seenSet, gate, failed>>

(* One projection read; the store watch is borrowed before it.             *)
ClientRead ==
  /\ client = "read"
  /\ LET shown == IF appRow THEN durable ELSE {}
     IN /\ seenSet' = shown
        /\ client' = IF ChunkSet \subseteq shown THEN "seen" ELSE "wait"
  /\ appEvent' = IF WakeOnAppStore THEN FALSE ELSE appEvent
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, appRow, rtEvent, gate, failed>>

ClientWakeRuntime ==
  /\ client = "wait" /\ rtEvent
  /\ rtEvent' = FALSE /\ client' = "read"
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, appRow, appEvent, seenSet, gate, failed>>

ClientWakeAppStore ==
  /\ WakeOnAppStore /\ client = "wait" /\ appEvent
  /\ appEvent' = FALSE /\ client' = "read"
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, appRow, rtEvent, seenSet, gate, failed>>

(* The test releases the provider only after the client saw the content.   *)
OpenGate ==
  /\ client = "seen" /\ gate = "closed"
  /\ gate' = "open"
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, appRow, appEvent, rtEvent, client, seenSet, failed>>

(* A wall-clock budget can elapse after any schedule: nothing bounds how   *)
(* long a loaded host takes for the steps above.                           *)
BudgetElapses ==
  /\ Budget /\ provider > 0 /\ client # "seen" /\ ~failed
  /\ failed' = TRUE
  /\ UNCHANGED <<provider, queue, drive, current, persisted, durable, age,
                 terminal, appRow, appEvent, rtEvent, client, seenSet, gate>>

Next ==
  \/ ProviderEmit \/ ProviderComplete
  \/ DriveTimerFlush \/ DriveTakeChunk \/ DriveAppend \/ DriveComplete
  \/ Tick \/ AppPublish
  \/ ClientRead \/ ClientWakeRuntime \/ ClientWakeAppStore
  \/ OpenGate \/ BudgetElapses

Fairness ==
  /\ WF_vars(ProviderEmit) /\ WF_vars(ProviderComplete)
  /\ WF_vars(DriveTimerFlush) /\ WF_vars(DriveTakeChunk)
  /\ WF_vars(DriveAppend) /\ WF_vars(DriveComplete)
  /\ WF_vars(Tick) /\ WF_vars(AppPublish)
  /\ WF_vars(ClientRead) /\ WF_vars(ClientWakeRuntime)
  /\ WF_vars(ClientWakeAppStore) /\ WF_vars(OpenGate)

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Safety form: a received chunk is never stranded during the pause.       *)
NoWithheldDuringPause ==
  ~ ( /\ Paused /\ queue = <<>> /\ drive = "select"
      /\ ~(current \subseteq durable)
      /\ age = "elapsed"
      /\ ~ENABLED DriveTimerFlush )

(* The order the test asserts: the client saw every pre-pause chunk while  *)
(* the request was still open.                                              *)
ObservedBeforeCompletion == terminal => client = "seen"

NoSpuriousFailure == ~failed

(* Liveness: every chunk emitted before the pause reaches the client.      *)
ChunksReachClient == Paused ~> (ChunkSet \subseteq seenSet)

Completes == <>terminal
====
