---- MODULE RuntimeConnectionTransport ----
(***************************************************************************)
(* The desktop observer's connection to one runtime it reads over           *)
(* authenticated HTTP as the User (gents-desktop-core client/observe).      *)
(*                                                                         *)
(* Three processes share the observer state:                               *)
(*  - the Source task (remote.rs `observe`/`connected`): arms K SSE         *)
(*    subscriptions one at a time, then sends a ready notice; a stream ends *)
(*    by a planned bearer renewal, a transport failure or a denial;         *)
(*  - the Observer loop (observe.rs): applies notices of the current        *)
(*    generation, runs one snapshot load at a time, re-probes a denied      *)
(*    source after a bounded delay;                                         *)
(*  - Reconcile: a peer directory change (endpoint added, changed or        *)
(*    removed, peer removed) replaces or drops the source.                  *)
(* The environment revokes/restores the User's grant and takes the runtime  *)
(* offline/online a bounded number of times.                                *)
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
  RenewalAsFailure,   \* RED: planned renewal reported as a transport failure
  EnvelopeClassified, \* FALSE: an errors-envelope denial is a transport failure
  DenyAtHandshake,    \* TRUE: a revoked grant is refused at the SSE handshake
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
  tPhase, tArmed, tRenewing,                \* the live source task
  chan,                                     \* notice channel
  status,                                   \* ObservedStore runtime_connection
  pending, load,                            \* observer snapshot scheduling
  denials, reprobeIn,                       \* re-probe schedule
  fresh, outage                             \* ghosts

vars == <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
          srcDenied, tPhase, tArmed, tRenewing, chan, status, pending, load,
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
  /\ tPhase \in Phases /\ tArmed \in 0..K /\ tRenewing \in BOOLEAN
  /\ \A i \in 1..Len(chan) : chan[i].gen \in Gens /\ chan[i].kind \in Kinds
  /\ status \in Statuses /\ pending \in BOOLEAN
  /\ load.gen \in 0..MaxGen /\ load.afterArm \in BOOLEAN
  /\ denials \in 0..Cap /\ reprobeIn \in 0..Cap
  /\ fresh \in BOOLEAN /\ outage \in BOOLEAN

Init ==
  /\ peer \in Endpoints /\ grant = TRUE /\ reachable = TRUE /\ env = 0
  /\ dirty = TRUE
  /\ srcGen = 0 /\ srcEndpoint = "none" /\ srcReady = FALSE /\ srcDenied = FALSE
  /\ tPhase = "idle" /\ tArmed = 0 /\ tRenewing = FALSE
  /\ chan = <<>> /\ status = "unset" /\ pending = FALSE /\ load = NoLoad
  /\ denials = 0 /\ reprobeIn = 0 /\ fresh = FALSE /\ outage = FALSE

Send(kind) == chan' = Append(chan, [gen |-> srcGen, kind |-> kind])

(* --------------------------- environment ------------------------------ *)

EnvStep == env < MaxEnv /\ env' = env + 1

MovePeer(p) ==
  /\ EnvStep /\ p # peer /\ peer' = p /\ dirty' = TRUE
  /\ UNCHANGED <<grant, reachable, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, chan, status, pending, load,
                 denials, reprobeIn, fresh, outage>>

ToggleGrant ==
  /\ EnvStep /\ grant' = ~grant
  /\ UNCHANGED <<peer, reachable, dirty, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, chan, status, pending, load,
                 denials, reprobeIn, fresh, outage>>

ToggleReachable ==
  /\ EnvStep /\ reachable' = ~reachable
  /\ outage' = (outage \/ reachable)
  /\ UNCHANGED <<peer, grant, dirty, srcGen, srcEndpoint, srcReady, srcDenied,
                 tPhase, tArmed, tRenewing, chan, status, pending, load,
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
  /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = FALSE
  /\ status' = "connecting" /\ reprobeIn' = 0 /\ fresh' = FALSE
  /\ load' = NoLoad

Keep == srcGen # 0 /\ srcEndpoint = peer /\ (srcDenied \/ tPhase # "ended")

Reconcile ==
  /\ dirty /\ dirty' = FALSE
  /\ IF peer \in Endpoints
       THEN IF Keep
              THEN UNCHANGED <<srcGen, srcEndpoint, srcReady, srcDenied, tPhase,
                               tArmed, tRenewing, chan, status, load, reprobeIn,
                               fresh, pending>>
              ELSE /\ CanStart
                   /\ \E k \in Kinds \cup {"none"} : DropSource(k)
                   /\ StartSource(peer)
                   /\ pending' = FALSE
       ELSE /\ \E k \in Kinds \cup {"none"} : DropSource(k)
            /\ srcGen' = 0 /\ srcEndpoint' = "none" /\ srcReady' = FALSE
            /\ srcDenied' = FALSE /\ tPhase' = "idle" /\ tArmed' = 0
            /\ tRenewing' = FALSE /\ load' = NoLoad /\ pending' = FALSE
            /\ reprobeIn' = 0 /\ fresh' = FALSE
            /\ status' = IF peer = "none" /\ NamedNoEndpoint THEN "noEndpoint" ELSE "unset"
  /\ denials' = IF peer \in Endpoints THEN denials ELSE 0
  /\ UNCHANGED <<peer, grant, reachable, env, outage>>

(* ---------------------------- source task ----------------------------- *)

TaskUnchanged == UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                             srcReady, srcDenied, status, pending, load, denials,
                             reprobeIn, outage>>

\* The task ends after a failure notice and re-arms after the fixed
\* transport retry delay; after a denial it ends for good.
Fail(kind) ==
  /\ Len(chan) < MaxChan /\ Send(kind) /\ fresh' = FALSE
  /\ tPhase' = IF kind = "denied" THEN "ended" ELSE "retry"
  /\ tArmed' = 0 /\ tRenewing' = FALSE

ArmOne ==
  /\ srcGen # 0 /\ tPhase = "arming" /\ tArmed < K
  /\ IF ~reachable THEN Fail("failed")
     ELSE IF ~grant /\ DenyAtHandshake THEN Fail("denied")
     ELSE /\ tArmed' = tArmed + 1 /\ UNCHANGED <<chan, tPhase, tRenewing, fresh>>
  /\ TaskUnchanged

ArmDone ==
  /\ srcGen # 0 /\ tPhase = "arming"
  /\ IF ArmBeforeReady THEN tArmed = K ELSE TRUE
  /\ Len(chan) < MaxChan
  /\ Send(IF tRenewing THEN "renewed" ELSE "ready")
  /\ tPhase' = "streaming" /\ tRenewing' = FALSE
  /\ UNCHANGED <<tArmed, fresh>>
  /\ TaskUnchanged

StreamFails ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~reachable
  /\ Fail("failed") /\ TaskUnchanged

\* The runtime answers an open stream with a GraphQL errors envelope.
StreamDenied ==
  /\ srcGen # 0 /\ tPhase = "streaming" /\ ~grant /\ reachable
  /\ Fail(IF EnvelopeClassified THEN "denied" ELSE "failed")
  /\ TaskUnchanged

\* Half the bearer lifetime elapsed: the stream ends to re-authenticate.
\* Changes committed before the new subscriptions are armed are not
\* delivered, so the shown data is fresh again only after a resnapshot.
Renew ==
  /\ srcGen # 0 /\ tPhase = "streaming"
  /\ IF RenewalAsFailure
       THEN Fail("failed")
       ELSE /\ tPhase' = "arming" /\ tArmed' = 0 /\ tRenewing' = TRUE
            /\ fresh' = FALSE /\ UNCHANGED chan
  /\ TaskUnchanged

Retry ==
  /\ srcGen # 0 /\ tPhase = "retry"
  /\ tPhase' = "arming" /\ tArmed' = 0
  /\ UNCHANGED <<chan, tRenewing, fresh>>
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
                 tArmed, tRenewing, fresh, outage>>

StartLoad ==
  /\ pending /\ srcReady /\ ~srcDenied /\ load.gen = 0 /\ srcGen # 0
  /\ load' = [gen |-> srcGen, afterArm |-> (tPhase = "streaming" /\ tArmed = K)]
  /\ pending' = FALSE
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcDenied, tPhase, tArmed, tRenewing, chan, status, denials,
                 reprobeIn, fresh, outage>>

\* A snapshot read is refused (HTTP 403) when the grant is revoked; the
\* observer's snapshot classification is the owner classifier.
FinishLoad ==
  /\ load.gen # 0 /\ load.gen = srcGen
  /\ IF ~reachable
       THEN /\ status' = "disconnected" /\ pending' = TRUE /\ load' = NoLoad
            /\ UNCHANGED <<srcReady, srcDenied, reprobeIn, denials, fresh, tPhase>>
       ELSE IF ~grant
       \* A denied snapshot aborts the source task.
       THEN /\ Denied /\ tPhase' = "ended" /\ UNCHANGED fresh
       ELSE /\ status' = "connected" /\ load' = NoLoad /\ denials' = 0
            /\ fresh' = (fresh \/ load.afterArm)
            /\ UNCHANGED <<srcReady, srcDenied, pending, reprobeIn, tPhase>>
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint,
                 tArmed, tRenewing, chan, outage>>

Tick ==
  /\ srcDenied /\ reprobeIn > 0 /\ reprobeIn' = reprobeIn - 1
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, srcGen, srcEndpoint, srcReady,
                 srcDenied, tPhase, tArmed, tRenewing, chan, status, pending, load,
                 denials, fresh, outage>>

\* `take_due_reprobes` then `reconcile`: the same endpoint, a new source.
Reprobe ==
  /\ srcDenied /\ reprobeIn = 0 /\ ~dirty /\ peer = srcEndpoint /\ CanStart
  /\ StartSource(srcEndpoint) /\ pending' = FALSE /\ chan' = chan
  /\ UNCHANGED <<peer, grant, reachable, env, dirty, denials, outage>>

System == Reconcile \/ ArmOne \/ ArmDone \/ StreamFails \/ StreamDenied \/ Renew
          \/ Retry \/ Notice \/ StartLoad \/ FinishLoad \/ Tick \/ Reprobe

Env == (\E p \in Peers : MovePeer(p)) \/ ToggleGrant \/ ToggleReachable

Next == System \/ Env

\* Renewal is a timer the task cannot skip; every system step is weakly fair.
Fairness ==
  /\ WF_vars(Reconcile) /\ WF_vars(ArmOne) /\ WF_vars(ArmDone)
  /\ WF_vars(StreamFails) /\ WF_vars(StreamDenied) /\ WF_vars(Retry)
  /\ WF_vars(Notice) /\ WF_vars(StartLoad) /\ WF_vars(FinishLoad)
  /\ WF_vars(Tick) /\ WF_vars(Reprobe)

Spec == Init /\ [][Next]_vars /\ Fairness

(* ----------------------------- properties ----------------------------- *)

\* The observer still owes a post-arm snapshot or a status change: a renewal
\* is re-arming, its notice or an end notice of this source is queued, a
\* resnapshot is scheduled, or a load that began after the arm is running.
Owed ==
  \/ tRenewing
  \/ \E i \in 1..Len(chan) : chan[i].gen = srcGen /\ chan[i].kind # "ready"
  \/ pending /\ srcReady
  \/ load.gen = srcGen /\ load.afterArm

\* Connected only when the shown data comes from a snapshot that began after
\* every subscription of the current arm was armed, or the observer still
\* owes one: a renewal that races a snapshot is followed by a resnapshot.
ConnectedAfterArmedSnapshot == status = "connected" => srcGen # 0 /\ (fresh \/ Owed)

\* A notice of a replaced source never moves the status.
StaleNoticeInert ==
  [][(Len(chan) > 0 /\ chan' = Tail(chan) /\ Head(chan).gen # srcGen /\ srcGen' = srcGen)
      => status' = status]_vars

\* A removed peer has no source and no status.
RemovalClears == (peer = "absent" /\ ~dirty) => (srcGen = 0 /\ status = "unset")

\* A peer with no endpoint shows that, once reconciled.
NoEndpointNamed == (peer = "none" /\ ~dirty) => status = "noEndpoint"

\* `disconnected` is shown only after the runtime was actually unreachable.
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
