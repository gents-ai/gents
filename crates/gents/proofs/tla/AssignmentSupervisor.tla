---- MODULE AssignmentSupervisor ----
\* lean: none
\* crates: gents, gents-store
\* models: RuntimeReconcile.Placement.desired, AssignmentReceiver.permits
EXTENDS Naturals, Sequences

\* Certified placement as an input of the agent supervisor on a Vera home
\* (gents#4245). The issuer withdraws B and places C (revision 0 holds {B},
\* revision 1 holds {C}). The Receiver commits the floor row before the stops
\* it owes and the applied row after them; it starts and stops nothing, and
\* waits on the supervisor's stop barrier (`stopped`: no published or queued
\* generation holds the id and its slot has joined). The ControlWatcher builds each
\* proposal from the durable rows and the verified bodies with Lean
\* `Placement.desired`; while a PluginInstaller holds the plugin store it keeps
\* the previous generation, except that a proposal withdrawing placed agents
\* publishes that withdrawal at once. The Supervisor applies proposals in
\* order, starts the agents of its generation and retires the others. BodySync
\* delivers C's verified body. A Crash loses everything in memory; the rows
\* survive. The model omits the publication epoch's deadline, so the installer
\* may hold the store for ever.
\*
\* Safeguards, each removed by one constant: `AppliedOnly` (the watcher reads
\* the applied row alone), `IgnoreBody` (an unverified body counts as
\* verified), `HoldWithdrawals` (contention holds a withdrawal back too),
\* `CrashRestoresOlderFloor` (a restart reads an older floor),
\* `BarrierIgnoresQueue` (the stop barrier looks only at the published
\* generation and the running slots).

CONSTANTS AppliedOnly, IgnoreBody, HoldWithdrawals, CrashRestoresOlderFloor, BarrierIgnoresQueue,
          MaxCrashes

VARIABLES floor, applied, body, held, queue, generation, running, retired, crashes
vars == <<floor, applied, body, held, queue, generation, running, retired, crashes>>

Ids == {"B", "C"}
Assigned(revision) == IF revision = 0 THEN {"B"} ELSE {"C"}

\* Lean `Placement.desired` on a Vera home, every Agent fresh.
PlacementDesired(inFloor, inApplied, bodyVerified) ==
  IF ~(inFloor /\ inApplied) THEN "absent"
  ELSE IF bodyVerified THEN "run"
  ELSE "waiting"

Decision(id) ==
  PlacementDesired(
    id \in Assigned(IF AppliedOnly THEN applied ELSE floor),
    id \in Assigned(applied), IgnoreBody \/ id \in body)

Desired == {id \in Ids : Decision(id) = "run"}

Init ==
  /\ floor = 0 /\ applied = 0
  /\ body = {"B"} /\ held = FALSE
  /\ queue = <<>> /\ generation = {"B"} /\ running = {"B"}
  /\ retired = FALSE /\ crashes = 0

\* Receiver: a certified read of revision 1 is accepted and its floor row
\* committed.
Accept ==
  /\ floor = 0 /\ floor' = 1
  /\ UNCHANGED <<applied, body, held, queue, generation, running, crashes>>

Queued == UNION {queue[i] : i \in DOMAIN queue}

\* The supervisor's stop barrier also waits out proposals built before the
\* floor committed: one still queued would start the agent again.
Stopped(ids) ==
  ids \cap (generation \cup running \cup (IF BarrierIgnoresQueue THEN {} ELSE Queued)) = {}

\* Receiver: once the supervisor's barrier reports the withdrawn agents
\* stopped, the applied row follows the floor.
Commit ==
  /\ applied < floor
  /\ Stopped(Assigned(applied) \ Assigned(floor))
  /\ applied' = floor
  /\ UNCHANGED <<floor, body, held, queue, generation, running, crashes>>

Last == IF queue = <<>> THEN generation ELSE queue[Len(queue)]

Proposal ==
  IF ~held THEN Desired
  ELSE IF ~HoldWithdrawals /\ Last \ Desired # {} THEN Last \cap Desired
  ELSE Last

Reconcile ==
  /\ Len(queue) < 2
  /\ Proposal # Last
  /\ queue' = Append(queue, Proposal)
  /\ UNCHANGED <<floor, applied, body, held, generation, running, crashes>>

Apply ==
  /\ queue # <<>>
  /\ generation' = Head(queue) /\ queue' = Tail(queue)
  /\ UNCHANGED <<floor, applied, body, held, running, crashes>>

Start(id) ==
  /\ id \in generation \ running
  /\ running' = running \cup {id}
  /\ UNCHANGED <<floor, applied, body, held, queue, generation, crashes>>

Retire(id) ==
  /\ id \in running \ generation
  /\ running' = running \ {id}
  /\ UNCHANGED <<floor, applied, body, held, queue, generation, crashes>>

BodyArrives ==
  /\ "C" \notin body /\ body' = body \cup {"C"}
  /\ UNCHANGED <<floor, applied, held, queue, generation, running, crashes>>

Hold ==
  /\ ~held /\ held' = TRUE
  /\ UNCHANGED <<floor, applied, body, queue, generation, running, crashes>>

Release ==
  /\ held /\ held' = FALSE
  /\ UNCHANGED <<floor, applied, body, queue, generation, running, crashes>>

Crash ==
  /\ crashes < MaxCrashes /\ crashes' = crashes + 1
  /\ running' = {} /\ generation' = {} /\ queue' = <<>>
  /\ floor' = IF CrashRestoresOlderFloor THEN applied ELSE floor
  /\ UNCHANGED <<applied, body, held>>

Step ==
  \/ Accept \/ Commit \/ Reconcile \/ Apply
  \/ \E id \in Ids: Start(id) \/ Retire(id)
  \/ BodyArrives \/ Hold \/ Release \/ Crash

\* `retired` records that B has left the generation and its slot has joined
\* after a floor withdrew it.
Next == Step /\ retired' = (retired \/ (floor' = 1 /\ Stopped({"B"})'))

Fairness ==
  /\ WF_vars(Reconcile /\ retired' = (retired \/ (floor' = 1 /\ Stopped({"B"})')))
  /\ WF_vars(Apply /\ retired' = (retired \/ (floor' = 1 /\ Stopped({"B"})')))
  /\ WF_vars(Retire("B") /\ retired' = (retired \/ (floor' = 1 /\ Stopped({"B"})')))

Spec == Init /\ [][Next]_vars /\ Fairness

\* Every running agent's body is verified.
VerifiedBody == running \subseteq body
\* Once withdrawn and stopped, B stays out of every generation, across crashes.
NoRevival == retired => ~("B" \in generation \cup running)
\* The replacement starts only after the withdrawal it follows has stopped.
NoOverlap == ~(Ids \subseteq running)
\* A withdrawn agent stops, however long the installer holds the store.
WithdrawalStops == (floor = 1) ~> Stopped({"B"})
====
