---- MODULE GuestAllowance ----
(***************************************************************************)
(* How long a computing plugin guest runs once it has its compute         *)
(* allowance, and when the worker thread it occupies is released.         *)
(*                                                                          *)
(* A host-call guest runs on a Tokio blocking thread                       *)
(* (`gents-toolcalls/src/plugin/host_calls.rs` `run`, the `spawn_blocking` *)
(* closure) inside afterburner's shared epoch VM (`shared_epoch_vm_with`,  *)
(* `gents-pack/src/plugin/host_imports.rs:125`). Its allowance is the Lean *)
(* `computeAllowance` (the declared wall clock, five seconds by default), *)
(* handed to `run_command_bounded` as a `timeout`. Afterburner turns that  *)
(* into a tick count, `ceil(timeout / EPOCH_TICK_PERIOD_MS)`, and sets the *)
(* store's epoch deadline to the engine's current epoch plus that count    *)
(* when it creates the store, after compiling the module                   *)
(* (`embedder_vm.rs` `run_command_raw`). Compiled code checks the epoch at *)
(* every function entry and loop back-edge and traps once it has reached   *)
(* the deadline (`EpochChecked`), so the guest's cooperation is not        *)
(* needed. The epoch is advanced by one process-wide ticker thread:        *)
(* `loop { sleep(EPOCH_TICK_PERIOD_MS); engine.increment_epoch() }`        *)
(* (`embedder_vm.rs` `shared_epoch_vm_with`). A sleep ends no earlier than *)
(* its period and up to `Late` units after it, by scheduling and timer     *)
(* coalescing; nothing in the ticker bounds that lateness.                 *)
(*                                                                          *)
(* Cancelling the invocation drops the async side, whose `StopOnDrop` sets *)
(* the stop flag. The VM exposes the store only inside a host import, so   *)
(* the flag revokes the allowance at the guest's next host call; a guest   *)
(* that never calls the host (this spec) is ended only by its allowance.   *)
(* The worker returns when the guest traps, which drops what occupied the  *)
(* thread (the test's `live::Guard`).                                      *)
(*                                                                          *)
(* `CatchUp` is the proposed afterburner change: each wake advances the    *)
(* epoch to the number of whole periods elapsed since the ticker started,  *)
(* not by one. Without it the allowance is counted in wake-ups, and a     *)
(* stop that is never lost (`EveryGuestReleased` holds either way) can     *)
(* come `A * Late` units after the declared wall clock.                    *)
(*                                                                          *)
(* Units: one unit of `now` is a fraction of a tick period `P`.            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  P,             \* the ticker's sleep period (EPOCH_TICK_PERIOD_MS)
  A,             \* the allowance in ticks: ceil(declared / P)
  Late,          \* the most a sleep may overrun its period
  MaxCompile,    \* time from the worker starting to the store existing
  CatchUp,       \* a wake advances the epoch to elapsed / P
  EpochChecked   \* compiled loops check the epoch at each back-edge

\* One allowance plus the wait for the first wake after it: a period and its lateness.
StopBound == A * P + P + Late
Horizon == MaxCompile + A * (P + Late) + StopBound + 1

VARIABLES
  now,        \* wall-clock time
  epoch,      \* the engine's epoch
  wakeAt,     \* when the ticker's current sleep ends
  guest,      \* compiling | running | trapped
  started,    \* when the store was created with its deadline
  deadline,   \* the store's epoch deadline
  cancelled,  \* the invocation was cancelled (its async side dropped)
  released,   \* the worker returned and released its thread
  cancelSaw   \* ghost: the guest's phase when the cancel arrived

vars == <<now, epoch, wakeAt, guest, started, deadline, cancelled, released, cancelSaw>>

Max(a, b) == IF a > b THEN a ELSE b

TypeOK ==
  /\ now \in 0..Horizon
  /\ epoch \in Nat
  /\ wakeAt \in Nat
  /\ guest \in {"compiling", "running", "trapped"}
  /\ started \in 0..Horizon
  /\ deadline \in Nat
  /\ cancelled \in BOOLEAN
  /\ released \in BOOLEAN
  /\ cancelSaw \in {"none", "compiling", "running", "trapped"}

Init ==
  /\ now = 0
  /\ epoch = 0
  /\ wakeAt = P
  /\ guest = "compiling"
  /\ started = 0
  /\ deadline = 0
  /\ cancelled = FALSE
  /\ released = FALSE
  /\ cancelSaw = "none"

TrapDue == guest = "running" /\ EpochChecked /\ epoch >= deadline

\* Time passes, but not past a sleep's latest end, not past a due trap (the
\* back-edge check is immediate) and not past the compile bound.
Advance ==
  /\ now < Horizon
  /\ now + 1 <= wakeAt + Late
  /\ ~TrapDue
  /\ guest = "compiling" => now + 1 <= MaxCompile
  /\ now' = now + 1
  /\ UNCHANGED <<epoch, wakeAt, guest, started, deadline, cancelled, released, cancelSaw>>

\* The ticker's sleep ends: `increment_epoch` once, or catch up to elapsed time.
Wake ==
  /\ now >= wakeAt
  /\ epoch' = IF CatchUp THEN Max(epoch + 1, now \div P) ELSE epoch + 1
  /\ wakeAt' = now + P
  /\ UNCHANGED <<now, guest, started, deadline, cancelled, released, cancelSaw>>

\* `run_command_raw`: the store is created after compiling, with its deadline.
StartGuest ==
  /\ guest = "compiling"
  /\ guest' = "running"
  /\ started' = now
  /\ deadline' = epoch + A
  /\ UNCHANGED <<now, epoch, wakeAt, cancelled, released, cancelSaw>>

Trap ==
  /\ TrapDue
  /\ guest' = "trapped"
  /\ UNCHANGED <<now, epoch, wakeAt, started, deadline, cancelled, released, cancelSaw>>

\* The worker returns; whatever occupied the thread is dropped.
Release ==
  /\ guest = "trapped"
  /\ ~released
  /\ released' = TRUE
  /\ UNCHANGED <<now, epoch, wakeAt, guest, started, deadline, cancelled, cancelSaw>>

\* Cancellation drops the async side. The guest never calls the host, so
\* the revoked allowance never reaches its store. Nothing orders the cancel
\* against the allowance: it may arrive after the guest already stopped.
Cancel ==
  /\ ~cancelled
  /\ cancelled' = TRUE
  /\ cancelSaw' = guest
  /\ UNCHANGED <<now, epoch, wakeAt, guest, started, deadline, released>>

Next == Advance \/ Wake \/ StartGuest \/ Trap \/ Release \/ Cancel

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Advance) /\ WF_vars(Wake) /\ WF_vars(StartGuest)
  /\ WF_vars(Trap) /\ WF_vars(Release)

(* A running guest is stopped within its declared wall clock plus one     *)
(* period and one late wake, however late every wake is.                  *)
StopsAtAllowance == guest = "running" => now - started <= StopBound

\* The thread is released only after the guest stopped.
ReleaseFollowsStop == released => guest = "trapped"

\* No stop is lost: every guest is stopped and its worker released.
EveryGuestReleased == <>released

(* What the test assumed, refuted by `MCGuestAllowanceCancelLate`: that a   *)
(* cancel always finds the guest still computing.                          *)
CancelFindsGuestRunning == cancelled => cancelSaw \in {"compiling", "running"}
====
