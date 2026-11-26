---- MODULE ApprovalQueueWatch ----
\* lean: none
\* crates: gents-tools

\* One approval waiter (`approval::ask` calling `write_atomic`) and one test
\* observer (`approval::testing::wait_for_question` / `observe`) watching the
\* queue directory via macOS `notify` `KqueueWatcher`.
\*
\* `prepare_listener` creates `.listening` before the observer registers its
\* watcher. With `RecursiveMode::NonRecursive`, `KqueueWatcher::add_watch`
\* watches only the directory inode and omits pre-existing files from its
\* `watches` map. On a coalesced directory `NOTE_WRITE` (`NamedTempFile` +
\* `persist` rename), `KqueueWatcher::handle_kqueue` scans `read_dir` and
\* reports only the first entry absent from `watches`, which can be
\* `.listening`. Dropping events whose path is `.listening` drops the only
\* wakeup for the newly persisted request file.
\*
\* Safeguards, removed by constants:
\*   FilterListenerOnly  drop kqueue events attributed to `.listening`
\*   NonRecursiveInit    do not pre-register `.listening` in `kqueue.watches`

CONSTANTS FilterListenerOnly, NonRecursiveInit

VARIABLES tempCreated, requestVisible, dirDirty, listenerWatched,
          pendingWake, observerPhase, seen, fallback

vars == <<tempCreated, requestVisible, dirDirty, listenerWatched,
          pendingWake, observerPhase, seen, fallback>>

Init ==
  /\ tempCreated = FALSE
  /\ requestVisible = FALSE
  /\ dirDirty = FALSE
  /\ listenerWatched = ~NonRecursiveInit
  /\ pendingWake = FALSE
  /\ observerPhase = "waiting"
  /\ seen = FALSE
  /\ fallback = FALSE

WriteTemp ==
  /\ ~tempCreated
  /\ tempCreated' = TRUE
  /\ dirDirty' = TRUE
  /\ UNCHANGED <<requestVisible, listenerWatched, pendingWake, observerPhase, seen, fallback>>

PersistRequest ==
  /\ tempCreated /\ ~requestVisible
  /\ requestVisible' = TRUE
  /\ dirDirty' = TRUE
  /\ UNCHANGED <<tempCreated, listenerWatched, pendingWake, observerPhase, seen, fallback>>

\* Kqueue coalesces directory `NOTE_WRITE` notifications and picks one unwatched
\* file from `read_dir` (which can be `.listening` if not yet in `watches`).
KqueueDeliverListener ==
  /\ dirDirty
  /\ ~listenerWatched
  /\ dirDirty' = FALSE
  /\ listenerWatched' = TRUE
  /\ pendingWake' = (pendingWake \/ ~FilterListenerOnly)
  /\ UNCHANGED <<tempCreated, requestVisible, observerPhase, seen, fallback>>

KqueueDeliverOther ==
  /\ dirDirty
  /\ dirDirty' = FALSE
  /\ pendingWake' = TRUE
  /\ UNCHANGED <<tempCreated, requestVisible, listenerWatched, observerPhase, seen, fallback>>

ObserverWake ==
  /\ observerPhase = "waiting"
  /\ pendingWake
  /\ pendingWake' = FALSE
  /\ observerPhase' = "reading"
  /\ UNCHANGED <<tempCreated, requestVisible, dirDirty, listenerWatched, seen, fallback>>

ObserverRead ==
  /\ observerPhase = "reading"
  /\ seen' = (seen \/ requestVisible)
  /\ observerPhase' = "waiting"
  /\ UNCHANGED <<tempCreated, requestVisible, dirDirty, listenerWatched, pendingWake, fallback>>

Quiet == ~ENABLED (WriteTemp \/ PersistRequest \/ KqueueDeliverListener \/ KqueueDeliverOther \/ ObserverWake \/ ObserverRead)

TimeoutFallback ==
  /\ observerPhase = "waiting" /\ requestVisible /\ ~seen
  /\ Quiet
  /\ fallback' = TRUE
  /\ seen' = TRUE
  /\ UNCHANGED <<tempCreated, requestVisible, dirDirty, listenerWatched, pendingWake, observerPhase>>

Done ==
  /\ seen
  /\ UNCHANGED vars

Next ==
  \/ WriteTemp
  \/ PersistRequest
  \/ KqueueDeliverListener
  \/ KqueueDeliverOther
  \/ ObserverWake
  \/ ObserverRead
  \/ TimeoutFallback
  \/ Done

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(WriteTemp)
  /\ WF_vars(PersistRequest)
  /\ WF_vars(KqueueDeliverListener)
  /\ WF_vars(KqueueDeliverOther)
  /\ WF_vars(ObserverWake)
  /\ WF_vars(ObserverRead)
  /\ WF_vars(TimeoutFallback)

WokenByEvents == ~fallback
Seen == <> seen
====
