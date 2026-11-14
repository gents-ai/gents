---- MODULE DroppedPermitCleanup ----
(***************************************************************************)
(* The InferenceCall row of one admitted provider call, from the row the   *)
(* admission controller persists to its terminal, and what a reader may    *)
(* await before it reads that row (#3659).                                 *)
(*                                                                          *)
(* A call's row is created `running` (`persist_call_started`) or `queued`  *)
(* (`persist_call_queued`) and then moved to `running`                     *)
(* (`persist_existing_call_running`). The holder ends the call explicitly: *)
(* `AdmissionPermit::finish` awaits the terminal write. A permit or a      *)
(* queued guard that is dropped instead (a cancelled or aborted task, a    *)
(* finish cancelled mid-write) cannot await in `Drop`: it hands the        *)
(* terminal write to `DropCleanup::spawn`, a detached task on the runtime  *)
(* the guard was created on. `CleanupTracker` counts those tasks from the  *)
(* `Drop` that spawns them until they finish or the runtime discards them, *)
(* and `AdmissionRegistry::drop_cleanup_idle` resolves when the count is   *)
(* zero. Every terminal install is a compare-and-set on a live row         *)
(* (`persist_existing_call_terminal`, Lean `InferenceCall.Persistence`), so *)
(* a cleanup that finds the row already terminal installs nothing.         *)
(*                                                                          *)
(* The runtime stops gracefully (`shutdown_runtime`: cancel, join the      *)
(* tasks that hold permits, flush, exit) or abruptly (a crash, a kill, or  *)
(* the teardown deadline cutting the joins or the flush, which has no      *)
(* deadline of its own). A runtime that exits discards the cleanup still   *)
(* in flight. Recovery (`InferenceCall::recover_all_with_identities`, at the  *)
(* next start and in the periodic sweep) terminalizes a live row whose     *)
(* request has ended, with its own reason and no usage.                    *)
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
  ShutdownAwaitsFlush,  \* a graceful stop awaits the tracker before it exits
  TerminalCas,          \* a terminal installs only over a live row
  Recovery,             \* recovery terminalizes a live row of an ended call
  CanShutdown,
  CanExitAbruptly,
  Signal,               \* "flush" | "changes" | "timer" | "poll"
  Target,               \* "persisted" | "terminal"
  SubscribeFirst,       \* the change subscription is armed before the read
  PollBudget

VARIABLES
  row,        \* none | queued | running | terminal
  writes,     \* terminal installs the row has taken
  permit,     \* none | waiting | held | finishing | finished | dropped | dead
  cleanup,    \* none | spawned | done | lost
  inflight,   \* the tracker's count
  rt,         \* up | stopping | down
  exit,       \* none | graceful | deadline | abrupt
  obs,        \* idle | armed | unarmed | parked | waiting | done | timedOut
  subscribed,
  pending,    \* a change the subscription has not delivered yet
  polls,
  saw         \* what the observer read when it finished

vars == <<row, writes, permit, cleanup, inflight, rt, exit, obs, subscribed,
          pending, polls, saw>>

Live == {"waiting", "held", "finishing"}
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
  /\ rt = "up" /\ permit[c] = "none"
  /\ Commit(c, "running")
  /\ permit' = [permit EXCEPT ![c] = "held"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw>>

Enqueue(c) ==
  /\ rt = "up" /\ permit[c] = "none"
  /\ Commit(c, "queued")
  /\ permit' = [permit EXCEPT ![c] = "waiting"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw>>

Admit(c) ==
  /\ rt = "up" /\ permit[c] = "waiting" /\ row[c] = "queued"
  /\ Commit(c, "running")
  /\ permit' = [permit EXCEPT ![c] = "held"]
  /\ UNCHANGED <<writes, cleanup, inflight, rt, exit, obs, subscribed, polls, saw>>

\* `finish` commits the terminal and then marks the permit finished; the
\* future may be dropped between the two.
FinishCommit(c) ==
  /\ rt # "down" /\ permit[c] = "held" /\ row[c] = "running"
  /\ Install(c)
  /\ permit' = [permit EXCEPT ![c] = "finishing"]
  /\ UNCHANGED <<cleanup, inflight, rt, exit, obs, subscribed, polls, saw>>

Finished(c) ==
  /\ rt # "down" /\ permit[c] = "finishing"
  /\ permit' = [permit EXCEPT ![c] = "finished"]
  /\ UNCHANGED <<row, writes, cleanup, inflight, rt, exit, obs, subscribed,
                 pending, polls, saw>>

\* `Drop` counts the cleanup in the tracker and spawns it in one synchronous
\* step.
Drop(c) ==
  /\ rt # "down" /\ permit[c] \in Live
  /\ permit' = [permit EXCEPT ![c] = "dropped"]
  /\ cleanup' = [cleanup EXCEPT ![c] = "spawned"]
  /\ inflight' = IF Tracked THEN inflight + 1 ELSE inflight
  /\ UNCHANGED <<row, writes, rt, exit, obs, subscribed, pending, polls, saw>>

RunCleanup(c) ==
  /\ rt # "down" /\ cleanup[c] = "spawned"
  /\ IF row[c] # "terminal" \/ ~TerminalCas
       THEN Install(c)
       ELSE UNCHANGED <<row, writes, pending>>
  /\ cleanup' = [cleanup EXCEPT ![c] = "done"]
  /\ inflight' = IF Tracked THEN inflight - 1 ELSE inflight
  /\ UNCHANGED <<permit, rt, exit, obs, subscribed, polls, saw>>

(* The runtime. *)

BeginShutdown ==
  /\ CanShutdown /\ rt = "up"
  /\ rt' = "stopping"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, exit, obs, subscribed,
                 pending, polls, saw>>

Discard ==
  /\ cleanup' = [c \in Calls |-> IF cleanup[c] = "spawned" THEN "lost" ELSE cleanup[c]]
  /\ inflight' = 0
  /\ rt' = "down"

\* A graceful stop has joined every task that holds a permit.
Exit ==
  /\ rt = "stopping"
  /\ \A c \in Calls : permit[c] \notin Live
  /\ ShutdownAwaitsFlush => inflight = 0
  /\ Discard
  /\ exit' = "graceful"
  /\ UNCHANGED <<row, writes, permit, obs, subscribed, pending, polls, saw>>

\* The teardown deadline cuts a stop that is still joining tasks or awaiting
\* the flush; the process then exits as it would on a crash or a kill.
AbruptExit ==
  /\ CanExitAbruptly /\ rt # "down"
  /\ Discard
  /\ exit' = IF rt = "stopping" THEN "deadline" ELSE "abrupt"
  /\ permit' = [c \in Calls |-> IF permit[c] \in Live THEN "dead" ELSE permit[c]]
  /\ UNCHANGED <<row, writes, obs, subscribed, pending, polls, saw>>

\* Recovery acts on a live row whose request has ended: after the runtime
\* that owned it is gone, or in the periodic sweep once its permit dropped.
\* It is itself a compare-and-set on the state it read.
Recover(c) ==
  /\ Recovery /\ row[c] \in {"queued", "running"}
  /\ rt = "down" \/ (rt = "up" /\ permit[c] = "dropped")
  /\ Install(c)
  /\ UNCHANGED <<permit, cleanup, inflight, rt, exit, obs, subscribed, polls, saw>>

(* The observer. *)

Finish(seen) ==
  /\ obs' = "done"
  /\ saw' = seen

\* The flush is awaited by the party that ended the permits it asks about;
\* nothing tells it that a row not yet created is on its way.
ObsWait ==
  /\ Signal = "flush" /\ obs = "idle" /\ rt # "down"
  /\ Target = "terminal" => \A c \in Calls : permit[c] \in Ended
  /\ obs' = "waiting"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls, saw>>

ObsFlushed ==
  /\ obs = "waiting" /\ inflight = 0 /\ rt # "down"
  /\ Finish(TargetHolds)
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls>>

ObsSubscribe ==
  /\ Signal = "changes" /\ obs \in {"idle", "unarmed"} /\ rt # "down"
  /\ SubscribeFirst <=> obs = "idle"
  /\ subscribed' = TRUE
  /\ obs' = IF obs = "idle" THEN "armed" ELSE "parked"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, pending,
                 polls, saw>>

ObsRead ==
  /\ Signal = "changes" /\ rt # "down"
  /\ obs = "armed" \/ (obs = "idle" /\ ~SubscribeFirst)
  /\ IF TargetHolds
       THEN Finish(TRUE)
       ELSE /\ obs' = IF obs = "armed" THEN "parked" ELSE "unarmed"
            /\ UNCHANGED saw
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls>>

ObsWake ==
  /\ obs = "parked" /\ pending /\ rt # "down"
  /\ pending' = FALSE
  /\ obs' = "armed"
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 polls, saw>>

ObsTimer ==
  /\ Signal = "timer" /\ obs = "idle" /\ rt # "down"
  /\ Finish(TargetHolds)
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending, polls>>

ObsPoll ==
  /\ Signal = "poll" /\ obs = "idle" /\ rt # "down"
  /\ IF TargetHolds
       THEN Finish(TRUE) /\ UNCHANGED polls
       ELSE /\ polls' = polls + 1
            /\ obs' = IF polls + 1 = PollBudget THEN "timedOut" ELSE obs
            /\ UNCHANGED saw
  /\ UNCHANGED <<row, writes, permit, cleanup, inflight, rt, exit, subscribed,
                 pending>>

Next ==
  \/ BeginShutdown \/ Exit \/ AbruptExit
  \/ ObsWait \/ ObsFlushed \/ ObsSubscribe \/ ObsRead \/ ObsWake \/ ObsTimer \/ ObsPoll
  \/ \E c \in Calls :
       \/ Start(c) \/ Enqueue(c) \/ Admit(c) \/ FinishCommit(c) \/ Finished(c)
       \/ Drop(c) \/ RunCleanup(c) \/ Recover(c)

Acquire(c) == Start(c) \/ Enqueue(c)

Fairness ==
  /\ WF_vars(ObsWait) /\ WF_vars(ObsFlushed) /\ WF_vars(ObsSubscribe)
  /\ WF_vars(ObsRead) /\ WF_vars(ObsWake)
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

\* A graceful stop discards no cleanup: every permit that ended before the
\* exit has the terminal, and the usage, its own `Drop` recorded.
GracefulExitFlushed ==
  exit = "graceful" => \A c \in Calls : cleanup[c] # "lost"

\* Every call that ended has a terminal row eventually: by its own finish,
\* by its cleanup, or by recovery after the runtime that owned it is gone.
EndedCallTerminal ==
  \A c \in Calls : permit[c] \in Ended ~> row[c] = "terminal"

\* A reader waiting for a state the rows reach is told.
ObserverSettles == TargetHolds ~> obs = "done"

====
