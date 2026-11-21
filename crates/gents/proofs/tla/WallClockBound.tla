---- MODULE WallClockBound ----
\* lean: none
\* crates: gents, gents-store, gents-toolcalls, gents-inference, gents-automation
(***************************************************************************)
(* A wall-clock bound racing an owner's progress in a test (#3840).        *)
(*                                                                          *)
(* A scenario needs `Steps` owner commits: a child row admitted, an output  *)
(* append, a callback invocation, an inference call row, a goal rescan.     *)
(* The host schedules every runnable step within `MaxDelay` ticks (the      *)
(* premise; nothing else bounds it) and lets time pass freely while nothing *)
(* is runnable. Two clocks can end the scenario:                            *)
(*                                                                          *)
(*  - the observer. With `Window = 0` it waits on the owner's own           *)
(*    notification (a database change, a join handle, a completion          *)
(*    registry) and re-reads after each one; it fails only when nothing     *)
(*    has changed for `Ceiling` ticks, the suite-wide hang ceiling           *)
(*    (`STALL_CEILING`, far above any scheduling delay). With `Window > 0`  *)
(*    it judges once at tick `Window` (`tokio::time::timeout`, a sleep      *)
(*    before one read, an elapsed-time assertion, a fixture that stops      *)
(*    after a fixed delay).                                                  *)
(*  - a product deadline (`DeadlineMode`). `"wallclock"` is a deadline the  *)
(*    fixture sets from the wall clock (a claim + 60 s request deadline, a  *)
(*    30 s tool deadline, the 5 s plugin budget): at tick `Deadline` the    *)
(*    product correctly cuts the scenario (refuses an append, kills the     *)
(*    process, times the call out). `"unreached"` is a fixture deadline     *)
(*    beyond the hang ceiling, or an append observed at the claim instant   *)
(*    (#3842): no run reaches it. `"driven"` is a deadline that is itself   *)
(*    the property: the test drives the cut (a paused clock advanced) once *)
(*    it has observed the steps the cut must follow.                        *)
(*                                                                          *)
(* `MCWallClockBound` and `MCWallClockBoundDriven`: the waiting observer    *)
(* with an unreached or driven deadline passes under every schedule and     *)
(* always reaches a verdict. `MCWallClockBoundFixedWindow` and             *)
(* `MCWallClockBoundWallDeadline` are the #3840 failures: the same correct  *)
(* owner fails the test whenever the host is slower than the window or the *)
(* deadline. `MCWallClockBoundStalledOwner`: the waiting observer still     *)
(* fails an owner that never commits, so removing the windows hides no     *)
(* lack of progress. `MCWallClockBoundSlowHost`: the ceiling is a premise   *)
(* about the host; a host that delays a runnable step past it fails a      *)
(* correct owner.                                                           *)
(*                                                                          *)
(* The `gents` e2e `session_message_turn_limit` chain is a FixedWindow      *)
(* instance: eight provider turns and their tool calls through one backend  *)
(* permit inside a 30 s window, about 6 s alone and past 30 s under a full  *)
(* suite. Its observer waits on progress rows instead, which a lease        *)
(* renewal does not change, so a renewing owner that stalls still fails.    *)
(*                                                                          *)
(* `deadline_closes_received_openai_reasoning_without_another_provider_call`*)
(* was a WallDeadline instance: a 3 s request deadline from the claim had  *)
(* to outlast the steps up to the first reasoning chunk, and a loaded host *)
(* cut the request before any chunk was received, leaving no prefix to    *)
(* close. It is Driven now: the deadline is past any run, and the test     *)
(* waits for the received reasoning's commit before it reaches the         *)
(* deadline itself. A paused clock cannot drive this cut: advancing it     *)
(* also fires the timers of the store calls the daemon has in flight.      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Steps,         \* owner commits the scenario needs
  MaxDelay,      \* ticks a runnable step may wait to be scheduled
  Ceiling,       \* ticks without a change before a waiting observer fails
  Window,        \* 0: the observer waits on notifications; n: it judges at tick n
  DeadlineMode,  \* "unreached" | "driven" | "wallclock"
  Deadline,      \* tick of a "wallclock" deadline
  OwnerStalls    \* the owner never commits

VARIABLES
  clock,    \* ticks since the scenario started (saturating at Cap)
  stall,    \* ticks since the last step
  done,     \* owner commits so far
  cut,      \* the product deadline cut the scenario
  verdict   \* "running" | "pass" | "fail"

vars == <<clock, stall, done, cut, verdict>>

Cap == Ceiling + Window + Deadline + 1

TypeOK ==
  /\ clock \in 0..Cap
  /\ stall \in 0..(Ceiling + MaxDelay)
  /\ done \in 0..Steps
  /\ cut \in BOOLEAN
  /\ verdict \in {"running", "pass", "fail"}

(* What the test asserts: every step committed and, when the deadline is *)
(* part of the property, the cut came after them.                        *)
Reached == done = Steps /\ (DeadlineMode = "driven" => cut)

OwnerRunnable == ~OwnerStalls /\ ~cut /\ done < Steps
DriverRunnable == DeadlineMode = "driven" /\ done = Steps /\ ~cut
Runnable == OwnerRunnable \/ DriverRunnable \/ Reached
Running == verdict = "running"

Init ==
  /\ clock = 0
  /\ stall = 0
  /\ done = 0
  /\ cut = FALSE
  /\ verdict = "running"

Commit ==
  /\ Running /\ OwnerRunnable
  /\ done' = done + 1
  /\ stall' = 0
  /\ UNCHANGED <<clock, cut, verdict>>

Tick ==
  /\ Running
  /\ IF Runnable THEN stall < MaxDelay ELSE stall < Ceiling
  /\ clock' = IF clock < Cap THEN clock + 1 ELSE clock
  /\ stall' = stall + 1
  /\ UNCHANGED <<done, cut, verdict>>

WallDeadline ==
  /\ Running /\ DeadlineMode = "wallclock" /\ ~cut /\ clock >= Deadline
  /\ cut' = TRUE
  /\ UNCHANGED <<clock, stall, done, verdict>>

DrivenDeadline ==
  /\ Running /\ DriverRunnable
  /\ cut' = TRUE
  /\ stall' = 0
  /\ UNCHANGED <<clock, done, verdict>>

(* Either observer returns once the awaited future completes. *)
Read ==
  /\ Running /\ Reached
  /\ verdict' = "pass"
  /\ UNCHANGED <<clock, stall, done, cut>>

Stalled ==
  /\ Running /\ Window = 0 /\ stall >= Ceiling
  /\ verdict' = "fail"
  /\ UNCHANGED <<clock, stall, done, cut>>

Expire ==
  /\ Running /\ Window > 0 /\ clock >= Window
  /\ verdict' = IF Reached THEN "pass" ELSE "fail"
  /\ UNCHANGED <<clock, stall, done, cut>>

Next == Commit \/ Tick \/ WallDeadline \/ DrivenDeadline \/ Read \/ Stalled \/ Expire

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Commit) /\ WF_vars(DrivenDeadline) /\ WF_vars(Read)
  /\ WF_vars(Tick) /\ WF_vars(WallDeadline) /\ WF_vars(Stalled) /\ WF_vars(Expire)

TestPasses == verdict # "fail"

ObserverSettles == <>(verdict # "running")

====
