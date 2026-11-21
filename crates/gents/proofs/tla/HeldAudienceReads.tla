---- MODULE HeldAudienceReads ----
\* lean: Identity.HeldAudience.recordChange, Identity.HeldAudience.recovery
\* crates: gents-store
\* models: Identity.HeldAudience.observe, Identity.HeldAudience.changeSince
EXTENDS Naturals, LeanDecision_Identity_HeldAudience

\* The clones of one endpoint share one held audience, clock and client
\* authentication (`HeldAudienceCell`). Each reader is a request sent with a
\* credential minted from the record the cell held then (`Send`, its
\* `presented` record) and refused for it (`Refused`). It reads the home's
\* status: it takes the cell's status lock (`Serialized`), sends a fresh nonce
\* and receives the status the home signs at that moment (`Fetch`), keeps it
\* (`Apply`, the `observe` of an authenticated status) and decides from the
\* record it kept whether to send again (Lean `recovery`), releases the lock,
\* and either mints its resend (`Mint`), from the cell (`MintFromCell`) or from
\* the status its own read returned, or is told the refusal. The home restarts
\* under a new deployment (`Restart`) and its clock moves (`Reclock`) between
\* reads, and a reader may be dropped while it reads (`Cancel`); a dropped
\* reader keeps nothing. Reads before a request-bound delegation or a dated
\* write, and those that learn the runtime's client authentication, take the
\* same lock and keep the same way; they mint whatever they read.
\*
\* A status is its home's `version` (one per deployment, 0 for no audience),
\* its `clock`, and the provider's time `at` the read was answered at, which
\* orders the reads. A record is a status's version and clock.
\*
\* What a read reports to `recovery` (Lean `changeSince`): how the record kept
\* differs from the record the request presented (`ReportSincePresented`), or
\* only what this read changed in the cell.
CONSTANTS Serialized, MintFromCell, ReportSincePresented, MaxRestarts, MaxReclocks, Readers

VARIABLES home, clock, tick, held, lock, pc, seen, presented, outcome, minted, newest
vars == <<home, clock, tick, held, lock, pc, seen, presented, outcome, minted, newest>>

None == [version |-> 0, clock |-> 0, at |-> 0]
Statuses == [version : 0..(MaxRestarts + 1), clock : 0..MaxReclocks, at : Nat]
Records == [version : 0..(MaxRestarts + 1), clock : 0..MaxReclocks]
Free == "free"

Record(status) == [version |-> status.version, clock |-> status.clock]

\* Lean `Held.of`.
HeldOf(status) == IF status.version = 0 THEN "unknown" ELSE "known"

\* Lean `observe` of a status naming an audience: the record follows it when
\* it names another audience or its clock moved.
Observe(kept, status) ==
  IF status.version # kept.version \/ status.clock # kept.clock THEN status ELSE kept

\* Lean `changeSince`, through its decision `recordChange`.
ChangeSince(before, after) ==
  recordChange(HeldOf(before), HeldOf(after),
    before.version # 0 /\ after.version # 0 /\ before.version = after.version,
    before.version # 0 /\ after.version # 0 /\ before.clock = after.clock)

Stale(before, after) == Record(before) # Record(after)

TypeOK ==
  /\ home \in 1..(MaxRestarts + 1)
  /\ clock \in 0..MaxReclocks
  /\ held \in Statuses
  /\ newest \in Statuses
  /\ lock \in Readers \cup {Free}
  /\ pc \in [Readers -> {"sending", "sent", "refused", "reading", "fetched", "applied",
                         "minted", "told", "dropped"}]
  /\ seen \in [Readers -> Statuses]
  /\ presented \in [Readers -> Statuses]
  /\ outcome \in [Readers -> {"none", "resend", "tell", "unauthenticated"}]
  /\ minted \in [Readers -> Records]

Init ==
  /\ home = 1
  /\ clock = 0
  /\ tick = 0
  /\ held = None
  /\ lock = Free
  /\ pc = [r \in Readers |-> "sending"]
  /\ seen = [r \in Readers |-> None]
  /\ presented = [r \in Readers |-> None]
  /\ outcome = [r \in Readers |-> "none"]
  /\ minted = [r \in Readers |-> Record(None)]
  /\ newest = None

Restart ==
  /\ home < MaxRestarts + 1
  /\ home' = home + 1
  /\ UNCHANGED <<clock, tick, held, lock, pc, seen, presented, outcome, minted, newest>>

Reclock ==
  /\ clock < MaxReclocks
  /\ clock' = clock + 1
  /\ UNCHANGED <<home, tick, held, lock, pc, seen, presented, outcome, minted, newest>>

Send(r) ==
  /\ pc[r] = "sending"
  /\ presented' = [presented EXCEPT ![r] = held]
  /\ pc' = [pc EXCEPT ![r] = "sent"]
  /\ UNCHANGED <<home, clock, tick, held, lock, seen, outcome, minted, newest>>

Refused(r) ==
  /\ pc[r] = "sent"
  /\ pc' = [pc EXCEPT ![r] = "refused"]
  /\ UNCHANGED <<home, clock, tick, held, lock, seen, presented, outcome, minted, newest>>

Acquire(r) ==
  /\ pc[r] = "refused"
  /\ IF Serialized THEN lock = Free /\ lock' = r ELSE UNCHANGED lock
  /\ pc' = [pc EXCEPT ![r] = "reading"]
  /\ UNCHANGED <<home, clock, tick, held, seen, presented, outcome, minted, newest>>

Fetch(r) ==
  /\ pc[r] = "reading"
  /\ tick' = tick + 1
  /\ seen' = [seen EXCEPT ![r] = [version |-> home, clock |-> clock, at |-> tick + 1]]
  /\ pc' = [pc EXCEPT ![r] = "fetched"]
  /\ UNCHANGED <<home, clock, held, lock, presented, outcome, minted, newest>>

Release(r) == IF lock = r THEN lock' = Free ELSE UNCHANGED lock

Apply(r) ==
  /\ pc[r] = "fetched"
  /\ held' = Observe(held, seen[r])
  /\ newest' = IF seen[r].at > newest.at THEN seen[r] ELSE newest
  /\ outcome' = [outcome EXCEPT ![r] =
       recovery(FALSE, "delegation_refused", TRUE,
         IF ReportSincePresented THEN ChangeSince(presented[r], held')
         ELSE ChangeSince(held, held'))]
  /\ Release(r)
  /\ pc' = [pc EXCEPT ![r] = "applied"]
  /\ UNCHANGED <<home, clock, tick, seen, presented, minted>>

Cancel(r) ==
  /\ pc[r] \in {"reading", "fetched"}
  /\ Release(r)
  /\ pc' = [pc EXCEPT ![r] = "dropped"]
  /\ UNCHANGED <<home, clock, tick, held, seen, presented, outcome, minted, newest>>

Mint(r) ==
  /\ pc[r] = "applied"
  /\ IF outcome[r] = "resend"
       THEN /\ minted' = [minted EXCEPT ![r] = Record(IF MintFromCell THEN held ELSE seen[r])]
            /\ pc' = [pc EXCEPT ![r] = "minted"]
       ELSE /\ pc' = [pc EXCEPT ![r] = "told"]
            /\ UNCHANGED minted
  /\ UNCHANGED <<home, clock, tick, held, lock, seen, presented, outcome, newest>>

Next ==
  \/ Restart
  \/ Reclock
  \/ \E r \in Readers :
       Send(r) \/ Refused(r) \/ Acquire(r) \/ Fetch(r) \/ Apply(r) \/ Cancel(r) \/ Mint(r)

Reader(r) == Acquire(r) \/ Fetch(r) \/ Apply(r) \/ Mint(r)

Spec == Init /\ [][Next]_vars /\ \A r \in Readers : WF_vars(Reader(r))

\* A status kept is never one of an earlier deployment than the one held.
AudienceNeverOlder == [][held'.version >= held.version]_vars

\* A status kept is never one read before the one held: its clock, and the
\* client authentication it carries, do not go back.
ClockNeverOlder == [][held'.at >= held.at]_vars

\* A resend is minted for the newest status any read has kept.
MintsNewest ==
  [][\A r \in Readers : pc[r] = "applied" /\ pc'[r] = "minted" => minted'[r] = Record(newest)]_vars

\* Two readers never read and keep a status at once.
ReadsDoNotOverlap ==
  \A r, s \in Readers : r # s => ~(pc[r] \in {"reading", "fetched"} /\ pc[s] \in {"reading", "fetched"})

\* A refused request is sent again exactly when the record its client keeps
\* once its read is kept is not the one its credential was minted from,
\* whether its own read or a peer's read before it moved the record (#3963).
ResendsExactlyWhenStale ==
  [][\A r \in Readers :
       pc[r] = "fetched" /\ pc'[r] = "applied"
         => (outcome'[r] = "resend" <=> Stale(presented[r], held'))]_vars

\* Every refused request that is not dropped is sent again or told.
EveryReaderSettles == \A r \in Readers : pc[r] = "refused" ~> pc[r] \in {"minted", "told", "dropped"}
====
