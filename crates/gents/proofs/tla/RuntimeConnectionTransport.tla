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
(* checked so a runtime that adopts them stays correct. There is no denial  *)
(* state: a refusal is a reconnect (status connecting).                     *)
(*                                                                         *)
(* Durations are ticks: `Delay(n)` is 1 tick for the first `Flat`           *)
(* reconnects, then doubles up to `Cap` (Lean `reconnectDelayMs`: 250 ms    *)
(* five times, then doubling to 30 s). The count saturates at `MaxAttempts` *)
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
  ArmBeforeReady      \* FALSE: ready is sent before every subscription is armed

Endpoints == {"e1", "e2"}
Peers == {"absent", "none"} \cup Endpoints
Statuses == {"unset", "connecting", "connected", "disconnected", "noEndpoint"}
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
  attempts, retryIn,                          \* reconnect schedule
  fresh, outage, causeless                    \* ghosts

vars == <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
          srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
          attempts, retryIn, fresh, outage, causeless>>

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
  /\ attempts \in 0..MaxAttempts /\ retryIn \in 0..MaxDelay
  /\ fresh \in BOOLEAN /\ outage \in BOOLEAN /\ causeless \in BOOLEAN

Init ==
  /\ peer \in Endpoints /\ grant = TRUE /\ reachable = TRUE /\ env = 0
  /\ dirty = TRUE
  /\ srcGen = 0 /\ srcEndpoint = "none" /\ srcReady = FALSE /\ srcRetrying = FALSE
  /\ tPhase = "idle" /\ tArmed = 0 /\ tRenewing = FALSE /\ tNew = 0
  /\ chan = <<>> /\ status = "unset" /\ pending = FALSE /\ load = NoLoad
  /\ attempts = 0 /\ retryIn = 0 /\ fresh = FALSE /\ outage = FALSE
  /\ causeless = FALSE

Send(kind) == chan' = Append(chan, [gen |-> srcGen, kind |-> kind])

\* The runtime gives a reason to reconnect: it is unreachable, or it refuses
\* a revoked User. A quiet or empty view of a reachable runtime is none.
HasCause == ~reachable \/ (~grant /\ Revocation # "served")

(* --------------------------- environment ------------------------------ *)

EnvStep == env < MaxEnv /\ env' = env + 1

MovePeer(p) ==
  /\ EnvStep /\ p # peer /\ peer' = p /\ dirty' = TRUE
  /\ UNCHANGED <<grant, reachable, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, fresh, outage, causeless>>

ToggleGrant ==
  /\ EnvStep /\ grant' = ~grant
  /\ UNCHANGED <<peer, reachable, dirty, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, fresh, outage, causeless>>

ToggleReachable ==
  /\ EnvStep /\ reachable' = ~reachable
  /\ outage' = (outage \/ reachable)
  /\ UNCHANGED <<peer, grant, dirty, srcGen, srcEndpoint, srcReady, srcRetrying,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, retryIn, fresh, causeless>>

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
  /\ load' = NoLoad

Keep == srcGen # 0 /\ srcEndpoint = peer /\ (srcRetrying \/ tPhase # "ended")

Reconcile ==
  /\ dirty /\ dirty' = FALSE
  /\ IF peer \in Endpoints
       THEN IF Keep
              THEN UNCHANGED <<srcGen, srcEndpoint, srcReady, srcRetrying, tPhase,
                               tArmed, tRenewing, tNew, chan, status, load, retryIn,
                               fresh, pending>>
              ELSE /\ CanStart
                   /\ \E k \in Kinds \cup {"none"} : DropSource(k)
                   /\ StartSource(peer, "connecting")
                   /\ pending' = FALSE
       ELSE /\ \E k \in Kinds \cup {"none"} : DropSource(k)
            /\ srcGen' = 0 /\ srcEndpoint' = "none" /\ srcReady' = FALSE
            /\ srcRetrying' = FALSE /\ tPhase' = "idle" /\ tArmed' = 0
            /\ tRenewing' = FALSE /\ tNew' = 0 /\ load' = NoLoad /\ pending' = FALSE
            /\ retryIn' = 0 /\ fresh' = FALSE
            /\ status' = IF peer = "none" /\ NamedNoEndpoint THEN "noEndpoint" ELSE "unset"
  /\ attempts' = IF peer \in Endpoints THEN attempts ELSE 0
  /\ UNCHANGED <<peer, grant, reachable, env, outage, causeless>>

(* ---------------------------- source task ----------------------------- *)

TaskUnchanged == UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                             srcReady, srcRetrying, status, pending, load, attempts,
                             retryIn, outage>>

\* The task sends its end notice and ends; the observer schedules the
\* reconnect. A failure drops every set, including a replacement still arming.
Fail(kind) ==
  /\ Len(chan) < MaxChan /\ Send(kind) /\ fresh' = FALSE
  /\ tPhase' = "ended"
  /\ tArmed' = 0 /\ tRenewing' = FALSE /\ tNew' = 0
  /\ causeless' = (causeless \/ ~HasCause)

\* One handshake: refused when unreachable, or for a revoked User only when
\* the runtime refuses at the handshake.
Handshake(armed) ==
  IF ~reachable THEN Fail("failed")
  ELSE IF ~grant /\ Revocation = "handshake" THEN Fail("refused")
  ELSE armed /\ UNCHANGED causeless

ArmOne ==
  /\ srcGen # 0 /\ tPhase = "arming" /\ tArmed < K
  /\ Handshake(tArmed' = tArmed + 1 /\ UNCHANGED <<chan, tPhase, tRenewing, tNew, fresh>>)
  /\ TaskUnchanged

ArmDone ==
  /\ srcGen # 0 /\ tPhase = "arming"
  /\ IF ArmBeforeReady THEN tArmed = K ELSE TRUE
  /\ Len(chan) < MaxChan
  /\ Send(IF tRenewing THEN "renewed" ELSE "ready")
  /\ tPhase' = "streaming" /\ tRenewing' = FALSE
  /\ UNCHANGED <<tArmed, tNew, fresh, causeless>>
  /\ TaskUnchanged

StreamFails ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~reachable
  /\ Fail("failed") /\ TaskUnchanged

\* A runtime that answers an open stream of a revoked User with a GraphQL
\* errors envelope; the served runtime keeps it silent instead.
StreamRefused ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~grant /\ reachable
  /\ Revocation = "envelope"
  /\ Fail("refused")
  /\ TaskUnchanged

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
  /\ TaskUnchanged

ArmReplacement ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew < K
  /\ Handshake(tNew' = tNew + 1 /\ UNCHANGED <<chan, tPhase, tArmed, tRenewing, fresh>>)
  /\ TaskUnchanged

\* The replacement is armed: the old set is dropped and `renewed` is sent so
\* the observer resnapshots, the only read that shows the served runtime's
\* current view of a revoked User.
Swap ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew = K
  /\ Len(chan) < MaxChan /\ Send("renewed")
  /\ tRenewing' = FALSE /\ tNew' = 0
  /\ UNCHANGED <<tPhase, tArmed, fresh, causeless>>
  /\ TaskUnchanged

(* ------------------------------ observer ------------------------------ *)

\* Lean `nextAttempts` of a reconnecting event and `reconnectDelayMs`.
Schedule ==
  /\ srcReady' = FALSE /\ srcRetrying' = TRUE /\ load' = NoLoad
  /\ retryIn' = Delay(attempts)
  /\ attempts' = IF attempts < MaxAttempts THEN attempts + 1 ELSE attempts

Notice ==
  /\ Len(chan) > 0
  /\ LET n == Head(chan) IN
     /\ chan' = Tail(chan)
     /\ IF srcGen # 0 /\ ~srcRetrying /\ (n.gen = srcGen \/ ~GenerationCheck)
          THEN CASE n.kind = "ready" ->
                      /\ srcReady' = TRUE /\ status' = "connecting" /\ pending' = TRUE
                      /\ UNCHANGED <<srcRetrying, load, retryIn, attempts>>
                 [] n.kind = "renewed" ->
                      /\ srcReady' = TRUE /\ pending' = TRUE
                      /\ UNCHANGED <<status, srcRetrying, load, retryIn, attempts>>
                 [] n.kind = "failed" ->
                      /\ Schedule /\ status' = "disconnected"
                      /\ UNCHANGED pending
                 [] n.kind = "refused" ->
                      /\ Schedule /\ status' = "connecting" /\ pending' = FALSE
          ELSE UNCHANGED <<srcReady, srcRetrying, status, pending, load, retryIn, attempts>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, tPhase,
                 tArmed, tRenewing, tNew, fresh, outage, causeless>>

StartLoad ==
  /\ pending /\ srcReady /\ ~srcRetrying /\ load.gen = 0 /\ srcGen # 0
  /\ load' = [gen |-> srcGen, afterArm |-> (tPhase = "streaming" /\ tArmed = K)]
  /\ pending' = FALSE
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, attempts,
                 retryIn, fresh, outage, causeless>>

\* A served runtime answers a revoked User's read with the selected roots
\* omitted, which the snapshot reader decodes as empty (Lean `SnapshotOutcome`
\* has no withheld case); `OmittedIsEmpty = FALSE` is the rejected reading
\* that treats it as a failure. A refused read (HTTP 403) aborts the source
\* task and reconnects it.
FinishLoad ==
  /\ load.gen # 0 /\ load.gen = srcGen
  /\ IF ~reachable
       THEN /\ status' = "disconnected" /\ pending' = TRUE /\ load' = NoLoad
            /\ UNCHANGED <<srcReady, srcRetrying, retryIn, attempts, fresh, tPhase,
                           causeless>>
       ELSE IF ~grant /\ (Revocation # "served" \/ ~OmittedIsEmpty)
       THEN /\ Schedule /\ status' = "connecting" /\ pending' = FALSE
            /\ tPhase' = "ended" /\ UNCHANGED fresh
            /\ causeless' = (causeless \/ ~HasCause)
       ELSE /\ status' = "connected" /\ load' = NoLoad /\ attempts' = 0
            /\ fresh' = (fresh \/ load.afterArm)
            /\ UNCHANGED <<srcReady, srcRetrying, pending, retryIn, tPhase, causeless>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                 tArmed, tRenewing, tNew, chan, outage>>

Tick ==
  /\ srcRetrying /\ retryIn > 0 /\ retryIn' = retryIn - 1
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcRetrying, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 attempts, fresh, outage, causeless>>

\* `take_due_reconnects` then `reconcile`: the same endpoint, a new source.
Reconnect ==
  /\ srcRetrying /\ retryIn = 0 /\ ~dirty /\ peer = srcEndpoint /\ CanStart
  /\ StartSource(srcEndpoint, status) /\ pending' = FALSE /\ chan' = chan
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, attempts, outage, causeless>>

System == Reconcile \/ ArmOne \/ ArmDone \/ StreamFails \/ StreamRefused \/ Renew
          \/ ArmReplacement \/ Swap \/ Notice \/ StartLoad \/ FinishLoad
          \/ Tick \/ Reconnect

Env == (\E p \in Peers : MovePeer(p)) \/ ToggleGrant \/ ToggleReachable

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
  /\ WF_vars(Tick) /\ WF_vars(Reconnect)

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

\* Bounded reconnect: once the runtime recovers, the source waits at most
\* `Cap` ticks before it dials again.
ReconnectBounded == srcRetrying => retryIn <= Cap

Reachable == peer \in Endpoints /\ reachable

\* A recovered runtime is eventually re-armed (backoff delays, never stops,
\* the reconnect), and its view eventually reads connected whatever the User
\* may read there, unless it refuses the User.
RecoveredEventuallyArmed ==
  <>[](Reachable /\ (grant \/ Revocation = "served"))
    => <>(tPhase = "streaming" /\ tArmed = K)

EventuallyConnected ==
  <>[](Reachable /\ (grant \/ Revocation = "served")) => <>[](status = "connected")

\* A runtime that refuses a revoked User reads as a reconnect in progress,
\* never as an outage or a denial.
RefusalReadsAsConnecting ==
  <>[](Reachable /\ ~grant /\ Revocation # "served") => <>[](status = "connecting")

\* A peer without an endpoint is never shown as a connection attempt.
NoEndpointEventuallyNamed ==
  <>[](peer = "none") => <>[](status = "noEndpoint")

====
