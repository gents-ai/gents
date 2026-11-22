---- MODULE DroppedPermitCleanup ----
\* lean: none
\* crates: gents, gents-inference, gents-store
\* models: InferenceCall.DroppedPermit.outcome
(***************************************************************************)
(* The InferenceCall row of one admitted provider call, from the row the   *)
(* admission controller persists to its terminal, and what a reader may    *)
(* await before it reads that row (#3659).                                 *)
(*                                                                          *)
(* A call's row is created `running` (`persist_call_started`) or `queued`  *)
(* (`persist_call_queued`) and then moved to `running`                     *)
(* (`persist_existing_call_running`). The holder ends the call explicitly: *)
(* `AdmissionPermit::finish` writes the terminal, and a stream's terminal  *)
(* is finished by a task of its own (`finish_guard`) that owns the permit  *)
(* and is aborted, not joined, when the stream is dropped. A permit or a   *)
(* queued guard that is dropped instead (a cancelled or aborted task, a    *)
(* finish cancelled mid-write) cannot await in `Drop`: it hands the        *)
(* terminal write to `DropCleanup::spawn`, a detached task on the runtime  *)
(* the guard was created on. `CleanupTracker` counts a finalizer from the  *)
(* moment it takes the permit, and a cleanup from the `Drop` that spawns   *)
(* it, until each finishes or the runtime discards it;                     *)
(* `AdmissionRegistry::drop_cleanup_idle` resolves when the count is       *)
(* zero. Every terminal install is a compare-and-set on a live row         *)
(* (`persist_existing_call_terminal`, Lean `InferenceCall.Persistence`), so *)
(* a cleanup that finds the row already terminal installs nothing.         *)
(*                                                                          *)
(* The runtime stops gracefully (`shutdown_runtime`: cancel, join the      *)
(* tasks that wait for or hold a permit, flush, exit) or abruptly (a       *)
(* crash, a kill, or the teardown deadline cutting the joins or the flush, *)
(* which has no deadline of its own). A runtime that exits discards the    *)
(* cleanup and the finalizers still in flight. Recovery                     *)
(* (`InferenceCall::recover_all_with_identities`, at the next start and in *)
(* the periodic sweep) terminalizes a live row whose request has ended,    *)
(* with its own reason and no usage.                                       *)
(*                                                                          *)
(* The request's executor drives the call's stream; when the request      *)
(* deadline cuts that stream (`drive_inference_stream`) it drops the       *)
(* stream, and with it the permit, and then writes the request's terminal. *)
(* Nothing orders that write after the dropped permit's cleanup: the two   *)
(* are separate actors, and recovery's periodic sweep acts on a live row   *)
(* only once its request has ended.                                         *)
(*                                                                          *)
(* The observer is a reader that wants the rows in a state: every row      *)
(* persisted, or every row terminal. It may wait on the tracker's flush,   *)
(* on the store's change subscription (`subscribe_collections`, armed      *)
(* before its first read), on a timer, or by polling within a budget. The  *)
(* model has no real-time step: a timer or a budget may expire before any  *)
(* write it was meant to cover.                                            *)
(*                                                                          *)
(* The terminal a dropped permit records is Lean                            *)
(* `InferenceCall.DroppedPermit.outcome`; this spec adds only the          *)
(* interleaving of the holder, the detached cleanup, the runtime's exit,   *)
(* recovery and the reader.                                                *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Calls,
  Tracked,              \* dropped guards count their cleanup in the tracker
  FinalizerCounted,     \* a finalizer is counted while it owns the permit
  ShutdownAwaitsFlush,  \* a graceful stop awaits the tracker before it exits
  TerminalCas,          \* a terminal installs only over a live row
  Recovery,             \* recovery terminalizes a live row of an ended call
  CanShutdown,
  CanExitAbruptly,
  Signal,               \* "flush" | "changes" | "timer" | "poll"
  Target,               \* "persisted" | "terminal"
  SubscribeFirst,       \* the change subscription is armed before the read
  PollBudget,
  AfterRequest          \* the observer begins once the request has ended

VARIABLES
  row,        \* none | queued | running | terminal
  writes,     \* terminal installs the row has taken
  permit,     \* none | waiting | held | finalizing | finishing | finished |
              \* dropped | dead
  cleanup,    \* none | spawned | done | lost
  inflight,   \* the tracker's count
  rt,         \* up | stopping | down
  exit,       \* none | graceful | deadline | abrupt
  obs,        \* idle | armed | unarmed | parked | waiting | done | timedOut
  subscribed,
  pending,    \* a change the subscription has not delivered yet
  polls,
  saw,        \* what the observer read when it finished
  req         \* live | ended: the request whose executor holds the calls

vars == <<row, writes, permit, cleanup, inflight, rt, exit, obs, subscribed,
          pending, polls, saw, req>>

Joined == {"waiting", "held"}          \* owned by a task the stop joins
Finalizer == {"finalizing", "finishing"} \* owned by a finalizer task
Live == Joined \cup Finalizer
Counts(c) == Tracked /\ FinalizerCounted /\ permit[c] \in Finalizer
Ended == {"finished", "dropped", "dead"}

TypeOK ==
  /\ row \in [Calls -> {"none", "queued", "running", "terminal"}]
  /\ writes \in [Calls -> 0..3]
  /\ permit \in [Calls -> {"none"} \cup Live \cup Ended]
  /\ cleanup \in [Calls -> {"none", "spawned", "done", "lost"}]
  /\ inflight \in Nat
  /\ rt \in {"up", "stopping", "down"}
  /\ exit \in {"none", "graceful", "deadline", "abrupt"}
  /\ obs \in {"idle", "armed", "unarmed", "parked", "waiting", "done", "timedOut"}
  /\ subscribed \in BOOLEAN
  /\ pending \in BOOLEAN
  /\ polls \in 0..PollBudget
  /\ saw \in BOOLEAN
  /\ req \in {"live", "ended"}

Init ==
  /\ row = [c \in Calls |-> "none"]
  /\ writes = [c \in Calls |-> 0]
  /\ permit = [c \in Calls |-> "none"]
  /\ cleanup = [c \in Calls |-> "none"]
  /\ inflight = 0
  /\ rt = "up"
  /\ exit = "none"
  /\ obs = "idle"
  /\ subscribed = FALSE
  /\ pending = FALSE
  /\ polls = 0
  /\ saw = FALSE
  /\ req = "live"

TargetHolds ==
  IF Target = "persisted"
    THEN \A c \in Calls : row[c] # "none"
    ELSE \A c \in Calls : row[c] = "terminal"

\* Every committed row write wakes an armed subscription.
Commit(c, state) ==
  /\ row' = [row EXCEPT ![c] = state]
  /\ pending' = (pending \/ subscribed)

Install(c) ==
  /\ Commit(c, "terminal")
  /\ writes' = [writes EXCEPT ![c] = @ + 1]

(* The holder. *)

Start(c) ==
  /\ rt = "up" /\ req = "live" /\ permit[c] = "none"
  /\ Commit(c, "running")
  /\ permit' = [permit EXCEPT ![c] = "held"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw, req>>

Enqueue(c) ==
  /\ rt = "up" /\ req = "live" /\ permit[c] = "none"
  /\ Commit(c, "queued")
  /\ permit' = [permit EXCEPT ![c] = "waiting"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw, req>>

Admit(c) ==
  /\ rt = "up" /\ permit[c] = "waiting" /\ row[c] = "queued"
  /\ Commit(c, "running")
  /\ permit' = [permit EXCEPT ![c] = "held"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw, req>>

\* The holder hands the permit to the finalizer, which commits the terminal
\* and then marks the permit finished; it may be aborted at either point.
BeginFinish(c) ==
  /\ rt # "down" /\ permit[c] = "held"
  /\ permit' = [permit EXCEPT ![c] = "finalizing"]
  /\ inflight' = IF Tracked /\ FinalizerCounted THEN inflight + 1 ELSE inflight
  /\ UNCHANGED <<row, writes, cleanup, rt, exit, obs, subscribed, pending,
                 polls, saw, req>>

FinishCommit(c) ==
  /\ rt # "down" /\ permit[c] = "finalizing" /\ row[c] = "running"
  /\ Install(c)
  /\ permit' = [permit EXCEPT ![c] = "finishing"]
  /\ UNCHANGED <<cleanup, inflight, rt, exit, obs, subscribed, polls, saw, req>>

Finished(c) ==
  /\ rt # "down" /\ permit[c] = "finishing"
  /\ permit' = [permit EXCEPT ![c] = "finished"]
  /\ inflight' = IF Counts(c) THEN inflight - 1 ELSE inflight
  /\ UNCHANGED <<row, writes, cleanup, rt, exit, obs, subscribed, pending,
                 polls, saw, req>>

\* `Drop` counts the cleanup in the tracker and spawns it in one synchronous
\* step; an aborted finalizer gives up its own count only after that.
Drop(c) ==
  /\ rt # "down" /\ permit[c] \in Live
  /\ permit' = [permit EXCEPT ![c] = "dropped"]
  /\ cleanup' = [cleanup EXCEPT ![c] = "spawned"]
  /\ inflight' = IF Tracked /\ ~Counts(c) THEN inflight + 1 ELSE inflight
  /\ UNCHANGED <<row, writes, rt, exit, obs, subscribed, pending, polls, saw, req>>

RunCleanup(c) ==
  /\ rt # "down" /\ cleanup[c] = "spawned"
  /\ IF row[c] # "terminal" \/ ~TerminalCas
       THEN Install(c)
       ELSE UNCHANGED <<row, writes, pending>>
  /\ cleanup' = [cleanup EXCEPT ![c] = "done"]
  /\ inflight' = IF Tracked THEN inflight - 1 ELSE inflight
  /\ UNCHANGED <<permit, rt, exit, obs, subscribed, polls, saw, req>>

(* The request. *)

\* The executor writes the request's terminal once its drive has returned,
\* that is once it has finished or dropped every permit it held; a dropped
\* permit's cleanup may still be in flight.
RequestEnd ==
  /\ rt # "down" /\ req = "live"
  /\ \A c \in Calls : permit[c] \in {"finished", "dropped"}
  /\ req' = "ended"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, obs, subscribed,
                 pending, polls, saw>>

(* The runtime. *)

BeginShutdown ==
  /\ CanShutdown /\ rt = "up"
  /\ rt' = "stopping"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, exit, obs, subscribed,
                 pending, polls, saw, req>>

\* An exit discards the cleanup in flight and every task still holding a
\* permit, whose `Drop` then has no runtime to hand its cleanup to.
Discard ==
  /\ cleanup' = [c \in Calls |-> IF cleanup[c] = "spawned" THEN "lost" ELSE cleanup[c]]
  /\ permit' = [c \in Calls |-> IF permit[c] \in Live THEN "dead" ELSE permit[c]]
  /\ inflight' = 0
  /\ rt' = "down"

\* A graceful stop has joined every task that waits for or holds a permit;
\* a finalizer is not one of them.
Exit ==
  /\ rt = "stopping"
  /\ \A c \in Calls : permit[c] \notin Joined
  /\ ShutdownAwaitsFlush => inflight = 0
  /\ Discard
  /\ exit' = "graceful"
  /\ UNCHANGED <<row, writes, obs, subscribed, pending, polls, saw, req>>

\* The teardown deadline cuts a stop that is still joining tasks or awaiting
\* the flush; the process then exits as it would on a crash or a kill.
AbruptExit ==
  /\ CanExitAbruptly /\ rt # "down"
  /\ Discard
  /\ exit' = IF rt = "stopping" THEN "deadline" ELSE "abrupt"
  /\ UNCHANGED <<row, writes, obs, subscribed, pending, polls, saw, req>>

\* Recovery acts on a live row whose request has ended: after the runtime
\* that owned it is gone, or in the periodic sweep once its permit dropped.
\* It is itself a compare-and-set on the state it read.
Recover(c) ==
  /\ Recovery /\ row[c] \in {"queued", "running"}
  /\ rt = "down" \/ (rt = "up" /\ permit[c] = "dropped" /\ req = "ended")
  /\ Install(c)
  /\ UNCHANGED <<permit, cleanup, inflight, rt, exit, obs, subscribed, polls, saw, req>>

(* The observer. *)

Begins == obs = "idle" => (AfterRequest => req = "ended")

Finish(seen) ==
  /\ obs' = "done"
  /\ saw' = seen

\* The flush is awaited by the party that ended the permits it asks about;
\* nothing tells it that a row not yet created is on its way.
ObsWait ==
  /\ Begins
  /\ Signal = "flush" /\ obs = "idle" /\ rt # "down"
  /\ Target = "terminal" => \A c \in Calls : permit[c] \in Ended
  /\ obs' = "waiting"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls, saw, req>>

ObsFlushed ==
  /\ obs = "waiting" /\ inflight = 0 /\ rt # "down"
  /\ Finish(TargetHolds)
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls, req>>

ObsSubscribe ==
  /\ Begins
  /\ Signal = "changes" /\ obs \in {"idle", "unarmed"} /\ rt # "down"
  /\ SubscribeFirst <=> obs = "idle"
  /\ subscribed' = TRUE
  /\ obs' = IF obs = "idle" THEN "armed" ELSE "parked"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, pending,
                 polls, saw, req>>

ObsRead ==
  /\ Begins
  /\ Signal = "changes" /\ rt # "down"
  /\ obs = "armed" \/ (obs = "idle" /\ ~SubscribeFirst)
  /\ IF TargetHolds
       THEN Finish(TRUE)
       ELSE /\ obs' = IF obs = "armed" THEN "parked" ELSE "unarmed"
            /\ UNCHANGED saw
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls, req>>

ObsWake ==
  /\ obs = "parked" /\ pending /\ rt # "down"
  /\ pending' = FALSE
  /\ obs' = "armed"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 polls, saw, req>>

ObsTimer ==
  /\ Begins
  /\ Signal = "timer" /\ obs = "idle" /\ rt # "down"
  /\ Finish(TargetHolds)
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls, req>>

ObsPoll ==
  /\ Begins
  /\ Signal = "poll" /\ obs = "idle" /\ rt # "down"
  /\ IF TargetHolds
       THEN Finish(TRUE) /\ UNCHANGED polls
       ELSE /\ polls' = polls + 1
            /\ obs' = IF polls + 1 = PollBudget THEN "timedOut" ELSE obs
            /\ UNCHANGED saw
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, req>>

Next ==
  \/ RequestEnd
  \/ BeginShutdown \/ Exit \/ AbruptExit
  \/ ObsWait \/ ObsFlushed \/ ObsSubscribe \/ ObsRead \/ ObsWake \/ ObsTimer \/ ObsPoll
  \/ \E c \in Calls :
       \/ Start(c) \/ Enqueue(c) \/ Admit(c) \/ BeginFinish(c) \/ FinishCommit(c)
       \/ Finished(c)
       \/ Drop(c) \/ RunCleanup(c) \/ Recover(c)

Acquire(c) == Start(c) \/ Enqueue(c)

Fairness ==
  /\ WF_vars(ObsWait) /\ WF_vars(ObsFlushed) /\ WF_vars(ObsSubscribe)
  /\ WF_vars(ObsRead) /\ WF_vars(ObsWake) /\ WF_vars(RequestEnd)
  /\ \A c \in Calls :
       WF_vars(Acquire(c)) /\ WF_vars(RunCleanup(c)) /\ WF_vars(Recover(c))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* A row takes its terminal once, whoever writes it.
ExactlyOneTerminal == \A c \in Calls : writes[c] <= 1

\* An observer that finished its wait read the rows in the state it waited
\* for.
ObserverSawTarget == obs = "done" => saw

\* A wait fails only when its target never comes, never because a budget ran
\* out first.
ObserverNeverTimesOut == obs # "timedOut"

\* A graceful stop discards no cleanup and no finalizer: every call it ends
\* has the terminal, and the usage, its own finish or `Drop` recorded.
GracefulExitFlushed ==
  exit = "graceful" =>
    \A c \in Calls : cleanup[c] # "lost" /\ permit[c] # "dead"

\* Every call that ended has a terminal row eventually: by its own finish,
\* by its cleanup, or by recovery after the runtime that owned it is gone.
EndedCallTerminal ==
  \A c \in Calls : permit[c] \in Ended ~> row[c] = "terminal"

\* Every call of an ended request has a terminal row eventually, written by
\* its own finish, its permit's cleanup, or recovery.
RequestEndedCallsTerminal ==
  req = "ended" ~> \A c \in Calls : row[c] = "terminal"

\* A reader that begins once the request has ended is told.
RequestEndedObserved == req = "ended" ~> obs = "done"

\* A reader waiting for a state the rows reach is told.
ObserverSettles == TargetHolds ~> obs = "done"

====
