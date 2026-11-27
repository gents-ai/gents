---- MODULE InterruptLatchContention ----
\* lean: none
\* crates: gents-session
\* traces: violates
\* models: TransactionReplay.disposition
\* A User's interrupt of a running turn, by request id, racing the turn's own
\* writes to the request document.
\*
\* Interrupt (`gents_session::operations::request::interrupt`, desktop
\* `interrupt_request`): the operator's client-signed transport runs one
\* DefraDB transaction, `interrupt.request`
\* (`interrupt_request_by_doc_id_with_access`). Its callback
\* (`interrupt_request_matching_in_txn`) reads the `AgentRequest` row by id
\* and the session's interrupt marker by id, stages the session-halt hook,
\* and, unless the marker already passed the generation the request's claim
\* recorded, sends the User-signed update that bumps the marker
\* (Lean `SessionQueue.InterruptGeneration`). The request is read by id so
\* that a request that settled meanwhile aborts the interrupt instead of
\* letting it pass a request claimed after it. `ver` counts the writes of
\* either document; `stamp` is whether the marker passed the claim. The
\* commit aborts with a DefraDB conflict when any write of either document
\* landed after the read. The transaction owner then decides by Lean
\* `TransactionReplay.disposition`: a client-signed write already sent is
\* rebuilt only in `signedConflictRebuild` mode after a confirmed abort
\* (`sent_signed_write_repeats_only_a_confirmed_conflict_rebuild`). `Redrive`
\* selects that mode; FALSE is `transact_write` (`conflictRetry`), where the
\* first conflict fails the interrupt.
\*
\* Writers of those documents while the turn runs: the execution lease
\* renewal (`renew_once_with_time`, a CAS of the lease deadline at each due
\* poll) and any other interrupter (the runtime's own `interrupt_request`, a
\* background job's caused interrupt, a session interrupt), which bumps the
\* marker first. A session interrupt reads no request and is not delayed by
\* renewals; only an interrupt by request id rests on this bound. Under
\* the renewal cadence premise (Lean `consecutive_renewals_obey_cadence`,
\* `BackgroundAdmissionContention.tla`) fewer such writes land in one attempt
\* series than it has attempts; `Writes` bounds them.
\*
\* External premise: DefraDB's HTTP commit answers a conflict with an
\* aborted outcome (`transaction_conflict_aborted`), which the owner takes as
\* the confirmed abort a rebuild needs.
\*
\* REDs: `SingleAttempt` fails the interrupt on the first conflict (the
\* desktop's "DefraDB transaction conflict" on interrupt); `StaleRebuild`
\* rebuilds from the first attempt's read, which bumps a marker another
\* interrupter already passed and breaks the receipt's timestamp; `Exhausted` drops the
\* cadence premise and shows the bound is what the guarantee rests on.
EXTENDS Naturals

CONSTANTS
  Writes,          \* renewals landing in the latch's attempt series
  OtherInterrupt,  \* another interrupter may latch first
  Attempts,        \* TRANSACT_CONFLICT_MAX_RETRIES + 1
  Redrive,         \* signed-conflict rebuild of the latch
  Reread           \* a rebuilt attempt reads the row afresh

ASSUME /\ Writes \in Nat
       /\ OtherInterrupt \in BOOLEAN
       /\ Attempts \in Nat \ {0}
       /\ Redrive \in BOOLEAN
       /\ Reread \in BOOLEAN

VARIABLES
  ver,        \* committed version of the AgentRequest document
  writesLeft, \* renewals still to land
  otherLeft,  \* the other interrupter has yet to run
  stamp,      \* "none" | "user" | "other": whose bump passed the claim
  stamps,     \* bumps that passed the claim
  latch,      \* "idle" | "open" | "done" | "failed"
  snap,       \* version the open attempt read
  seen,       \* the latch value the open attempt read
  tries,      \* attempts begun
  conflictLost

vars == <<ver, writesLeft, otherLeft, stamp, stamps, latch, snap, seen, tries, conflictLost>>

Init ==
  /\ ver = 0
  /\ writesLeft = Writes
  /\ otherLeft = OtherInterrupt
  /\ stamp = "none"
  /\ stamps = 0
  /\ latch = "idle"
  /\ snap = 0
  /\ seen = "none"
  /\ tries = 0
  /\ conflictLost = FALSE

\* The working Agent's lease renewal CASes the request deadline.
Renew ==
  /\ writesLeft > 0
  /\ ver' = ver + 1
  /\ writesLeft' = writesLeft - 1
  /\ UNCHANGED <<otherLeft, stamp, stamps, latch, snap, seen, tries, conflictLost>>

\* Another interrupter bumps the marker in its own transaction; a marker that
\* passed the claim is kept.
OtherLatch ==
  /\ otherLeft
  /\ otherLeft' = FALSE
  /\ IF stamp = "none"
       THEN /\ stamp' = "other"
            /\ stamps' = stamps + 1
            /\ ver' = ver + 1
       ELSE UNCHANGED <<stamp, stamps, ver>>
  /\ UNCHANGED <<writesLeft, latch, snap, seen, tries, conflictLost>>

\* The User's interrupt begins its first attempt and reads the row.
Open ==
  /\ latch = "idle"
  /\ latch' = "open"
  /\ snap' = ver
  /\ seen' = stamp
  /\ tries' = 1
  /\ UNCHANGED <<ver, writesLeft, otherLeft, stamp, stamps, conflictLost>>

\* Commit: lands when the read documents are unchanged, bumping only a marker
\* that has not passed the claim. A conflict is rebuilt from a fresh read while attempts remain in
\* signed-conflict rebuild; otherwise the interrupt fails back to the User.
Commit ==
  /\ latch = "open"
  /\ IF snap = ver
       THEN /\ latch' = "done"
            /\ IF seen = "none"
                 THEN /\ stamp' = "user"
                      /\ stamps' = stamps + 1
                      /\ ver' = ver + 1
                 ELSE UNCHANGED <<stamp, stamps, ver>>
            /\ UNCHANGED <<snap, seen, tries, conflictLost>>
       ELSE IF Redrive /\ tries < Attempts
              THEN /\ snap' = ver
                   /\ seen' = IF Reread THEN stamp ELSE seen
                   /\ tries' = tries + 1
                   /\ UNCHANGED <<latch, ver, stamp, stamps, conflictLost>>
              ELSE /\ latch' = "failed"
                   /\ conflictLost' = TRUE
                   /\ UNCHANGED <<ver, stamp, stamps, snap, seen, tries>>
  /\ UNCHANGED <<writesLeft, otherLeft>>

Next == Renew \/ OtherLatch \/ Open \/ Commit

Spec == Init /\ [][Next]_vars /\ WF_vars(Open) /\ WF_vars(Commit)

TypeOK ==
  /\ ver \in 0..(Writes + 2)
  /\ writesLeft \in 0..Writes
  /\ otherLeft \in BOOLEAN
  /\ stamp \in {"none", "user", "other"}
  /\ stamps \in 0..2
  /\ latch \in {"idle", "open", "done", "failed"}
  /\ seen \in {"none", "user", "other"}
  /\ tries \in 0..Attempts
  /\ conflictLost \in BOOLEAN

\* An interrupt during a turn is never lost to the turn's own writes.
NoConflictLoss == ~conflictLost

\* The marker passes the claim once: a later interrupt keeps the first
\* timestamp, which the receipt reports.
StampedOnce == stamps <= 1

\* An accepted interrupt leaves the marker past the claim.
DoneIsLatched == latch = "done" => stamp # "none"

\* The interrupt settles.
InterruptSettles == <>(latch \in {"done", "failed"})

====
