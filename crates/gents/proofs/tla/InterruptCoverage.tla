---- MODULE InterruptCoverage ----
\* lean: none
\* crates: gents-session
\* models: SessionQueue.InterruptGeneration.bump, SessionQueue.InterruptGeneration.runningInterrupted, SessionQueue.InterruptGeneration.wakeAtClaim, SessionQueue.InterruptGeneration.only_another_interrupt_aborts_an_interrupt, SessionQueue.InterruptGeneration.an_interrupt_lost_to_another_is_passed_by_it, TransactionFence.fencedAgainst
\* traces: violates
\* An interrupt of a session racing the claim of its pending request, the
\* wakes published into it, and the writes a running turn makes.
\*
\* The session's interrupt marker carries a generation. The interrupt reads
\* it by id and writes one past it, in one transaction. A claim and a wake publication each
\* read the marker by id when `ClaimReadsMarker` / `WakeReadsMarker` and
\* record the generation on the row they write; a marker written after that
\* read aborts their commit and they run again. A claimed request whose
\* recorded generation is below the marker is interrupted; a wake whose
\* recorded generation is below the marker when claimed is drained
\* (Lean `InterruptGeneration`). A row that recorded nothing is never
\* compared.
\*
\* The interrupt runs at most `Attempts` attempts
\* (`TRANSACT_CONFLICT_MAX_RETRIES` + 1). In production it reads only its
\* marker, found by the session key the marker carries, so claims, wakes and
\* `OtherWrites` (terminals, titles, stops: writes of the session document)
\* commit beside it, and only `OtherInterrupts` (other interrupts of the
\* session, which bump the marker) abort an attempt; each such interrupt
\* passes the generation the lost attempt read. `InterruptReads` is the
\* safeguard removed: the interrupt reads the session document, so every
\* write of it aborts an open attempt. Requests' own rows, which renewals
\* write, are read by no session interrupt.
\*
\* Covered: the request active at the interrupt's commit and the wakes
\* pending at it. A request claimed and a wake published after the commit
\* are new turns.
\* trace actors: WakeRead => wakeSeen
\* trace actors: ClaimRead => claimSeen
\* trace actors: WakeCommit => wakeRec|wakeSeen
\* trace actors: ClaimCommit => claimRec|claimSeen
EXTENDS Integers, FiniteSets

CONSTANTS
  Wakes, Requests, OtherWrites, OtherInterrupts, Attempts,
  ClaimReadsMarker, WakeReadsMarker, InterruptReads

None == -1
NoRequest == "none"

ASSUME /\ OtherWrites \in Nat /\ OtherInterrupts \in Nat /\ Attempts \in Nat \ {0}
       /\ ClaimReadsMarker \in BOOLEAN /\ WakeReadsMarker \in BOOLEAN
       /\ InterruptReads \in BOOLEAN

VARIABLES
  gen,          \* the marker's generation
  sessionVer,   \* version of the session document
  others,       \* other session writes still to land
  rivals,       \* other interrupts of the session still to land
  istate,       \* "idle" | "open" | "done" | "failed"
  iread,        \* <<generation, session version>> the open attempt read
  attempts,     \* interrupt attempts aborted
  wakeSeen,     \* wake -> generation its open publication read, or None
  wakeRec,      \* published wake -> recorded generation or None
  claimSeen,    \* request -> generation its open claim read, or None
  claimRec,     \* claimed request -> recorded generation or None
  active,       \* the running request or NoRequest
  coveredReq,   \* request active at the interrupt's commit, or NoRequest
  coveredWakes, \* wakes pending at the interrupt's commit
  later         \* rows committed after the interrupt's commit

vars == <<gen, sessionVer, others, rivals, istate, iread, attempts, wakeSeen, wakeRec,
          claimSeen, claimRec, active, coveredReq, coveredWakes, later>>

Init ==
  /\ gen = 0 /\ sessionVer = 0 /\ others = OtherWrites /\ rivals = OtherInterrupts
  /\ istate = "idle" /\ iread = <<0, 0>> /\ attempts = 0
  /\ wakeSeen = [w \in Wakes |-> None] /\ wakeRec = [w \in {} |-> None]
  /\ claimSeen = [r \in Requests |-> None] /\ claimRec = [r \in {} |-> None]
  /\ active = NoRequest /\ coveredReq = NoRequest /\ coveredWakes = {} /\ later = {}

Recorded(seen, reads) == IF reads THEN seen ELSE None
Stale(seen, reads) == reads /\ seen # gen
AfterCommit(x) == IF istate = "done" THEN later \cup {x} ELSE later

WakeRead(w) ==
  /\ w \notin DOMAIN wakeRec /\ wakeSeen[w] = None
  /\ wakeSeen' = [wakeSeen EXCEPT ![w] = gen]
  /\ UNCHANGED <<gen, sessionVer, others, rivals, istate, iread, attempts, wakeRec,
                 claimSeen, claimRec, active, coveredReq, coveredWakes, later>>

WakeCommit(w) ==
  /\ w \notin DOMAIN wakeRec /\ wakeSeen[w] # None
  /\ IF Stale(wakeSeen[w], WakeReadsMarker)
       THEN /\ wakeSeen' = [wakeSeen EXCEPT ![w] = None]
            /\ UNCHANGED <<wakeRec, sessionVer, later>>
       ELSE /\ wakeRec' = [x \in DOMAIN wakeRec \cup {w} |->
                             IF x = w THEN Recorded(wakeSeen[w], WakeReadsMarker) ELSE wakeRec[x]]
            /\ sessionVer' = sessionVer + 1
            /\ later' = AfterCommit(w)
            /\ UNCHANGED wakeSeen
  /\ UNCHANGED <<gen, others, rivals, istate, iread, attempts, claimSeen, claimRec,
                 active, coveredReq, coveredWakes>>

ClaimRead(r) ==
  /\ r \notin DOMAIN claimRec /\ active = NoRequest /\ claimSeen[r] = None
  /\ claimSeen' = [claimSeen EXCEPT ![r] = gen]
  /\ UNCHANGED <<gen, sessionVer, others, rivals, istate, iread, attempts, wakeSeen,
                 wakeRec, claimRec, active, coveredReq, coveredWakes, later>>

ClaimCommit(r) ==
  /\ r \notin DOMAIN claimRec /\ active = NoRequest /\ claimSeen[r] # None
  /\ IF Stale(claimSeen[r], ClaimReadsMarker)
       THEN /\ claimSeen' = [claimSeen EXCEPT ![r] = None]
            /\ UNCHANGED <<claimRec, active, sessionVer, later>>
       ELSE /\ claimRec' = [x \in DOMAIN claimRec \cup {r} |->
                              IF x = r THEN Recorded(claimSeen[r], ClaimReadsMarker) ELSE claimRec[x]]
            /\ active' = r
            /\ sessionVer' = sessionVer + 1
            /\ later' = AfterCommit(r)
            /\ UNCHANGED claimSeen
  /\ UNCHANGED <<gen, others, rivals, istate, iread, attempts, wakeSeen, wakeRec,
                 coveredReq, coveredWakes>>

OtherWrite ==
  /\ others > 0 /\ others' = others - 1 /\ sessionVer' = sessionVer + 1
  /\ UNCHANGED <<gen, rivals, istate, iread, attempts, wakeSeen, wakeRec, claimSeen,
                 claimRec, active, coveredReq, coveredWakes, later>>

OtherInterrupt ==
  /\ istate # "done" /\ rivals > 0 /\ rivals' = rivals - 1 /\ gen' = gen + 1
  /\ UNCHANGED <<sessionVer, others, istate, iread, attempts, wakeSeen, wakeRec,
                 claimSeen, claimRec, active, coveredReq, coveredWakes, later>>

Begin ==
  /\ istate = "idle" /\ istate' = "open" /\ iread' = <<gen, sessionVer>>
  /\ UNCHANGED <<gen, sessionVer, others, rivals, attempts, wakeSeen, wakeRec,
                 claimSeen, claimRec, active, coveredReq, coveredWakes, later>>

Aborted == \/ iread[1] # gen
           \/ InterruptReads /\ iread[2] # sessionVer

Abort ==
  /\ istate = "open" /\ Aborted
  /\ attempts' = attempts + 1
  /\ istate' = IF attempts + 1 >= Attempts THEN "failed" ELSE "idle"
  /\ UNCHANGED <<gen, sessionVer, others, rivals, iread, wakeSeen, wakeRec, claimSeen,
                 claimRec, active, coveredReq, coveredWakes, later>>

Commit ==
  /\ istate = "open" /\ ~Aborted
  /\ istate' = "done" /\ gen' = gen + 1
  /\ coveredReq' = active
  /\ coveredWakes' = DOMAIN wakeRec
  /\ UNCHANGED <<sessionVer, others, rivals, iread, attempts, wakeSeen, wakeRec,
                 claimSeen, claimRec, active, later>>

Next ==
  \/ \E w \in Wakes : WakeRead(w) \/ WakeCommit(w)
  \/ \E r \in Requests : ClaimRead(r) \/ ClaimCommit(r)
  \/ OtherWrite \/ OtherInterrupt \/ Begin \/ Abort \/ Commit

Spec == Init /\ [][Next]_vars /\ WF_vars(Begin) /\ WF_vars(Abort) /\ WF_vars(Commit)

Interrupted(rec) == rec # None /\ rec < gen

\* What the interrupt covered does not run: the request active at its commit
\* is interrupted and every wake pending at it is drained.
InterruptedRequestDoesNotRun ==
  istate = "done" =>
    /\ coveredReq # NoRequest => Interrupted(claimRec[coveredReq])
    /\ \A w \in coveredWakes : Interrupted(wakeRec[w])

\* A request claimed or a wake published after the commit is a new turn.
LaterArrivalsAreNewTurns ==
  /\ \A r \in later \cap DOMAIN claimRec : ~Interrupted(claimRec[r])
  /\ \A w \in later \cap DOMAIN wakeRec : ~Interrupted(wakeRec[w])

\* No interleaving of claims, wakes and other writes makes the interrupt fail.
InterruptNotPrevented == istate # "failed"

\* An interrupt that fails lost every attempt to another interrupt of the
\* session, whose generation passes what the lost attempts read.
OnlyInterruptsDefeatAnInterrupt == istate = "failed" => gen >= Attempts

InterruptCommits == <>(istate = "done")
====
