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
(*    transport failure or a denial;                                        *)
(*  - the Observer loop (observe.rs): applies notices of the current        *)
(*    generation, runs one snapshot load at a time, re-probes a denied      *)
(*    source after a bounded delay;                                         *)
(*  - Reconcile: a peer directory change (endpoint added, changed or        *)
(*    removed, peer removed) replaces or drops the source.                  *)
(* The environment revokes/restores the User's grant and takes the runtime  *)
(* offline/online a bounded number of times. `Revocation` is how the        *)
(* runtime answers a revoked User: "served" is the pinned served runtime    *)
(* (handshake accepted, streams silent, a read answers with the selected    *)
(* roots omitted; observe tests `revoked_user_is_answered_without_a_        *)
(* classified_denial`); "handshake" (HTTP 403 at the handshake and on a     *)
(* read) and "envelope" (a PERMISSION_DENIED envelope on an open stream and *)
(* HTTP 403 on a read) are the access owner's other classified surfaces,    *)
(* checked so a runtime that adopts them stays correct.                     *)
(*                                                                         *)
(* Generations are compared only for equality, so a new source takes the   *)
(* smallest generation no notice or load still names (`FreshGen`).         *)
(* A tokio abort lands at the task's next await, so an aborted task may     *)
(* still enqueue one notice (`Abort`).                                      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  K,                  \* subscriptions armed per connection
  MaxGen,             \* generation labels (only equality matters)
  Cap,                \* reprobe cap, in ticks (Lean reprobeCapMs)
  MaxEnv,             \* environment changes (grant, reachability, endpoint)
  MaxChan,            \* notice channel bound for TLC
  Revocation,         \* "served" | "handshake" | "envelope" (see header)
  RenewalAsFailure,   \* RED: planned renewal reported as a transport failure
  MakeBeforeBreak,    \* FALSE: a renewal drops the armed set before re-arming
  WithheldClassified, \* FALSE: a read with withheld roots is a transport failure
  NamedNoEndpoint,    \* FALSE: a peer without an endpoint has no status
  GenerationCheck,    \* FALSE: notices of a replaced source are applied
  ArmBeforeReady      \* FALSE: ready is sent before every subscription is armed

Endpoints == {"e1", "e2"}
Peers == {"absent", "none"} \cup Endpoints
Statuses == {"unset", "connecting", "connected", "disconnected", "denied", "noEndpoint"}
Kinds == {"ready", "renewed", "failed", "denied"}
Phases == {"idle", "arming", "streaming", "retry", "ended"}
Gens == 1..MaxGen

VARIABLES
  peer, grant, reachable, env, dirty,      \* environment and peer directory
  srcGen, srcEndpoint, srcReady, srcDenied, \* Sources entry (0 = none)
  tPhase, tArmed, tRenewing, tNew,          \* the live source task
  chan,                                     \* notice channel
  status,                                   \* ObservedStore runtime_connection
  pending, load,                            \* observer snapshot scheduling
  denials, reprobeIn,                       \* re-probe schedule
  fresh, outage                             \* ghosts

vars == <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
          srcDenied, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
          denials, reprobeIn, fresh, outage>>

NoLoad == [gen |-> 0, afterArm |-> FALSE]

Min(S) == CHOOSE x \in S : \A y \in S : x <= y

ReprobeDelay(n) == IF 2 ^ n < Cap THEN 2 ^ n ELSE Cap

UsedGens == {chan[i].gen : i \in 1..Len(chan)} \cup {load.gen, srcGen}

FreshGen == Min(Gens \ UsedGens)

CanStart == Gens \ UsedGens # {}

TypeOK ==
  /\ peer \in Peers /\ grant \in BOOLEAN /\ reachable \in BOOLEAN
  /\ env \in 0..MaxEnv /\ dirty \in BOOLEAN
  /\ srcGen \in 0..MaxGen /\ srcEndpoint \in Endpoints \cup {"none"}
  /\ srcReady \in BOOLEAN /\ srcDenied \in BOOLEAN
  /\ tPhase \in Phases /\ tArmed \in 0..K /\ tRenewing \in BOOLEAN /\ tNew \in 0..K
  /\ \A i \in 1..Len(chan) : chan[i].gen \in Gens /\ chan[i].kind \in Kinds
  /\ status \in Statuses /\ pending \in BOOLEAN
  /\ load.gen \in 0..MaxGen /\ load.afterArm \in BOOLEAN
  /\ denials \in 0..Cap /\ reprobeIn \in 0..Cap
  /\ fresh \in BOOLEAN /\ outage \in BOOLEAN

Init ==
  /\ peer \in Endpoints /\ grant = TRUE /\ reachable = TRUE /\ env = 0
  /\ dirty = TRUE
  /\ srcGen = 0 /\ srcEndpoint = "none" /\ srcReady = FALSE /\ srcDenied = FALSE
  /\ tPhase = "idle" /\ tArmed = 0 /\ tRenewing = FALSE /\ tNew = 0
  /\ chan = <<>> /\ status = "unset" /\ pending = FALSE /\ load = NoLoad
  /\ denials = 0 /\ reprobeIn = 0 /\ fresh = FALSE /\ outage = FALSE

Send(kind) == chan' = Append(chan, [gen |-> srcGen, kind |-> kind])

(* --------------------------- environment ------------------------------ *)

EnvStep == env < MaxEnv /\ env' = env + 1

MovePeer(p) ==
  /\ EnvStep /\ p # peer /\ peer' = p /\ dirty' = TRUE
  /\ UNCHANGED <<grant, reachable, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 denials, reprobeIn, fresh, outage>>

ToggleGrant ==
  /\ EnvStep /\ grant' = ~grant
  /\ UNCHANGED <<peer, reachable, dirty, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 denials, reprobeIn, fresh, outage>>

ToggleReachable ==
  /\ EnvStep /\ reachable' = ~reachable
  /\ outage' = (outage \/ reachable)
  /\ UNCHANGED <<peer, grant, dirty, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 denials, reprobeIn, fresh>>

(* ----------------------------- reconcile ------------------------------ *)

\* Dropping a source aborts its task; one already-running poll may still
\* enqueue its next notice.
DropSource(staleKind) ==
  /\ \/ chan' = chan
     \/ /\ srcGen # 0 /\ tPhase \in {"arming", "streaming"}
        /\ Len(chan) < MaxChan
        /\ staleKind \in Kinds
        /\ chan' = Append(chan, [gen |-> srcGen, kind |-> staleKind])

StartSource(endpoint) ==
  /\ srcGen' = FreshGen /\ srcEndpoint' = endpoint
  /\ srcReady' = FALSE /\ srcDenied' = FALSE
  /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = FALSE /\ tNew' = 0
  /\ status' = "connecting" /\ reprobeIn' = 0 /\ fresh' = FALSE
  /\ load' = NoLoad

Keep == srcGen # 0 /\ srcEndpoint = peer /\ (srcDenied \/ tPhase # "ended")

Reconcile ==
  /\ dirty /\ dirty' = FALSE
  /\ IF peer \in Endpoints
       THEN IF Keep
              THEN UNCHANGED <<srcGen, srcEndpoint, srcReady, srcDenied, tPhase,
                               tArmed, tRenewing, tNew, chan, status, load, reprobeIn,
                               fresh, pending>>
              ELSE /\ CanStart
                   /\ \E k \in Kinds \cup {"none"} : DropSource(k)
                   /\ StartSource(peer)
                   /\ pending' = FALSE
       ELSE /\ \E k \in Kinds \cup {"none"} : DropSource(k)
            /\ srcGen' = 0 /\ srcEndpoint' = "none" /\ srcReady' = FALSE
            /\ srcDenied' = FALSE /\ tPhase' = "idle" /\ tArmed' = 0
            /\ tRenewing' = FALSE /\ tNew' = 0 /\ load' = NoLoad /\ pending' = FALSE
            /\ reprobeIn' = 0 /\ fresh' = FALSE
            /\ status' = IF peer = "none" /\ NamedNoEndpoint THEN "noEndpoint" ELSE "unset"
  /\ denials' = IF peer \in Endpoints THEN denials ELSE 0
  /\ UNCHANGED <<peer, grant, reachable, env, outage>>

(* ---------------------------- source task ----------------------------- *)

TaskUnchanged == UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                             srcReady, srcDenied, status, pending, load, denials,
                             reprobeIn, outage>>

\* The task ends after a failure notice and re-arms after the fixed
\* transport retry delay; after a denial it ends for good. A failure drops
\* every set, including a replacement still arming.
Fail(kind) ==
  /\ Len(chan) < MaxChan /\ Send(kind) /\ fresh' = FALSE
  /\ tPhase' = IF kind = "denied" THEN "ended" ELSE "retry"
  /\ tArmed' = 0 /\ tRenewing' = FALSE /\ tNew' = 0

\* One handshake: refused when unreachable, or for a revoked User only when
\* the runtime refuses at the handshake.
Handshake(armed) ==
  IF ~reachable THEN Fail("failed")
  ELSE IF ~grant /\ Revocation = "handshake" THEN Fail("denied")
  ELSE armed

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
  /\ UNCHANGED <<tArmed, tNew, fresh>>
  /\ TaskUnchanged

StreamFails ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~reachable
  /\ Fail("failed") /\ TaskUnchanged

\* A runtime that answers an open stream of a revoked User with a GraphQL
\* errors envelope; the served runtime keeps it silent instead.
StreamDenied ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~grant /\ reachable
  /\ Revocation = "envelope"
  /\ Fail("denied")
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
            /\ UNCHANGED <<chan, tPhase, tArmed, fresh>>
       ELSE /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = TRUE
            /\ fresh' = FALSE /\ UNCHANGED <<chan, tNew>>
  /\ TaskUnchanged

ArmReplacement ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew < K
  /\ Handshake(tNew' = tNew + 1 /\ UNCHANGED <<chan, tPhase, tArmed, tRenewing, fresh>>)
  /\ TaskUnchanged

\* The replacement is armed: the old set is dropped and `renewed` is sent so
\* the observer resnapshots, the only read that reveals a revocation on the
\* served runtime.
Swap ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ tRenewing /\ tNew = K
  /\ Len(chan) < MaxChan /\ Send("renewed")
  /\ tRenewing' = FALSE /\ tNew' = 0
  /\ UNCHANGED <<tPhase, tArmed, fresh>>
  /\ TaskUnchanged

Retry ==
  /\ srcGen # 0 /\ tPhase = "retry"
  /\ tPhase' = "arming" /\ tArmed' = 0
  /\ UNCHANGED <<chan, tRenewing, tNew, fresh>>
  /\ TaskUnchanged

(* ------------------------------ observer ------------------------------ *)

Denied ==
  /\ srcReady' = FALSE /\ srcDenied' = TRUE /\ status' = "denied"
  /\ load' = NoLoad /\ pending' = FALSE
  /\ reprobeIn' = ReprobeDelay(denials)
  /\ denials' = IF denials < Cap THEN denials + 1 ELSE denials

Notice ==
  /\ Len(chan) > 0
  /\ LET n == Head(chan) IN
     /\ chan' = Tail(chan)
     /\ IF srcGen # 0 /\ ~srcDenied /\ (n.gen = srcGen \/ ~GenerationCheck)
          THEN CASE n.kind = "ready" ->
                      /\ srcReady' = TRUE /\ status' = "connecting" /\ pending' = TRUE
                      /\ UNCHANGED <<srcDenied, load, reprobeIn, denials>>
                 [] n.kind = "renewed" ->
                      /\ srcReady' = TRUE /\ pending' = TRUE
                      /\ UNCHANGED <<status, srcDenied, load, reprobeIn, denials>>
                 [] n.kind = "failed" ->
                      /\ srcReady' = FALSE /\ status' = "disconnected" /\ load' = NoLoad
                      /\ UNCHANGED <<srcDenied, pending, reprobeIn, denials>>
                 [] n.kind = "denied" -> Denied
          ELSE UNCHANGED <<srcReady, srcDenied, status, pending, load, reprobeIn, denials>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, tPhase,
                 tArmed, tRenewing, tNew, fresh, outage>>

StartLoad ==
  /\ pending /\ srcReady /\ ~srcDenied /\ load.gen = 0 /\ srcGen # 0
  /\ load' = [gen |-> srcGen, afterArm |-> (tPhase = "streaming" /\ tArmed = K)]
  /\ pending' = FALSE
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcDenied, tPhase, tArmed, tRenewing, tNew, chan, status, denials,
                 reprobeIn, fresh, outage>>

\* A revoked User's read is refused (HTTP 403) or, on the served runtime,
\* answered with the selected roots withheld; the observer classifies both
\* as a denial (Lean `snapshotEvent`) unless `WithheldClassified` is FALSE.
FinishLoad ==
  /\ load.gen # 0 /\ load.gen = srcGen
  /\ IF ~reachable \/ (~grant /\ Revocation = "served" /\ ~WithheldClassified)
       THEN /\ status' = "disconnected" /\ pending' = TRUE /\ load' = NoLoad
            /\ UNCHANGED <<srcReady, srcDenied, reprobeIn, denials, fresh, tPhase>>
       ELSE IF ~grant
       \* A denied snapshot aborts the source task.
       THEN /\ Denied /\ tPhase' = "ended" /\ UNCHANGED fresh
       ELSE /\ status' = "connected" /\ load' = NoLoad /\ denials' = 0
            /\ fresh' = (fresh \/ load.afterArm)
            /\ UNCHANGED <<srcReady, srcDenied, pending, reprobeIn, tPhase>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                 tArmed, tRenewing, tNew, chan, outage>>

Tick ==
  /\ srcDenied /\ reprobeIn > 0 /\ reprobeIn' = reprobeIn - 1
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcDenied, tPhase, tArmed, tRenewing, tNew, chan, status, pending, load,
                 denials, fresh, outage>>

\* `take_due_reprobes` then `reconcile`: the same endpoint, a new source.
Reprobe ==
  /\ srcDenied /\ reprobeIn = 0 /\ ~dirty /\ peer = srcEndpoint /\ CanStart
  /\ StartSource(srcEndpoint) /\ pending' = FALSE /\ chan' = chan
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, denials, outage>>

System == Reconcile \/ ArmOne \/ ArmDone \/ StreamFails \/ StreamDenied \/ Renew
          \/ ArmReplacement \/ Swap \/ Retry \/ Notice \/ StartLoad \/ FinishLoad
          \/ Tick \/ Reprobe

Env == (\E p \in Peers : MovePeer(p)) \/ ToggleGrant \/ ToggleReachable

Next == System \/ Env

\* Every system step is weakly fair. Renewal is a timer the task cannot
\* skip: a streaming source renews forever, which is how a revocation reaches
\* an observer of the served runtime. Lean `renewalStartMs` spaces renewals
\* a full period after each completed arm; durations are not modeled here.
Fairness ==
  /\ WF_vars(Reconcile) /\ WF_vars(ArmOne) /\ WF_vars(ArmDone)
  /\ WF_vars(StreamFails) /\ WF_vars(StreamDenied) /\ WF_vars(Renew)
  /\ WF_vars(ArmReplacement) /\ WF_vars(Swap) /\ WF_vars(Retry)
  /\ WF_vars(Notice) /\ WF_vars(StartLoad) /\ WF_vars(FinishLoad)
  /\ WF_vars(Tick) /\ WF_vars(Reprobe)

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

Stable == peer \in Endpoints /\ grant /\ reachable

EventuallyConnected == <>[]Stable => <>[](status = "connected")

RevokedStable == peer \in Endpoints /\ ~grant /\ reachable

\* With the grant revoked, the source keeps showing the denial (re-probes
\* pass through `connecting` only transiently).
DenialObserved == <>[]RevokedStable => []<>(status = "denied")

\* A peer without an endpoint is never shown as a connection attempt.
NoEndpointEventuallyNamed ==
  <>[](peer = "none") => <>[](status = "noEndpoint")

\* The re-probe of a denied source waits at most `Cap` ticks.
ReprobeBounded == srcDenied => reprobeIn <= Cap

====
