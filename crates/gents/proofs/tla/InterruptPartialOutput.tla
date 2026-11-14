---- MODULE InterruptPartialOutput ----
(***************************************************************************)
(* Text a provider has already handed to the owned loop when its request   *)
(* is interrupted (#3820). One provider attempt streams `Chunks` text      *)
(* chunks to the daemon's inference drive; a requester may latch an        *)
(* interrupt at any moment; the drive then drops the loop stream, drains   *)
(* what it holds and the request's terminal owner records the outcome.     *)
(*                                                                          *)
(* Actors and their atomic steps, as the code has them:                    *)
(*  - The provider work. The drive's stream branch polls the loop stream   *)
(*    (`drive.rs` `drive_inference_stream`, stream branch); the admission   *)
(*    driver runs the owned loop's `ProviderWork` (`loop_stream.rs`         *)
(*    `drive_admissions`), whose provider poll returns the chunk and sends *)
(*    it through a oneshot reply (`loop_stream.rs`, provider poll handoff). *)
(*    `Receive` is that step.                                              *)
(*  - The handoff. The owned generator then reads that reply and yields     *)
(*    `LoopStreamItem::Text`. Reading the reply with `.await` is a          *)
(*    suspension point: a tokio oneshot returns `Pending` when the task's  *)
(*    cooperative budget is spent even if the value is present, so the     *)
(*    chunk can sit in the reply while the drive is re-polled. `Yield` is   *)
(*    that step; with `AtomicHandoff` the reply is read synchronously       *)
(*    (`try_recv`: the driver resumes the generator only after the work     *)
(*    finished) and `Receive` yields in the same step.                     *)
(*  - The stream processor (`stream_processor.rs` `process_item`), awaited *)
(*    outside the drive's select and so never cancelled: first visible     *)
(*    text is flushed at once as an `AgentOutputSegment`, later text waits *)
(*    for the batch deadline (`BatchFlush`, the drive's flush branch).     *)
(*  - The interrupt latch (`interrupt.rs` `interrupt_request_matching_in_txn`) *)
(*    and its observer (`observe_request_interrupt`), which signals the     *)
(*    drive's watch.                                                       *)
(*  - The drive's interrupt branch, biased before the stream branch: it    *)
(*    drops the loop stream (and whatever the handoff holds) and drains    *)
(*    (`drain_for_interrupt` -> `persist_received_partial_turn` ->          *)
(*    `persist_partial_turn`: flush the accumulator, close the attempt      *)
(*    `Partial`).                                                          *)
(*  - The terminal owner (`terminal.rs` `terminalize_request`): one CAS     *)
(*    selecting the published partial message, if any.                     *)
(* A provider end of stream reaches the same drain through the processor   *)
(* and fails the request; it is modeled so the property covers both ends.  *)
(*                                                                          *)
(* Property: when the request reaches a terminal state, every chunk the    *)
(* provider handed to the loop is durable, and the terminal state is        *)
(* written once. The receipt is the boundary the product owns: bytes still *)
(* in the provider's transport were never received and are not promised.   *)
(*                                                                          *)
(* Configurations: `MCInterruptPartialOutput` (green, atomic handoff),     *)
(* `MCInterruptPartialOutputSuspendedHandoff` (the code before #3820's fix: *)
(* the interrupt drops a received chunk), `MCInterruptPartialOutputNoDrainFlush` *)
(* (the drain closes without flushing the accumulator).                    *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Chunks,         \* text chunks the provider streams in this attempt
  AtomicHandoff,  \* the owned loop reads a received provider item without suspending
  DrainFlushes    \* the interrupt drain flushes the accumulator before closing

VARIABLES
  emitted,        \* chunks the provider has handed to the loop's provider work
  slot,           \* a received chunk held in the provider-work reply, not yet yielded
  acc,            \* chunks in the stream processor's assistant-turn accumulator
  durable,        \* chunks covered by committed output segments
  drive,          \* "select" | "processing" | "draining" | "drained"
  cause,          \* why the drive left its select: "none" | "interrupt" | "eof"
  latched,        \* interrupt_requested_at is committed
  observed,       \* the observer has signalled the drive's interrupt watch
  lifecycle,      \* "processing" | "interrupted" | "failed"
  terminalWrites  \* terminal CASes won

vars == <<emitted, slot, acc, durable, drive, cause, latched, observed,
          lifecycle, terminalWrites>>

TypeOK ==
  /\ emitted \in 0..Chunks
  /\ slot \in 0..1
  /\ acc \in 0..Chunks
  /\ durable \in 0..Chunks
  /\ drive \in {"select", "processing", "draining", "drained"}
  /\ cause \in {"none", "interrupt", "eof"}
  /\ latched \in BOOLEAN
  /\ observed \in BOOLEAN
  /\ lifecycle \in {"processing", "interrupted", "failed"}
  /\ terminalWrites \in 0..2

Init ==
  /\ emitted = 0
  /\ slot = 0
  /\ acc = 0
  /\ durable = 0
  /\ drive = "select"
  /\ cause = "none"
  /\ latched = FALSE
  /\ observed = FALSE
  /\ lifecycle = "processing"
  /\ terminalWrites = 0

Streaming == drive = "select" /\ cause = "none"

(* The provider work polls the provider and receives one chunk. The biased *)
(* select reaches the stream branch only while the interrupt watch is quiet. *)
Receive ==
  /\ Streaming /\ ~observed
  /\ slot = 0 /\ emitted < Chunks
  /\ emitted' = emitted + 1
  /\ IF AtomicHandoff
       THEN /\ acc' = acc + 1
            /\ drive' = "processing"
            /\ UNCHANGED slot
       ELSE /\ slot' = 1
            /\ UNCHANGED <<acc, drive>>
  /\ UNCHANGED <<durable, cause, latched, observed, lifecycle, terminalWrites>>

(* The generator reads the reply and yields the chunk to the drive. *)
Yield ==
  /\ ~AtomicHandoff
  /\ Streaming /\ ~observed
  /\ slot = 1
  /\ slot' = 0
  /\ acc' = acc + 1
  /\ drive' = "processing"
  /\ UNCHANGED <<emitted, durable, cause, latched, observed, lifecycle, terminalWrites>>

(* The processor takes the chunk; first visible text is flushed at once. *)
Process ==
  /\ drive = "processing"
  /\ durable' = IF durable = 0 THEN acc ELSE durable
  /\ drive' = "select"
  /\ UNCHANGED <<emitted, slot, acc, cause, latched, observed, lifecycle, terminalWrites>>

(* The flush-deadline branch commits batched text. *)
BatchFlush ==
  /\ Streaming /\ ~observed
  /\ durable < acc
  /\ durable' = acc
  /\ UNCHANGED <<emitted, slot, acc, drive, cause, latched, observed, lifecycle, terminalWrites>>

(* The provider ends its stream; the drive leaves through the processor's drain. *)
EndOfStream ==
  /\ Streaming /\ ~observed
  /\ slot = 0 /\ emitted = Chunks
  /\ drive' = "draining"
  /\ cause' = "eof"
  /\ UNCHANGED <<emitted, slot, acc, durable, latched, observed, lifecycle, terminalWrites>>

(* A requester latches the interrupt. *)
Latch ==
  /\ ~latched /\ lifecycle = "processing"
  /\ latched' = TRUE
  /\ UNCHANGED <<emitted, slot, acc, durable, drive, cause, observed, lifecycle, terminalWrites>>

(* The observer reads the latch and signals the drive. *)
Observe ==
  /\ latched /\ ~observed
  /\ observed' = TRUE
  /\ UNCHANGED <<emitted, slot, acc, durable, drive, cause, latched, lifecycle, terminalWrites>>

(* The interrupt branch drops the loop stream with whatever the reply holds. *)
InterruptBranch ==
  /\ Streaming /\ observed
  /\ slot' = 0
  /\ drive' = "draining"
  /\ cause' = "interrupt"
  /\ UNCHANGED <<emitted, acc, durable, latched, observed, lifecycle, terminalWrites>>

(* `persist_partial_turn`: flush the accumulator, close the attempt. *)
Drain ==
  /\ drive = "draining"
  /\ durable' = IF DrainFlushes THEN acc ELSE durable
  /\ drive' = "drained"
  /\ UNCHANGED <<emitted, slot, acc, cause, latched, observed, lifecycle, terminalWrites>>

(* The terminal owner's CAS. *)
Terminalize ==
  /\ drive = "drained" /\ lifecycle = "processing"
  /\ lifecycle' = IF cause = "interrupt" THEN "interrupted" ELSE "failed"
  /\ terminalWrites' = terminalWrites + 1
  /\ UNCHANGED <<emitted, slot, acc, durable, drive, cause, latched, observed>>

Next ==
  \/ Receive \/ Yield \/ Process \/ BatchFlush \/ EndOfStream
  \/ Latch \/ Observe \/ InterruptBranch \/ Drain \/ Terminalize

Fairness ==
  /\ WF_vars(Receive) /\ WF_vars(Yield) /\ WF_vars(Process)
  /\ WF_vars(EndOfStream) /\ WF_vars(Observe) /\ WF_vars(InterruptBranch)
  /\ WF_vars(Drain) /\ WF_vars(Terminalize)

Spec == Init /\ [][Next]_vars /\ Fairness

Terminal == lifecycle # "processing"

(* Every chunk the provider handed to the loop is durable at the terminal state. *)
NoReceivedOutputDropped == Terminal => durable = emitted

TerminalOnce == terminalWrites <= 1

(* A latched interrupt ends the request. *)
InterruptSettles == latched ~> Terminal

(* Non-vacuity: an interrupt that lands after output was received ends the *)
(* request with that output durable. TLC reports this invariant violated;  *)
(* the trace is the witness.                                               *)
NoInterruptAfterOutput == ~(lifecycle = "interrupted" /\ durable > 0)

====
