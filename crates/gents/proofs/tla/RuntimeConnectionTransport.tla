---- MODULE RuntimeConnectionTransport ----
(***************************************************************************)
(* The desktop observer's connection to one runtime it reads over           *)
(* authenticated HTTP as the User (gents-desktop-core client/observe).      *)
(*                                                                         *)
(* Three processes share the observer state:                               *)
(*  - the Source task (remote.rs `observe`/`arm`): arms K SSE              *)
(*    subscriptions one at a time, then sends a ready notice; every         *)
(*    renewal period it arms a replacement set while the old set keeps      *)
(*    streaming, swaps them and sends a renewed notice; a stream ends by a  *)
(*    transport failure or a refusal, and the task ends;                    *)
(*  - the Observer loop (observe.rs): applies notices of the current        *)
(*    generation, runs one snapshot load at a time, and reconnects an      *)
(*    ended source at the same endpoint after the Lean `reconnectDelayMs`   *)
(*    of its consecutive reconnect count (`Delay`);                         *)
(*  - Reconcile: a peer directory change (endpoint added, changed or        *)
(*    removed, peer removed) replaces or drops the source.                  *)
(* The environment revokes/restores the User's grant and takes the runtime  *)
(* offline/online a bounded number of times. `Revocation` is how the        *)
(* runtime answers a revoked User: "served" is the pinned served runtime    *)
(* (handshake accepted, streams silent, a read answers with the selected    *)
(* roots omitted; observe test `revoked_user_is_answered_with_omitted_      *)
(* roots`), which native ACP means as "nothing visible", and the observer   *)
(* reads it as an ordinary empty view; "handshake" (HTTP 403 at the         *)
(* handshake and on a read) and "envelope" (a PERMISSION_DENIED envelope on *)
(* an open stream and HTTP 403 on a read) are the access owner's refusals,  *)
(* checked so a runtime that adopts them stays correct. A refusal is the    *)
(* runtime's own answer, so it is shown (status refused) while the source   *)
(* keeps reconnecting on the backoff; nothing omitted or empty is shown as  *)
(* one. `Drops` adds a runtime that accepts the arm and the snapshot and    *)
(* then drops the stream at any time (a flapping runtime).                  *)
(*                                                                         *)
(* Durations are ticks: `Delay(n)` is 1 tick for the first `Flat`           *)
(* reconnects, then doubles up to `Cap` (Lean `reconnectDelayMs`: 250 ms    *)
(* five times, then doubling to 30 s). The count resets only after the      *)
(* source stayed connected for the stable period (Lean `reconnectStableMs`; *)
(* `Stabilize`), never at an arm or a snapshot alone. The count saturates   *)
(* at `MaxAttempts` *)
(* for TLC. Generations are compared only for equality, so a new source     *)
(* takes the smallest generation no notice or load still names              *)
(* (`FreshGen`). A tokio abort lands at the task's next await, so an        *)
(* aborted task may still enqueue one notice (`Abort`).                     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  K,                  \* subscriptions armed per connection
  MaxGen,             \* generation labels (only equality matters)
  Flat,               \* reconnects at the base delay (Lean reconnectFlatAttempts)
  Cap,                \* reconnect cap, in ticks (Lean reconnectCapMs)
  MaxAttempts,        \* reconnect count saturation for TLC
  MaxEnv,             \* environment changes (grant, reachability, endpoint)
  MaxChan,            \* notice channel bound for TLC
  Revocation,         \* "served" | "handshake" | "envelope" (see header)
  Capped,             \* FALSE: the backoff doubles without a cap
  RenewalAsFailure,   \* RED: planned renewal reported as a transport failure
  MakeBeforeBreak,    \* FALSE: a renewal drops the armed set before re-arming
  OmittedIsEmpty,     \* FALSE: a read with omitted roots reconnects the source
  NamedNoEndpoint,    \* FALSE: a peer without an endpoint has no status
  GenerationCheck,    \* FALSE: notices of a replaced source are applied
  ArmBeforeReady,     \* FALSE: ready is sent before every subscription is armed
  Drops,              \* a reachable runtime may drop an open stream at any time
  KeepEnded,          \* FALSE: a reconcile restarts a source whose task ended
                      \* before its end notice was applied
  ResetOnConnect      \* RED: a connected snapshot resets the reconnect count

Endpoints == {"e1", "e2"}
Peers == {"absent", "none"} \cup Endpoints
Statuses == {"unset", "connecting", "connected", "disconnected", "refused", "noEndpoint"}
Kinds == {"ready", "renewed", "failed", "refused"}
Phases == {"idle", "arming", "streaming", "ended"}
Gens == 1..MaxGen

VARIABLES
  peer, grant, reachable, env, dirty,         \* environment and peer directory
  srcGen, srcEndpoint, srcReady, srcRetrying, \* Sources entry (0 = none)
  tPhase, tArmed, tRenewing, tNew,            \* the live source task
  chan,                                       \* notice channel
  status,                                     \* ObservedStore runtime_connection
  pending, load,                              \* observer snapshot scheduling
  attempts, retryIn, stable,                  \* reconnect schedule
  fresh, outage, causeless, waited, flatDials, \* ghosts
  skipped

\* `stable`: the current source stayed connected for the stable period.
\* `waited`: ticks waited since the runtime last stopped giving a reason to
\* reconnect (it answers and accepts the client), until the source re-arms.
\* `flatDials`: reconnects scheduled at the base delay since the source last
\* stayed connected for the stable period.
\* `skipped`: a source of the same endpoint was restarted without its
\* reconnect delay.
ghosts == <<waited, flatDials>>

vars == <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
          srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
          attempts, retryIn, stable, fresh, outage, causeless, waited, flatDials, skipped>>

NoLoad == [gen |-> 0, afterArm |-> FALSE]

Min(S) == CHOOSE x \in S : \A y \in S : x <= y

Doubled(n) == 2 ^ (n + 1 - Flat)

\* Lean `reconnectDelayMs`, in ticks.
Delay(n) ==
  IF n < Flat THEN 1
  ELSE IF Capped /\ Doubled(n) > Cap THEN Cap ELSE Doubled(n)

MaxDelay == IF Capped THEN Cap ELSE Doubled(MaxAttempts)

UsedGens == {chan[i].gen : i \in 1..Len(chan)} \cup {load.gen, srcGen}

FreshGen == Min(Gens \ UsedGens)

CanStart == Gens \ UsedGens # {}

TypeOK ==
  /\ peer \in Peers /\ grant \in BOOLEAN /\ reachable \in BOOLEAN
  /\ env \in 0..MaxEnv /\ dirty \in BOOLEAN
  /\ srcGen \in 0..MaxGen /\ srcEndpoint \in Endpoints \cup {"none"}
  /\ srcReady \in BOOLEAN /\ srcRetrying \in BOOLEAN
  /\ tPhase \in Phases /\ tArmed \in 0..K /\ tRenewing \in BOOLEAN /\ tNew \in 0..K
  /\ \A i \in 1..Len(chan) : chan[i].gen \in Gens /\ chan[i].kind \in Kinds
  /\ status \in Statuses /\ pending \in BOOLEAN
  /\ load.gen \in 0..MaxGen /\ load.afterArm \in BOOLEAN
  /\ attempts \in 0..MaxAttempts /\ retryIn \in 0..MaxDelay /\ stable \in BOOLEAN
  /\ fresh \in BOOLEAN /\ outage \in BOOLEAN /\ causeless \in BOOLEAN
  /\ waited \in 0..(MaxDelay + 1) /\ flatDials \in 0..(Flat + 1) /\ skipped \in BOOLEAN

Init ==
  /\ peer \in Endpoints /\ grant = TRUE /\ reachable = TRUE /\ env = 0
  /\ dirty = TRUE
  /\ srcGen = 0 /\ srcEndpoint = "none" /\ srcReady = FALSE /\ srcRetrying = FALSE
  /\ tPhase = "idle" /\ tArmed = 0 /\ tRenewing = FALSE /\ tNew = 0
  /\ chan = <<>> /\ status = "unset" /\ pending = FALSE /\ load = NoLoad
  /\ attempts = 0 /\ retryIn = 0 /\ stable = FALSE /\ fresh = FALSE
  /\ outage = FALSE /\ causeless = FALSE /\ waited = 0 /\ flatDials = 0
  /\ skipped = FALSE

Send(kind) == chan' = Append(chan, [gen |-> srcGen, kind |-> kind])

\* The runtime gives a reason to reconnect: it is unreachable, or it refuses
\* a revoked User. A quiet or empty view of a reachable runtime is none.
HasCause == ~reachable \/ (~grant /\ Revocation # "served")

(* --------------------------- environment ------------------------------ *)

EnvStep == env < MaxEnv /\ env' = env + 1

MovePeer(p) ==
  /\ EnvStep /\ p # peer /\ peer' = p /\ dirty' = TRUE /\ waited' = 0
  /\ UNCHANGED <<grant, reachable, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, stable, fresh, outage, causeless, flatDials, skipped>>

ToggleGrant ==
  /\ EnvStep /\ grant' = ~grant /\ waited' = 0
  /\ UNCHANGED <<peer, reachable, dirty, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, stable, fresh, outage, causeless, flatDials, skipped>>

\* Another peer's record changed: the directory is reconciled with this
\* peer unchanged.
TouchDirectory ==
  /\ EnvStep /\ dirty' = TRUE
  /\ UNCHANGED <<peer, grant, reachable, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, stable, fresh, outage, causeless, waited, flatDials,
                 skipped>>

ToggleReachable ==
  /\ EnvStep /\ reachable' = ~reachable
  /\ outage' = (outage \/ reachable) /\ waited' = 0 /\ UNCHANGED skipped
  /\ UNCHANGED <<peer, grant, dirty, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, stable, fresh, causeless, flatDials>>

(* ----------------------------- reconcile ------------------------------ *)

\* Dropping a source aborts its task; one already-running poll may still
\* enqueue its next notice.
DropSource(staleKind) ==
  /\ \/ chan' = chan
     \/ /\ srcGen # 0 /\ tPhase \in {"arming", "streaming"}
        /\ Len(chan) < MaxChan
        /\ staleKind \in Kinds
        /\ chan' = Append(chan, [gen |-> srcGen, kind |-> staleKind])

\* A new source task. A reconnect keeps the shown status (an outage stays
\* `disconnected` until the runtime answers); a new endpoint shows
\* `connecting`.
StartSource(endpoint, newStatus) ==
  /\ srcGen' = FreshGen /\ srcEndpoint' = endpoint
  /\ srcReady' = FALSE /\ srcRetrying' = FALSE
  /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = FALSE /\ tNew' = 0
  /\ status' = newStatus /\ retryIn' = 0 /\ fresh' = FALSE
  /\ load' = NoLoad /\ stable' = FALSE

\* A reconcile keeps the source of an unchanged endpoint, including one whose
\* task has ended: its end notice is already queued (every task end sends
\* one) and schedules the reconnect on the backoff. Restarting it here would
\* skip that delay.
Keep == srcGen # 0 /\ srcEndpoint = peer /\ (KeepEnded \/ srcRetrying \/ tPhase # "ended")

Reconcile ==
  /\ dirty /\ dirty' = FALSE
  /\ skipped' = (skipped \/ (peer \in Endpoints /\ srcGen # 0 /\ srcEndpoint = peer /\ ~Keep))
  /\ IF peer \in Endpoints
       THEN IF Keep
              THEN UNCHANGED <<srcGen, srcEndpoint, srcReady, srcRetrying, tPhase,
                               tArmed, tRenewing, tNew, chan, status, load, retryIn,
                               stable, fresh, pending>>
              ELSE /\ CanStart
                   /\ \E k \in Kinds \cup {"none"} : DropSource(k)
                   /\ StartSource(peer, "connecting")
                   /\ pending' = FALSE
       ELSE /\ \E k \in Kinds \cup {"none"} : DropSource(k)
            /\ srcGen' = 0 /\ srcEndpoint' = "none" /\ srcReady' = FALSE
            /\ srcRetrying' = FALSE /\ tPhase' = "idle" /\ tArmed' = 0
            /\ tRenewing' = FALSE /\ tNew' = 0 /\ load' = NoLoad /\ pending' = FALSE
            /\ retryIn' = 0 /\ fresh' = FALSE /\ stable' = FALSE
            /\ status' = IF peer = "none" /\ NamedNoEndpoint THEN "noEndpoint" ELSE "unset"
  \* A peer removed from the directory forgets its schedule; re-adding it is
  \* the user's action, not a redial.
  /\ attempts' = IF peer \in Endpoints THEN attempts ELSE 0
  /\ flatDials' = IF peer \in Endpoints THEN flatDials ELSE 0
  /\ UNCHANGED <<peer, grant, reachable, env, outage, causeless, waited>>

(* ---------------------------- source task ----------------------------- *)

TaskUnchanged == UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                             srcReady, srcRetrying, status, pending, load, attempts,
                             retryIn, stable, outage, flatDials, skipped>>

\* The task sends its end notice and ends; the observer schedules the
\* reconnect. A failure drops every set, including a replacement still arming.
EndTask(kind) ==
  /\ Len(chan) < MaxChan /\ Send(kind) /\ fresh' = FALSE
  /\ tPhase' = "ended"
  /\ tArmed' = 0 /\ tRenewing' = FALSE /\ tNew' = 0

Fail(kind) == EndTask(kind) /\ causeless' = (causeless \/ ~HasCause)

\* One handshake: refused when unreachable, or for a revoked User only when
\* the runtime refuses at the handshake.
Handshake(armed) ==
  IF ~reachable THEN Fail("failed")
  ELSE IF ~grant /\ Revocation = "handshake" THEN Fail("refused")
  ELSE armed /\ UNCHANGED causeless

ArmOne ==
  /\ srcGen # 0 /\ tPhase = "arming" /\ tArmed < K
  /\ Handshake(tArmed' = tArmed + 1 /\ UNCHANGED <<chan, tPhase, tRenewing, tNew, fresh>>)
  /\ TaskUnchanged /\ UNCHANGED waited

ArmDone ==
  /\ srcGen # 0 /\ tPhase = "arming"
  /\ IF ArmBeforeReady THEN tArmed = K ELSE TRUE
  /\ Len(chan) < MaxChan
  /\ Send(IF tRenewing THEN "renewed" ELSE "ready")
  /\ tPhase' = "streaming" /\ tRenewing' = FALSE /\ waited' = 0
  /\ UNCHANGED <<tArmed, tNew, fresh, causeless>>
  /\ TaskUnchanged

StreamFails ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~reachable
  /\ Fail("failed") /\ TaskUnchanged /\ UNCHANGED waited

\* A flapping runtime: reachable, it accepted the arm (and maybe the
\* snapshot), and drops the open stream. Not fair: it may happen at any time.
StreamDrops ==
  /\ Drops /\ srcGen # 0 /\ tPhase = "streaming" /\ reachable
  /\ EndTask("failed") /\ TaskUnchanged /\ UNCHANGED <<causeless, waited>>

\* A runtime that answers an open stream of a revoked User with a GraphQL
\* errors envelope; the served runtime keeps it silent instead.
StreamRefused ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~grant /\ reachable
  /\ Revocation = "envelope"
  /\ Fail("refused")
  /\ TaskUnchanged /\ UNCHANGED waited

\* A renewal period elapsed since the current set was armed. Make-before-
\* break: the replacement is armed while the current set keeps streaming,
\* so no change goes undelivered and the shown data stays fresh. Breaking
\* first leaves no armed subscription until the re-arm completes.
Renew ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~tRenewing
  /\ IF RenewalAsFailure
       THEN Fail("failed")
       ELSE IF MakeBeforeBreak
       THEN /\ tRenewing' = TRUE /\ tNew' = 0
            /\ UNCHANGED <<chan, tPhase, tArmed, fresh, causeless>>
       ELSE /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = TRUE
            /\ fresh' = FALSE /\ UNCHANGED <<chan, tNew, causeless>>
  /\ TaskUnchanged /\ UNCHANGED waited

ArmReplacement ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew < K
  /\ Handshake(tNew' = tNew + 1 /\ UNCHANGED <<chan, tPhase, tArmed, tRenewing, fresh>>)
  /\ TaskUnchanged /\ UNCHANGED waited

\* The replacement is armed: the old set is dropped and `renewed` is sent so
\* the observer resnapshots, the only read that shows the served runtime's
\* current view of a revoked User.
Swap ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew = K
  /\ Len(chan) < MaxChan /\ Send("renewed")
  /\ tRenewing' = FALSE /\ tNew' = 0
  /\ UNCHANGED <<tPhase, tArmed, fresh, causeless, waited>>
  /\ TaskUnchanged

(* ------------------------------ observer ------------------------------ *)

\* Lean `reconnectAttempt` and `reconnectDelayMs`: the count restarts only
\* when the source stayed connected for the stable period.
Schedule ==
  LET a == IF stable THEN 0 ELSE attempts IN
  /\ srcReady' = FALSE /\ srcRetrying' = TRUE /\ load' = NoLoad
  /\ retryIn' = Delay(a) /\ stable' = FALSE
  /\ attempts' = IF a < MaxAttempts THEN a + 1 ELSE a
  /\ flatDials' = IF stable THEN 1
                  ELSE IF Delay(a) = 1 THEN Min({flatDials + 1, Flat + 1}) ELSE flatDials

Notice ==
  /\ Len(chan) > 0
  /\ LET n == Head(chan) IN
     /\ chan' = Tail(chan)
     /\ IF srcGen # 0 /\ ~srcRetrying /\ (n.gen = srcGen \/ ~GenerationCheck)
          THEN CASE n.kind = "ready" ->
                      /\ srcReady' = TRUE /\ pending' = TRUE
                      /\ status' = IF status = "refused" THEN "refused" ELSE "connecting"
                      /\ UNCHANGED <<srcRetrying, load, retryIn, attempts, stable, flatDials>>
                 [] n.kind = "renewed" ->
                      /\ srcReady' = TRUE /\ pending' = TRUE
                      /\ UNCHANGED <<status, srcRetrying, load, retryIn, attempts, stable,
                                     flatDials>>
                 [] n.kind = "failed" ->
                      /\ Schedule /\ status' = "disconnected"
                      /\ UNCHANGED pending
                 [] n.kind = "refused" ->
                      /\ Schedule /\ status' = "refused" /\ pending' = FALSE
          ELSE UNCHANGED <<srcReady, srcRetrying, status, pending, load, retryIn, attempts,
                           stable, flatDials>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, tPhase,
                 tArmed, tRenewing, tNew, fresh, outage, causeless, waited, skipped>>

StartLoad ==
  /\ pending /\ srcReady /\ ~srcRetrying /\ load.gen = 0 /\ srcGen # 0
  /\ load' = [gen |-> srcGen, afterArm |-> (tPhase = "streaming" /\ tArmed = K)]
  /\ pending' = FALSE
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, attempts,
                 retryIn, stable, fresh, outage, causeless, ghosts, skipped>>

\* A served runtime answers a revoked User's read with the selected roots
\* omitted, which the snapshot reader decodes as empty (Lean `SnapshotOutcome`
\* has no withheld case); `OmittedIsEmpty = FALSE` is the rejected reading
\* that treats it as a failure. A refused read (HTTP 403) aborts the source
\* task and reconnects it. A read that failed for a transport reason leaves
\* the streams armed and is retried (Lean `reconnects .snapshot
\* .transportFailed = false`); it neither schedules a reconnect nor ends the
\* task.
FinishLoad ==
  /\ load.gen # 0 /\ load.gen = srcGen
  /\ IF ~reachable
       THEN /\ status' = "disconnected" /\ pending' = TRUE /\ load' = NoLoad
            /\ UNCHANGED <<srcReady, srcRetrying, retryIn, attempts, stable, fresh, tPhase,
                           causeless, flatDials>>
       ELSE IF ~grant /\ (Revocation # "served" \/ ~OmittedIsEmpty)
       THEN /\ Schedule /\ status' = "refused" /\ pending' = FALSE
            /\ tPhase' = "ended" /\ UNCHANGED fresh
            /\ causeless' = (causeless \/ ~HasCause)
       ELSE /\ status' = "connected" /\ load' = NoLoad
            /\ attempts' = IF ResetOnConnect THEN 0 ELSE attempts
            /\ fresh' = (fresh \/ load.afterArm)
            /\ UNCHANGED <<srcReady, srcRetrying, pending, retryIn, stable, tPhase, causeless,
                           flatDials>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                 tArmed, tRenewing, tNew, chan, outage, waited, skipped>>

\* The source stayed connected for the stable period (Lean
\* `reconnectStableMs`): its next reconnect starts the count again.
Stabilize ==
  /\ srcGen # 0 /\ ~srcRetrying /\ tPhase = "streaming" /\ status = "connected"
  /\ ~stable /\ stable' = TRUE /\ flatDials' = 0
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, fresh, outage, causeless, waited, skipped>>

Tick ==
  /\ srcRetrying /\ retryIn > 0 /\ retryIn' = retryIn - 1
  /\ waited' = IF HasCause THEN 0 ELSE Min({waited + 1, MaxDelay + 1})
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, stable, fresh, outage, causeless, flatDials, skipped>>

\* `take_due_reconnects` then `reconcile`: the same endpoint, a new source.
Reconnect ==
  /\ srcRetrying /\ retryIn = 0 /\ ~dirty /\ peer = srcEndpoint /\ CanStart
  /\ StartSource(srcEndpoint, status) /\ pending' = FALSE /\ chan' = chan
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, attempts, outage, causeless, ghosts, skipped>>

System == Reconcile \/ ArmOne \/ ArmDone \/ StreamFails \/ StreamRefused \/ Renew
          \/ ArmReplacement \/ Swap \/ Notice \/ StartLoad \/ FinishLoad
          \/ Stabilize \/ Tick \/ Reconnect \/ StreamDrops

Env == (\E p \in Peers : MovePeer(p)) \/ ToggleGrant \/ ToggleReachable \/ TouchDirectory

Next == System \/ Env

\* Every system step is weakly fair. Renewal is a timer the task cannot
\* skip: a streaming source renews forever, which is how an observer of the
\* served runtime reads its current view. Lean `renewalStartMs` spaces
\* renewals a full period after each completed arm; durations are not
\* modeled here beyond the reconnect ticks.
Fairness ==
  /\ WF_vars(Reconcile) /\ WF_vars(ArmOne) /\ WF_vars(ArmDone)
  /\ WF_vars(StreamFails) /\ WF_vars(StreamRefused) /\ WF_vars(Renew)
  /\ WF_vars(ArmReplacement) /\ WF_vars(Swap)
  /\ WF_vars(Notice) /\ WF_vars(StartLoad) /\ WF_vars(FinishLoad)
  /\ WF_vars(Stabilize) /\ WF_vars(Tick) /\ WF_vars(Reconnect)

Spec == Init /\ [][Next]_vars /\ Fairness

(* ----------------------------- properties ----------------------------- *)

\* The observer still owes a post-arm snapshot or a status change: an end
\* notice of this source is queued, a resnapshot is scheduled, or a load that
\* began after the arm is running. A renewal owes nothing: it never leaves
\* the source without an armed set.
Owed ==
  \/ \E i \in 1..Len(chan) : chan[i].gen = srcGen /\ chan[i].kind # "ready"
  \/ pending /\ srcReady
  \/ load.gen = srcGen /\ load.afterArm

\* Connected only when the shown data comes from a snapshot that began after
\* every subscription of the current arm was armed and some set has stayed
\* armed since, or the observer still owes one.
ConnectedAfterArmedSnapshot == status = "connected" => srcGen # 0 /\ (fresh \/ Owed)

\* A notice of a replaced source never moves the status.
StaleNoticeInert ==
  [][(Len(chan) > 0 /\ chan' = Tail(chan) /\ Head(chan).gen # srcGen /\ srcGen' = srcGen)
      => status' = status]_vars

\* A removed peer has no source and no status.
RemovalClears == (peer = "absent" /\ ~dirty) => (srcGen = 0 /\ status = "unset")

\* A peer with no endpoint shows that, once reconciled.
NoEndpointNamed == (peer = "none" /\ ~dirty) => status = "noEndpoint"

\* `disconnected` is shown only after the runtime was actually unreachable:
\* neither a renewal nor a revocation reads as an outage.
RenewalNeverDisconnects == status = "disconnected" => outage

\* A quiet or empty view is never a reason to reconnect: every reconnect
\* follows an unreachable runtime or a refusal.
NoCauselessReconnect == ~causeless

\* Bounded recovery: once the runtime answers and accepts the client, the
\* source re-arms within `Cap` ticks, however many reconnects came before.
ReArmedWithinCap == waited <= Cap

\* A runtime that never holds a connection for the stable period (it drops
\* every stream after the arm and the snapshot) is dialed at the base delay
\* at most `Flat` times in a row; later dials back off.
FlapBounded == flatDials <= Flat

\* A source of an unchanged endpoint is restarted only by its reconnect,
\* after the backoff; a directory reconcile never short-cuts it.
ReconnectsWaitTheBackoff == ~skipped

Reachable == peer \in Endpoints /\ reachable

\* A recovered runtime is eventually re-armed (backoff delays, never stops,
\* the reconnect), and its view eventually reads connected whatever the User
\* may read there, unless it refuses the User.
RecoveredEventuallyArmed ==
  <>[](Reachable /\ (grant \/ Revocation = "served"))
    => <>(tPhase = "streaming" /\ tArmed = K)

EventuallyConnected ==
  <>[](Reachable /\ (grant \/ Revocation = "served")) => <>[](status = "connected")

\* A runtime that refuses a revoked User names the refusal, and keeps naming
\* it across every reconnect; it never reads as a silent connection attempt
\* or an outage.
RefusalNamed ==
  <>[](Reachable /\ ~grant /\ Revocation # "served") => <>[](status = "refused")

\* A peer without an endpoint is never shown as a connection attempt.
NoEndpointEventuallyNamed ==
  <>[](peer = "none") => <>[](status = "noEndpoint")

====
