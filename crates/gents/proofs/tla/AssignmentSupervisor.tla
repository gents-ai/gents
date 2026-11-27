---- MODULE AssignmentSupervisor ----
\* lean: none
\* crates: gents, gents-store
\* models: RuntimeReconcile.Placement.desired, AssignmentReceiver.permits, RuntimeReconcile.Placement.admitsProposal, RuntimeReconcile.Placement.proposalOutcome, RuntimeReconcile.Placement.stopped
EXTENDS Naturals, Sequences

\* Certified placement as an input of the agent supervisor on a Vera home
\* (gents#4245). The issuer withdraws B and places C (revision 0 holds {B},
\* revision 1 holds {C}). The Receiver commits the floor row before the stops
\* it owes and the applied row after them; it starts and stops nothing. After
\* the floor commit it raises the supervisor's placement fence to the floor and
\* waits on the stop barrier (`stopped`). The ControlWatcher builds each
\* proposal from the durable rows and the verified bodies with Lean
\* `Placement.desired`, and records as the proposal's basis the floor it read
\* in the same transaction. While a PluginInstaller holds the plugin store it
\* keeps the previous generation, except that a proposal withdrawing placed
\* agents or advancing the basis publishes at once, keeping only what the floor
\* still admits. The watcher sends a proposal when its agents or its basis
\* differ from the last one. The Supervisor takes proposals in order
\* (`Admit`): one whose basis is below the fence is dropped, one whose agents
\* match the generation only advances the published basis, and any other is
\* published as the next generation (`Publish`), a separate step because an
\* admitted proposal may publish after the fence rises. It starts the agents
\* of its generation and retires the others; `running` holds every slot that
\* has not joined. The barrier passes once the published basis has reached
\* the floor and no withdrawn agent is in the generation or still running.
\* BodySync delivers C's verified body. A Crash loses everything in memory,
\* the fence included: nothing is queued or admitted afterwards and the
\* supervisor's bootstrap reads the durable rows, so
\* its basis is the floor and the fence is not needed again until the receiver
\* raises it. The rows survive. The model omits the publication epoch's
\* deadline, so the installer may hold the store for ever.
\*
\* Safeguards, each removed by one constant: `AppliedOnly` (the watcher reads
\* the applied row alone), `IgnoreBody` (an unverified body counts as
\* verified), `HoldWithdrawals` (contention holds a withdrawal back too),
\* `CrashRestoresOlderFloor` (a restart reads an older floor),
\* `BarrierIgnoresBasis` (the barrier looks only at the generation and the
\* running slots), `KeepStale` (the supervisor applies a proposal below the
\* fence), `NoopKeepsBasis` (an unchanged proposal does not advance the
\* published basis), `SendOnAgentsOnly` (the watcher sends only when the agents
\* change).

CONSTANTS AppliedOnly, IgnoreBody, HoldWithdrawals, CrashRestoresOlderFloor, BarrierIgnoresBasis,
          KeepStale, NoopKeepsBasis, SendOnAgentsOnly, MaxCrashes

VARIABLES floor, applied, body, held, fence, queue, applying, generation, basis, running,
          retired, crashes
vars == <<floor, applied, body, held, fence, queue, applying, generation, basis, running,
          retired, crashes>>

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
  /\ body = {"B"} /\ held = FALSE /\ fence = 0
  /\ queue = <<>> /\ applying = <<>>
  /\ generation = Desired /\ basis = 0 /\ running = generation
  /\ retired = FALSE /\ crashes = 0

Pending == UNION {applying[i].agents : i \in DOMAIN applying}

\* `retired` records that B stopped behind a raised fence: out of the
\* generation, out of every admitted proposal, its slot joined.
Retired == retired' = (retired \/ (fence' = 1 /\ "B" \notin generation' \cup running' \cup Pending'))

\* Receiver: a certified read of revision 1 is accepted and its floor row
\* committed.
Accept ==
  /\ floor = 0 /\ floor' = 1
  /\ UNCHANGED <<applied, body, held, fence, queue, applying, generation, basis, running, crashes>>

\* Receiver: right after the floor commit, the fence rises to the floor.
Raise ==
  /\ fence < floor /\ fence' = floor
  /\ UNCHANGED <<floor, applied, body, held, queue, applying, generation, basis, running, crashes>>

\* Lean `Placement.stopped`.
Stopped(ids) ==
  /\ BarrierIgnoresBasis \/ basis >= floor
  /\ ids \cap (generation \cup running) = {}

\* Receiver: once the barrier reports the withdrawn agents stopped, the
\* applied row follows the floor.
Commit ==
  /\ applied < floor /\ fence = floor
  /\ Stopped(Assigned(applied) \ Assigned(floor))
  /\ applied' = floor
  /\ UNCHANGED <<floor, body, held, fence, queue, applying, generation, basis, running, crashes>>

Last ==
  IF queue # <<>> THEN queue[Len(queue)]
  ELSE IF applying # <<>> THEN applying[1]
  ELSE [agents |-> generation, basis |-> basis]

ProposalAgents ==
  IF ~held THEN Desired
  ELSE IF ~HoldWithdrawals /\ (Last.agents \ Desired # {} \/ floor # Last.basis)
    THEN Last.agents \cap Desired
  ELSE Last.agents

Reconcile ==
  /\ Len(queue) < 2
  /\ LET proposal == [agents |-> ProposalAgents, basis |-> floor] IN
       /\ proposal.agents # Last.agents \/ (~SendOnAgentsOnly /\ proposal.basis # Last.basis)
       /\ queue' = Append(queue, proposal)
  /\ UNCHANGED <<floor, applied, body, held, fence, applying, generation, basis, running, crashes>>

\* Lean `Placement.admitsProposal` and `Placement.proposalOutcome`. A dropped
\* proposal leaves `Last` older than the rows, so the watcher resolves again.
Admit ==
  /\ queue # <<>> /\ applying = <<>>
  /\ queue' = Tail(queue)
  /\ LET proposal == Head(queue) IN
       IF ~KeepStale /\ proposal.basis < fence THEN
         UNCHANGED <<applying, basis>>
       ELSE IF proposal.agents = generation THEN
         /\ basis' = IF NoopKeepsBasis THEN basis ELSE proposal.basis
         /\ UNCHANGED applying
       ELSE
         /\ applying' = <<proposal>>
         /\ UNCHANGED basis
  /\ UNCHANGED <<floor, applied, body, held, fence, generation, running, crashes>>

Publish ==
  /\ applying # <<>>
  /\ generation' = applying[1].agents /\ basis' = applying[1].basis /\ applying' = <<>>
  /\ UNCHANGED <<floor, applied, body, held, fence, queue, running, crashes>>

Start(id) ==
  /\ id \in generation \ running
  /\ running' = running \cup {id}
  /\ UNCHANGED <<floor, applied, body, held, fence, queue, applying, generation, basis, crashes>>

Retire(id) ==
  /\ id \in running \ generation
  /\ running' = running \ {id}
  /\ UNCHANGED <<floor, applied, body, held, fence, queue, applying, generation, basis, crashes>>

BodyArrives ==
  /\ "C" \notin body /\ body' = body \cup {"C"}
  /\ UNCHANGED <<floor, applied, held, fence, queue, applying, generation, basis, running, crashes>>

Hold ==
  /\ ~held /\ held' = TRUE
  /\ UNCHANGED <<floor, applied, body, fence, queue, applying, generation, basis, running, crashes>>

Release ==
  /\ held /\ held' = FALSE
  /\ UNCHANGED <<floor, applied, body, fence, queue, applying, generation, basis, running, crashes>>

Crash ==
  /\ crashes < MaxCrashes /\ crashes' = crashes + 1
  /\ running' = {} /\ generation' = {} /\ queue' = <<>> /\ applying' = <<>> /\ fence' = 0
  /\ floor' = IF CrashRestoresOlderFloor THEN applied ELSE floor
  /\ basis' = floor'
  /\ UNCHANGED <<applied, body, held>>

Step ==
  \/ Accept \/ Raise \/ Commit \/ Reconcile \/ Admit \/ Publish
  \/ \E id \in Ids: Start(id) \/ Retire(id)
  \/ BodyArrives \/ Hold \/ Release \/ Crash

Next == Step /\ Retired

Fairness ==
  /\ WF_vars(Raise /\ Retired)
  /\ WF_vars(Reconcile /\ Retired)
  /\ WF_vars(Admit /\ Retired)
  /\ WF_vars(Publish /\ Retired)
  /\ WF_vars(Retire("B") /\ Retired)

Spec == Init /\ [][Next]_vars /\ Fairness

\* Every running agent's body is verified.
VerifiedBody == running \subseteq body
\* Once stopped behind the fence, B stays out of every generation, across crashes.
NoRevival == retired => ~("B" \in generation \cup running)
\* The replacement starts only after the withdrawal it follows has stopped.
NoOverlap == ~(Ids \subseteq running)
\* A withdrawn agent stops, however long the installer holds the store.
WithdrawalStops == (floor = 1) ~> Stopped({"B"})
====
