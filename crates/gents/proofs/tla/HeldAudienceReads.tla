---- MODULE HeldAudienceReads ----
\* lean: none
\* crates: gents-store
\* models: Identity.HeldAudience.observe, Identity.HeldAudience.recovery
EXTENDS Naturals

\* The clones of one endpoint share one held audience, clock and client
\* authentication (`HeldAudienceCell`). Each reader is a request that read the
\* home's status: after its credential was refused (`recovers`), or before it
\* mints a request-bound delegation or dates a write (`request_bound_audience`,
\* `home_time`), or to learn the runtime's client authentication. A read takes
\* the cell's status lock (`Serialized`), sends a fresh nonce and receives the
\* status the home signs at that moment (`Fetch`), keeps it (`Apply`, the
\* `observe` of an authenticated status), releases the lock and mints its
\* credential (`Mint`): a resend or a request-bound delegation, from the cell
\* (`MintFromCell`) or from the status its own read returned. The home restarts
\* between reads (`Restart`, a new deployment), and a reader may be dropped
\* while it reads (`Cancel`); a dropped reader keeps nothing.
\*
\* A status is its home's `version` (one per deployment) and the provider's
\* time `at` the read was answered at, which orders the reads.
CONSTANTS Serialized, MintFromCell, MaxRestarts, Readers

VARIABLES home, tick, held, lock, pc, seen, minted, newest, restarts
vars == <<home, tick, held, lock, pc, seen, minted, newest, restarts>>

None == [version |-> 0, at |-> 0]
Statuses == [version : 0..(MaxRestarts + 1), at : Nat]
Free == "free"

TypeOK ==
  /\ home \in 1..(MaxRestarts + 1)
  /\ held \in Statuses
  /\ newest \in Statuses
  /\ lock \in Readers \cup {Free}
  /\ pc \in [Readers -> {"sending", "refused", "reading", "fetched", "applied", "minted", "dropped"}]
  /\ seen \in [Readers -> Statuses]
  /\ minted \in [Readers -> Statuses]

Init ==
  /\ home = 1
  /\ tick = 0
  /\ held = None
  /\ lock = Free
  /\ pc = [r \in Readers |-> "sending"]
  /\ seen = [r \in Readers |-> None]
  /\ minted = [r \in Readers |-> None]
  /\ newest = None
  /\ restarts = 0

Restart ==
  /\ restarts < MaxRestarts
  /\ home' = home + 1
  /\ restarts' = restarts + 1
  /\ UNCHANGED <<tick, held, lock, pc, seen, minted, newest>>

Refused(r) ==
  /\ pc[r] = "sending"
  /\ pc' = [pc EXCEPT ![r] = "refused"]
  /\ UNCHANGED <<home, tick, held, lock, seen, minted, newest, restarts>>

Acquire(r) ==
  /\ pc[r] = "refused"
  /\ IF Serialized THEN lock = Free /\ lock' = r ELSE UNCHANGED lock
  /\ pc' = [pc EXCEPT ![r] = "reading"]
  /\ UNCHANGED <<home, tick, held, seen, minted, newest, restarts>>

Fetch(r) ==
  /\ pc[r] = "reading"
  /\ tick' = tick + 1
  /\ seen' = [seen EXCEPT ![r] = [version |-> home, at |-> tick + 1]]
  /\ pc' = [pc EXCEPT ![r] = "fetched"]
  /\ UNCHANGED <<home, held, lock, minted, newest, restarts>>

Release(r) == IF lock = r THEN lock' = Free ELSE UNCHANGED lock

Apply(r) ==
  /\ pc[r] = "fetched"
  /\ held' = seen[r]
  /\ newest' = IF seen[r].at > newest.at THEN seen[r] ELSE newest
  /\ Release(r)
  /\ pc' = [pc EXCEPT ![r] = "applied"]
  /\ UNCHANGED <<home, tick, seen, minted, restarts>>

Cancel(r) ==
  /\ pc[r] \in {"reading", "fetched"}
  /\ Release(r)
  /\ pc' = [pc EXCEPT ![r] = "dropped"]
  /\ UNCHANGED <<home, tick, held, seen, minted, newest, restarts>>

Mint(r) ==
  /\ pc[r] = "applied"
  /\ minted' = [minted EXCEPT ![r] = IF MintFromCell THEN held ELSE seen[r]]
  /\ pc' = [pc EXCEPT ![r] = "minted"]
  /\ UNCHANGED <<home, tick, held, lock, seen, newest, restarts>>

Next ==
  \/ Restart
  \/ \E r \in Readers :
       Refused(r) \/ Acquire(r) \/ Fetch(r) \/ Apply(r) \/ Cancel(r) \/ Mint(r)

Reader(r) == Acquire(r) \/ Fetch(r) \/ Apply(r) \/ Mint(r)

Spec == Init /\ [][Next]_vars /\ \A r \in Readers : WF_vars(Reader(r))

\* A status kept is never one of an earlier deployment than the one held.
AudienceNeverOlder == [][held'.version >= held.version]_vars

\* A status kept is never one read before the one held: its clock, and the
\* client authentication it carries, do not go back.
ClockNeverOlder == [][held'.at >= held.at]_vars

\* A credential is minted for the newest status any read has kept.
MintsNewest == [][\A r \in Readers : pc[r] = "applied" /\ pc'[r] = "minted" => minted'[r] = newest]_vars

\* Two readers never read and keep a status at once.
ReadsDoNotOverlap ==
  \A r, s \in Readers : r # s => ~(pc[r] \in {"reading", "fetched"} /\ pc[s] \in {"reading", "fetched"})

\* Every refused request that is not dropped mints its credential.
EveryReaderMints == \A r \in Readers : pc[r] = "refused" ~> pc[r] \in {"minted", "dropped"}
====
