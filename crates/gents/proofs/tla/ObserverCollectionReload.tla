---- MODULE ObserverCollectionReload ----
\* lean: ClientDataPublication.reloadAcceptedSince, SessionRequestReload.derivesAlone
\* crates: gents-observe
\* models: ObserverResync.deliver, ObserverResync.start, ObserverResync.collectionStartable, ObserverResync.startCollections, ObserverResync.collectionSettlement, ObserverResync.settleCollections
\* The desktop observer's partial reads beside its full node reloads
\* (`observe.rs`, `observe/delivery.rs`, `observe/projection_store.rs`). One
\* node: per-node independence is `ObserverResync`'s.
\*
\* Runtime. The node's runtime commits projected writes to three parts of the
\* snapshot: "cfg", a collection whose snapshot root is one self-contained
\* view (a collection reload reads exactly its rows); "ses", the session row
\* of one session scope and its requests that no trigger caused (a read of the
\* scope shows what a full load shows for it, Lean
\* `SessionRequestReload.session_requests_derive_alone`); and "req", whose
\* rows depend on other roots (a trigger, the Node, a request a trigger
\* caused). DefraDB delivers a change event for a commit only on a
\* subscription armed before it, after the commit is visible to a new read; a
\* dropped stream loses every undelivered event. `Touch` is a "cfg" rewrite
\* with no projected change (the per-minute catalog `observed_at` rewrite of
\* an inference backend observation); `Renew` is a "req" write the projection
\* does not show, at most `K` per full load; `RenewScope` is such a write of
\* a "ses" request (an execution-lease renewal), at most `ScopeK` per partial
\* read.
\* `Trigger` writes a trigger-caused request into the session scope, which
\* stays in it as a trigger candidate: a full load shows it, a read of the
\* scope does not.
\*
\* Delivery (Lean `ObserverResync.deliver`). Every delivered change is a
\* request event of the node's store. A "cfg" change queues the collection
\* and a "ses" change its session scope for the partial read; a "req" change,
\* which a trigger-caused request's is, queues the node's full reload.
\*
\* Observer. Per node at most one read is in flight (`OneReadPerNode`): the
\* full reload or one partial read. A full reload starts when queued
\* (`ObserverResync.start`) and takes the queued parts with it: its read
\* begins after the events that queued them. A partial read starts only when
\* no full reload is queued (`ObserverResync.collectionStartable`,
\* `startCollections`) and reads every queued part. Each read captures the
\* store fence, reads the runtime, then settles
\* (`ObserverResync.collectionSettlement`, `settleCollections`): a partial
\* read of a scope the store holds a trigger-caused request for publishes
\* nothing and queues the full reload (`TriggeredDefers`, Lean
\* `SessionRequestReload.derivesAlone`); an accepted one replaces only its
\* parts' rows; a rejected one queues its parts again (`RequeueRejected`); a
\* failed one queues the full reload, which owns status, back-off and refusal
\* (`FailureResyncs`).
\*
\* Fences. Every publication (full reload, partial read, direct refresh)
\* moves the store fence; a request event does not. Both are Lean
\* `ClientDataPublication.reloadAcceptedSince`, read from the generated table;
\* `CollectionsFenced` FALSE lets a partial read publish through a moved
\* fence, `PartialMovesFence` FALSE lets it publish without moving the fence,
\* and `EventFence` TRUE makes a request event reject it, as it rejects a
\* patch (`ClientDataPublication.accepts`).
\*
\* Direct refresh. A user mutation reads the node once and publishes every
\* part through the fence; a rejected refresh queues the full reload.
\*
\* Each boolean constant but `EventFence` is TRUE in the fixed design and
\* breaks one guard; `EventFence` is FALSE in it.
EXTENDS Naturals, LeanDecision_ClientDataPublication, LeanDecision_SessionRequestReload

CONSTANTS
  MaxWrites,         \* projected writes to "cfg" and to "req"
  MaxScopeWrites,    \* projected writes to "ses"
  K,                 \* touches and renewals per full load
  ScopeK,            \* scope renewals per partial read
  MaxDrops,          \* stream drops
  MaxRefreshes,      \* user mutations that refresh the node directly
  MaxFailures,       \* partial reads that fail
  MaxTriggers,       \* trigger-caused requests written into the session scope
  CollectionsFenced, \* a partial read publishes only through its fence
  RequeueRejected,   \* a rejected partial read queues its parts again
  FailureResyncs,    \* a failed partial read queues the full reload
  OneReadPerNode,    \* a partial read never overlaps a full reload
  PartialMovesFence, \* a published partial read moves the store fence
  TriggeredDefers,   \* a scope holding a trigger-caused request waits for the full reload
  EventFence         \* a request event rejects a partial read in flight

ASSUME \A flag \in {CollectionsFenced, RequeueRejected, FailureResyncs, OneReadPerNode,
                    PartialMovesFence, TriggeredDefers, EventFence} :
         flag \in BOOLEAN

Collections == {"cfg", "ses", "req"}
\* The parts a partial read reads alone.
Partial == {"cfg", "ses"}

VARIABLES
  db,            \* part -> projected version committed on the runtime
  dbCand,        \* the session scope holds a trigger candidate on the runtime
  undelivered,   \* part -> a committed change whose event is not delivered
  stream,        \* "arming" | "armed"
  drops,         \* stream drops so far
  touches,       \* "cfg" touches since the last full load started
  renews,        \* "req" renewals during the current full load
  scopeRenews,   \* "ses" renewals during the current partial read
  failures,      \* failed partial reads so far
  pending,       \* the full reload is queued
  queued,        \* the parts queued for the partial read
  full,          \* "idle" | "captured" | "read": the full reload
  fullFenced,    \* the full reload's fence moved since its capture
  fullRead,      \* part -> version the full reload read
  fullCand,      \* the full reload read the trigger candidate
  coll,          \* "idle" | "captured" | "read": the partial read
  collParts,     \* the parts the partial read reads
  collFenced,    \* the partial read's fence moved since its capture
  collRead,      \* part -> version the partial read read
  refresh,       \* "idle" | "captured" | "read": the direct refresh
  refreshFenced, \* the refresh's fence moved since its capture
  refreshRead,   \* part -> version the refresh read
  refreshCand,   \* the refresh read the trigger candidate
  refreshes,     \* user mutations so far
  store,         \* part -> projected version the desktop shows
  storeCand,     \* the desktop shows the session scope's trigger candidate
  outcome        \* "none" | "published" | "rejected": the last full reload

runtimeVars == <<db, dbCand>>
countVars == <<drops, touches, renews, failures>>
fullVars == <<full, fullFenced, fullRead, fullCand>>
collVars == <<coll, collParts, collFenced, collRead, scopeRenews>>
refreshVars == <<refresh, refreshFenced, refreshRead, refreshCand, refreshes>>
shown == <<store, storeCand>>

vars == <<runtimeVars, undelivered, stream, countVars, pending, queued, fullVars,
          collVars, refreshVars, shown, outcome>>

\* Whether a request event, or a publication, since a capture rejects a reload.
EventMovesReloads == ~reloadAcceptedSince(FALSE, TRUE)
PublicationMovesReloads == ~reloadAcceptedSince(TRUE, FALSE)
\* Whether a request event since its capture rejects a partial read.
EventMovesPartial == EventMovesReloads \/ EventFence
\* Whether a published partial read rejects the reloads in flight.
PartialMovesReloads == PartialMovesFence /\ PublicationMovesReloads

Init ==
  /\ db = [c \in Collections |-> 0]
  /\ dbCand = FALSE
  /\ undelivered = [c \in Collections |-> FALSE]
  /\ stream = "arming"
  /\ drops = 0
  /\ touches = 0
  /\ renews = 0
  /\ scopeRenews = 0
  /\ failures = 0
  /\ pending = FALSE
  /\ queued = {}
  /\ full = "idle"
  /\ fullFenced = FALSE
  /\ fullRead = [c \in Collections |-> 0]
  /\ fullCand = FALSE
  /\ coll = "idle"
  /\ collParts = {}
  /\ collFenced = FALSE
  /\ collRead = [c \in Partial |-> 0]
  /\ refresh = "idle"
  /\ refreshFenced = FALSE
  /\ refreshRead = [c \in Collections |-> 0]
  /\ refreshCand = FALSE
  /\ refreshes = 0
  /\ store = [c \in Collections |-> 0]
  /\ storeCand = FALSE
  /\ outcome = "none"

----
\* Runtime.

WriteBound(c) == IF c = "ses" THEN MaxScopeWrites ELSE MaxWrites

Write(c) ==
  /\ db[c] < WriteBound(c)
  /\ db' = [db EXCEPT ![c] = @ + 1]
  /\ undelivered' = [undelivered EXCEPT ![c] = @ \/ stream = "armed"]
  /\ UNCHANGED <<dbCand, stream, countVars, pending, queued, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

\* A trigger-caused request in the session scope: a node change.
Trigger ==
  /\ MaxTriggers > 0
  /\ ~dbCand
  /\ dbCand' = TRUE
  /\ undelivered' = [undelivered EXCEPT !["req"] = @ \/ stream = "armed"]
  /\ UNCHANGED <<db, stream, countVars, pending, queued, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

Touch ==
  /\ stream = "armed"
  /\ touches < K
  /\ touches' = touches + 1
  /\ undelivered' = [undelivered EXCEPT !["cfg"] = TRUE]
  /\ UNCHANGED <<runtimeVars, stream, drops, renews, failures, pending, queued,
                 shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

Renew ==
  /\ stream = "armed"
  /\ full # "idle"
  /\ renews < K
  /\ renews' = renews + 1
  /\ undelivered' = [undelivered EXCEPT !["req"] = TRUE]
  /\ UNCHANGED <<runtimeVars, stream, drops, touches, failures, pending, queued,
                 shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

RenewScope ==
  /\ stream = "armed"
  /\ coll # "idle"
  /\ scopeRenews < ScopeK
  /\ scopeRenews' = scopeRenews + 1
  /\ undelivered' = [undelivered EXCEPT !["ses"] = TRUE]
  /\ UNCHANGED <<runtimeVars, stream, countVars, pending, queued, shown, outcome,
                 coll, collParts, collFenced, collRead>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

\* A drop loses undelivered events and aborts both reads; the re-arm queues
\* the full reload. The reconnect is a request event of the store.
Drop ==
  /\ stream = "armed"
  /\ drops < MaxDrops
  /\ stream' = "arming"
  /\ drops' = drops + 1
  /\ undelivered' = [c \in Collections |-> FALSE]
  /\ full' = "idle"
  /\ coll' = "idle"
  /\ refreshFenced' = (refreshFenced \/ EventMovesReloads)
  /\ UNCHANGED <<runtimeVars, touches, renews, failures, pending, queued,
                 fullFenced, fullRead, fullCand, collParts, collFenced, collRead,
                 scopeRenews, refresh, refreshRead, refreshCand, refreshes, shown,
                 outcome>>

RefreshBegin ==
  /\ refresh = "idle"
  /\ refreshes < MaxRefreshes
  /\ refreshes' = refreshes + 1
  /\ refresh' = "captured"
  /\ refreshFenced' = FALSE
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, pending, queued,
                 refreshRead, refreshCand, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars

----
\* Delivery (`observe/delivery.rs`, Lean `ObserverResync.deliver`): a request
\* event of the store, then the partial read's queue or the node's.

Deliver(c) ==
  /\ stream = "armed"
  /\ undelivered[c]
  /\ undelivered' = [undelivered EXCEPT ![c] = FALSE]
  /\ fullFenced' = (fullFenced \/ EventMovesReloads)
  /\ collFenced' = (collFenced \/ EventMovesPartial)
  /\ refreshFenced' = (refreshFenced \/ EventMovesReloads)
  /\ IF c \in Partial
       THEN queued' = queued \cup {c} /\ UNCHANGED pending
       ELSE pending' = TRUE /\ UNCHANGED queued
  /\ UNCHANGED <<runtimeVars, stream, countVars, full, fullRead, fullCand, coll,
                 collParts, collRead, scopeRenews, refresh, refreshRead,
                 refreshCand, refreshes, shown, outcome>>

\* Every root armed: `SubscriptionsReady` queues the full reload.
Arm ==
  /\ stream = "arming"
  /\ stream' = "armed"
  /\ pending' = TRUE
  /\ UNCHANGED <<runtimeVars, undelivered, countVars, queued, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars /\ UNCHANGED refreshVars

----
\* Full reload (`ObserverResync.start`, `settle`).

StartFull ==
  /\ pending /\ full = "idle" /\ stream = "armed"
  /\ coll = "idle" \/ ~OneReadPerNode
  /\ pending' = FALSE
  /\ queued' = {}
  /\ full' = "captured"
  /\ fullFenced' = FALSE
  /\ touches' = 0
  /\ renews' = 0
  /\ UNCHANGED <<runtimeVars, undelivered, stream, drops, failures, fullRead,
                 fullCand, shown, outcome>>
  /\ UNCHANGED collVars /\ UNCHANGED refreshVars

ReadFull ==
  /\ full = "captured"
  /\ full' = "read"
  /\ fullRead' = db
  /\ fullCand' = dbCand
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, pending, queued,
                 fullFenced, shown, outcome>>
  /\ UNCHANGED collVars /\ UNCHANGED refreshVars

PublishFull ==
  /\ full = "read"
  /\ full' = "idle"
  /\ IF fullFenced
       THEN /\ pending' = TRUE
            /\ outcome' = "rejected"
            /\ UNCHANGED <<shown, collFenced, refreshFenced>>
       ELSE /\ store' = fullRead
            /\ storeCand' = fullCand
            /\ outcome' = "published"
            /\ collFenced' = (collFenced \/ PublicationMovesReloads)
            /\ refreshFenced' = (refreshFenced \/ PublicationMovesReloads)
            /\ UNCHANGED pending
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, queued, fullFenced,
                 fullRead, fullCand, coll, collParts, collRead, scopeRenews,
                 refresh, refreshRead, refreshCand, refreshes>>

----
\* Partial read (`ObserverResync.collectionStartable`, `startCollections`,
\* `collectionSettlement`, `settleCollections`).

StartColl ==
  /\ queued # {} /\ ~pending /\ coll = "idle" /\ stream = "armed"
  /\ full = "idle" \/ ~OneReadPerNode
  /\ queued' = {}
  /\ coll' = "captured"
  /\ collParts' = queued
  /\ collFenced' = FALSE
  /\ scopeRenews' = 0
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, pending, collRead,
                 shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

ReadColl ==
  /\ coll = "captured"
  /\ coll' = "read"
  /\ collRead' = [c \in Partial |-> db[c]]
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, pending, queued,
                 collParts, collFenced, scopeRenews, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

\* A transport failure or a refusal of the partial read.
FailColl ==
  /\ coll # "idle"
  /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ coll' = "idle"
  /\ pending' = (pending \/ FailureResyncs)
  /\ UNCHANGED <<runtimeVars, undelivered, stream, drops, touches, renews, queued,
                 collParts, collFenced, collRead, scopeRenews, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED refreshVars

\* The read of the scope returns its non-terminal requests, its head and
\* their closure: never the trigger candidate, which only the store holds.
ScopeDerivesAlone == ~TriggeredDefers \/ derivesAlone(storeCand, FALSE)

PublishColl ==
  /\ coll = "read"
  /\ coll' = "idle"
  /\ IF "ses" \in collParts /\ ~ScopeDerivesAlone
       THEN /\ pending' = TRUE
            /\ UNCHANGED <<queued, shown, fullFenced, refreshFenced>>
       ELSE IF collFenced /\ CollectionsFenced
         THEN /\ queued' = (queued \cup IF RequeueRejected THEN collParts ELSE {})
              /\ UNCHANGED <<pending, shown, fullFenced, refreshFenced>>
         ELSE /\ store' = [c \in Collections |->
                            IF c \in collParts THEN collRead[c] ELSE store[c]]
              /\ storeCand' = (storeCand /\ "ses" \notin collParts)
              /\ fullFenced' = (fullFenced \/ PartialMovesReloads)
              /\ refreshFenced' = (refreshFenced \/ PartialMovesReloads)
              /\ UNCHANGED <<pending, queued>>
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, full, fullRead,
                 fullCand, collParts, collFenced, collRead, scopeRenews, refresh,
                 refreshRead, refreshCand, refreshes, outcome>>

----
\* Direct refresh after a user mutation.

RefreshRead ==
  /\ refresh = "captured"
  /\ refresh' = "read"
  /\ refreshRead' = db
  /\ refreshCand' = dbCand
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, pending, queued,
                 refreshFenced, refreshes, shown, outcome>>
  /\ UNCHANGED fullVars /\ UNCHANGED collVars

RefreshPublish ==
  /\ refresh = "read"
  /\ refresh' = "idle"
  /\ IF refreshFenced
       THEN /\ pending' = TRUE
            /\ UNCHANGED <<shown, fullFenced, collFenced>>
       ELSE /\ store' = refreshRead
            /\ storeCand' = refreshCand
            /\ fullFenced' = (fullFenced \/ PublicationMovesReloads)
            /\ collFenced' = (collFenced \/ PublicationMovesReloads)
            /\ UNCHANGED pending
  /\ UNCHANGED <<runtimeVars, undelivered, stream, countVars, queued, full,
                 fullRead, fullCand, coll, collParts, collRead, scopeRenews,
                 refreshFenced, refreshRead, refreshCand, refreshes, outcome>>

----

Environment ==
  \/ \E c \in Collections : Write(c)
  \/ Trigger \/ Touch \/ Renew \/ RenewScope \/ Drop \/ RefreshBegin \/ FailColl

Observer ==
  \/ \E c \in Collections : Deliver(c)
  \/ Arm \/ StartFull \/ ReadFull \/ PublishFull
  \/ StartColl \/ ReadColl \/ PublishColl
  \/ RefreshRead \/ RefreshPublish

Next == Environment \/ Observer

Fairness ==
  /\ \A c \in Collections : WF_vars(Deliver(c))
  /\ WF_vars(Arm) /\ WF_vars(StartFull) /\ WF_vars(ReadFull) /\ WF_vars(PublishFull)
  /\ WF_vars(StartColl) /\ WF_vars(ReadColl) /\ WF_vars(PublishColl)
  /\ WF_vars(RefreshRead) /\ WF_vars(RefreshPublish)

Spec == Init /\ [][Next]_vars /\ Fairness

----

TypeOK ==
  /\ \A c \in Collections : db[c] \in 0..WriteBound(c) /\ store[c] \in 0..WriteBound(c)
  /\ undelivered \in [Collections -> BOOLEAN]
  /\ stream \in {"arming", "armed"}
  /\ full \in {"idle", "captured", "read"}
  /\ coll \in {"idle", "captured", "read"}
  /\ refresh \in {"idle", "captured", "read"}
  /\ pending \in BOOLEAN
  /\ queued \subseteq Partial /\ collParts \subseteq Partial
  /\ touches \in 0..K /\ renews \in 0..K /\ scopeRenews \in 0..ScopeK
  /\ \A c \in Partial : collRead[c] \in 0..WriteBound(c)
  /\ dbCand \in BOOLEAN /\ storeCand \in BOOLEAN
  /\ fullCand \in BOOLEAN /\ refreshCand \in BOOLEAN
  /\ outcome \in {"none", "published", "rejected"}

\* A read of the whole node that can still show what the runtime holds: not
\* read yet, read it, or will be rejected and queued again.
FullOwns(c) ==
  \/ full = "captured"
  \/ full = "read" /\ (fullRead[c] = db[c] \/ fullFenced)
  \/ refresh = "captured"
  \/ refresh = "read" /\ (refreshRead[c] = db[c] \/ refreshFenced)

\* What still owns a write of `c` the desktop does not show: its undelivered
\* event, a queued full reload, its queued partial read, the re-arm of a
\* dropped stream, or a read that can still see it. A partial read holding the
\* session scope may also settle by queuing the full reload.
Owned(c) ==
  \/ undelivered[c]
  \/ pending
  \/ stream = "arming"
  \/ c \in queued
  \/ FullOwns(c)
  \/ c \in collParts /\ coll = "captured"
  \/ c \in collParts /\ coll = "read"
       /\ (collRead[c] = db[c] \/ collFenced \/ ("ses" \in collParts /\ storeCand))

\* The same for the trigger candidate, which only a read of the node shows.
CandidateOwned ==
  \/ undelivered["req"]
  \/ pending
  \/ stream = "arming"
  \/ full = "captured"
  \/ full = "read" /\ (fullCand \/ fullFenced)
  \/ refresh = "captured"
  \/ refresh = "read" /\ (refreshCand \/ refreshFenced)

\* NoLostWrite: once armed, every write the desktop does not show is owned.
NoLostWrite ==
  /\ \A c \in Collections : store[c] < db[c] => Owned(c)
  /\ dbCand /\ ~storeCand => CandidateOwned

\* NoStaleCommit: no publication replaces a newer published version of any
\* part.
NoStaleCommit == [][\A c \in Collections : store'[c] >= store[c]]_vars

\* NoDroppedCandidate: no publication removes a request the runtime still
\* holds and a full load would show.
NoDroppedCandidate == [][storeCand /\ dbCand => storeCand']_vars

\* EventualPublication: the node keeps publishing full reloads.
EventualPublication == []<>(outcome = "published")

\* Convergence: everything the desktop shows eventually stays the runtime's.
Convergence == <>[](store = db /\ storeCand = dbCand)

====
