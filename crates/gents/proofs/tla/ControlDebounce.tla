---- MODULE ControlDebounce ----
\* lean: none
\* crates: gents
\* models: ControlDebounce.due
EXTENDS Naturals

\* The runtime's control watcher and the wakes that reach it: a pending
\* control change, then wakes at any moment for as long as the home is busy
\* (access changes on a Vera home, unrelated to control documents). Each wake
\* schedules the reconcile; the timer fires at its deadline.
\*
\* Wakes stop at `Horizon`; time goes on while a reconcile is pending.
\*
\* Safeguard and premise, each removed by one constant:
\*   Uncapped      each wake schedules the reconcile `Delay` after itself alone;
\*   TimerStalls   the runtime does not run the watcher once its timer is due
\*                 (the timer premise: an expired tokio sleep is polled).

CONSTANTS Delay, Ceiling, Horizon, Uncapped, TimerStalls

\* Lean `ControlDebounce.due`.
Due(first, now) ==
  IF Uncapped THEN now + Delay
  ELSE IF now + Delay <= first + Ceiling THEN now + Delay ELSE first + Ceiling

VARIABLES clock, pending, first, due, reconciled
vars == <<clock, pending, first, due, reconciled>>

Init ==
  /\ clock = 0
  /\ pending = FALSE
  /\ first = 0
  /\ due = 0
  /\ reconciled = FALSE

Wake ==
  /\ clock < Horizon
  /\ first' = IF pending THEN first ELSE clock
  /\ pending' = TRUE
  /\ due' = Due(IF pending THEN first ELSE clock, clock)
  /\ UNCHANGED <<clock, reconciled>>

\* Time moves on only up to a pending deadline: the timer is not late.
Tick ==
  /\ clock < Horizon \/ pending
  /\ ~pending \/ clock < due
  /\ clock' = clock + 1
  /\ UNCHANGED <<pending, first, due, reconciled>>

Fire ==
  /\ pending /\ clock >= due
  /\ pending' = FALSE
  /\ reconciled' = TRUE
  /\ UNCHANGED <<clock, first, due>>

Next == Wake \/ Tick \/ Fire
Spec == Init /\ [][Next]_vars /\ WF_vars(Tick)
          /\ (IF TimerStalls THEN TRUE ELSE WF_vars(Fire))

\* A pending change is reconciled within `Ceiling` of its first wake, however
\* the wakes that follow it arrive.
NeverStarved == pending => clock <= first + Ceiling

\* Every pending change is reconciled.
Reconciled == pending ~> ~pending
====
