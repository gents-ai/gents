---- MODULE PluginPublicationRevocation ----
\* lean: none
\* crates: gents
\* models: RuntimeReconcile.SurfacePublication.epochDeadline, RuntimeReconcile.SurfacePublication.reconcile
EXTENDS Naturals

\* The control watcher and an installer that holds the plugin store's
\* exclusive lock for as long as it likes. A configuration change (a revoked
\* plugin pin, or any other change) opens the publication epoch; the watcher
\* reconciles at each tick, never sleeping past the epoch's deadline (Rust
\* `next_publication_poll`). While the epoch is open, contention keeps the
\* active generation; at its deadline the latest configuration publishes and
\* only the busy plugin agents are demoted (Lean `reconcile`). `running` is
\* the digest new admissions run: an admitted turn may finish on its old pin,
\* which this model does not claim to interrupt.
\*
\* Safeguards, each removed by one constant: `EnforceBound` (contention stops
\* holding a change back at the deadline), `EnforcePins` (admission runs only
\* active pins), `KeepEpoch` (a later change never moves the deadline, Lean
\* `epochDeadline`), `KeepPrevious` (contention inside the epoch never
\* demotes).

CONSTANTS Bound, MaxEvents, EnforceBound, EnforcePins, KeepEpoch, KeepPrevious

VARIABLES busy, epochOpen, elapsed, sinceRevoke, events, desired, active, running,
          revoked, applied, earlyDemotion
vars == <<busy, epochOpen, elapsed, sinceRevoke, events, desired, active, running,
          revoked, applied, earlyDemotion>>

Min(a, b) == IF a < b THEN a ELSE b

Reconcile(contended, now, deadline) ==
  IF ~contended THEN "publish"
  ELSE IF ~KeepPrevious THEN "demote"
  ELSE IF EnforceBound /\ ~(now < deadline) THEN "demote"
  ELSE "keep"

Init ==
  /\ busy = TRUE
  /\ epochOpen = FALSE /\ elapsed = 0 /\ sinceRevoke = 0 /\ events = 0
  /\ desired = {1} /\ active = {1} /\ running = {1}
  /\ revoked = FALSE /\ applied = FALSE /\ earlyDemotion = FALSE

Open ==
  IF epochOpen /\ KeepEpoch
    THEN UNCHANGED <<epochOpen, elapsed>>
    ELSE epochOpen' = TRUE /\ elapsed' = 0

Revoke ==
  /\ ~revoked
  /\ revoked' = TRUE /\ desired' = {} /\ sinceRevoke' = 0
  /\ Open
  /\ UNCHANGED <<busy, events, active, running, applied, earlyDemotion>>

OtherChange ==
  /\ events < MaxEvents
  /\ events' = events + 1
  /\ Open
  /\ UNCHANGED <<busy, sinceRevoke, desired, active, running, revoked, applied, earlyDemotion>>

Publish(disposition) ==
  /\ active' = desired /\ running' = {}
  /\ applied' = (applied \/ revoked)
  /\ earlyDemotion' = (earlyDemotion \/ (disposition = "demote" /\ elapsed' < Bound))
  /\ epochOpen' = (disposition = "demote")

Tick ==
  /\ elapsed' = IF epochOpen THEN Min(elapsed + 1, Bound) ELSE elapsed
  /\ sinceRevoke' = IF revoked /\ ~applied THEN Min(sinceRevoke + 1, Bound) ELSE sinceRevoke
  /\ IF epochOpen
       THEN LET d == Reconcile(busy, elapsed', Bound) IN
            IF d = "keep" THEN UNCHANGED <<epochOpen, active, running, applied, earlyDemotion>>
            ELSE Publish(d)
       ELSE UNCHANGED <<epochOpen, active, running, applied, earlyDemotion>>
  /\ UNCHANGED <<busy, events, desired, revoked>>

Release ==
  /\ busy /\ busy' = FALSE
  /\ UNCHANGED <<epochOpen, elapsed, sinceRevoke, events, desired, active, running, revoked,
                 applied, earlyDemotion>>

Execute ==
  \E digest \in {1, 2}:
    /\ (~EnforcePins \/ digest \in active)
    /\ running' = {digest}
    /\ UNCHANGED <<busy, epochOpen, elapsed, sinceRevoke, events, desired, active, revoked,
                   applied, earlyDemotion>>

Next == Revoke \/ OtherChange \/ Tick \/ Release \/ Execute
Spec == Init /\ [][Next]_vars

\* A revocation applies within one budget of being made, however long the
\* store stays held and however many other changes arrive.
RevocationBound == (revoked /\ sinceRevoke = Bound) => applied
\* New admissions run only pins the active generation holds.
RunningWithinPins == running \subseteq active
\* Contention inside an open epoch never demotes an agent.
NoEarlyDemotion == ~earlyDemotion
====
