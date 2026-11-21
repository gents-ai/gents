---- MODULE AccessWake ----
\* lean: AccessWake.listens
\* crates: gents-store, gents-session, gents-toolcalls, gents
EXTENDS LeanDecision_AccessWake

\* One runtime watcher and one document written on a home whose document
\* access control is decided by `Provider`. The writer's commit makes the row
\* and its change event; on an embedded Vera node the watcher's read sees the
\* row only once the provider settled its access, which the node publishes as
\* an access change after its access cache let go of the old answers. The
\* watcher waits on its change subscription and then reads its whole scope;
\* its fallback timer is what wakes it when nothing else does.
\*
\* Safeguards, each removed by one constant:
\*   IgnoreAccess   the watcher listens only for row changes;
\*   EarlyWake      the access change is published when the row commits,
\*                  before the read can see it, and not again;
\*   ClearAfterRead the watcher forgets wakes that arrived during its read.

CONSTANTS Provider, IgnoreAccess, EarlyWake, ClearAfterRead

Listens == ~IgnoreAccess /\ listens(Provider)
Vera == Provider = "embedded_vera"

VARIABLES committed, settled, rowWake, accessWake, phase, sampled, seen, fallback
vars == <<committed, settled, rowWake, accessWake, phase, sampled, seen, fallback>>

Visible == committed /\ (settled \/ ~Vera)

Init ==
  /\ committed = FALSE
  /\ settled = FALSE
  /\ rowWake = FALSE
  /\ accessWake = FALSE
  /\ phase = "waiting"
  /\ sampled = FALSE
  /\ seen = FALSE
  /\ fallback = FALSE

Commit ==
  /\ ~committed
  /\ committed' = TRUE
  /\ rowWake' = TRUE
  /\ accessWake' = (accessWake \/ (Vera /\ EarlyWake /\ Listens))
  /\ UNCHANGED <<settled, phase, sampled, seen, fallback>>

Settle ==
  /\ Vera /\ committed /\ ~settled
  /\ settled' = TRUE
  /\ accessWake' = (accessWake \/ (~EarlyWake /\ Listens))
  /\ UNCHANGED <<committed, rowWake, phase, sampled, seen, fallback>>

\* A read decides on the state at its start; what changes while it runs is
\* the next read's to see.
Wake ==
  /\ phase = "waiting"
  /\ rowWake \/ accessWake
  /\ phase' = "reading"
  /\ sampled' = Visible
  /\ rowWake' = (rowWake /\ ClearAfterRead)
  /\ accessWake' = (accessWake /\ ClearAfterRead)
  /\ UNCHANGED <<committed, settled, seen, fallback>>

Read ==
  /\ phase = "reading"
  /\ seen' = (seen \/ sampled)
  /\ phase' = "waiting"
  /\ rowWake' = (rowWake /\ ~ClearAfterRead)
  /\ accessWake' = (accessWake /\ ~ClearAfterRead)
  /\ UNCHANGED <<committed, settled, sampled, fallback>>

\* The fallback fires only once nothing else can happen: a watcher that still
\* needs it is one the events left asleep.
Quiet == ~ENABLED (Commit \/ Settle \/ Wake \/ Read)

Fallback ==
  /\ phase = "waiting" /\ ~seen /\ committed
  /\ Quiet
  /\ fallback' = TRUE
  /\ phase' = "reading"
  /\ sampled' = Visible
  /\ UNCHANGED <<committed, settled, rowWake, accessWake, seen>>

Next == Commit \/ Settle \/ Wake \/ Read \/ Fallback
Spec == Init /\ [][Next]_vars /\ WF_vars(Commit) /\ WF_vars(Settle) /\ WF_vars(Wake)
          /\ WF_vars(Read) /\ WF_vars(Fallback)

\* The watcher reads the row without its fallback.
WokenByEvents == ~fallback
Seen == <> seen
====
