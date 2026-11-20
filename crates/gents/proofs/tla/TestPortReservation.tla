---- MODULE TestPortReservation ----
\* lean: none
\* crates: gents, gents-cli
(***************************************************************************)
(* Two gents-cli integration tests on one host, each in its own test       *)
(* process (nextest runs every test in a process of its own), each taking  *)
(* a port from `allocate_port` (gents `tests/support/ports.rs`), starting  *)
(* a `gents server` on it and waiting for it (`wait_for_port`, gents-cli   *)
(* `tests/support/process.rs`). They may belong to two CI jobs on two      *)
(* runners of one machine, which share the host's network namespace but  *)
(* not their filesystem view: TMPDIR differs per runner, and each runner's *)
(* systemd service has `PrivateTmp=yes`, so `/tmp` differs too.            *)
(*                                                                          *)
(* The allocator (ports.rs `allocate_port`), per candidate port:           *)
(*   Lock   - take the port's reservation in `Namespace(t)`; a held one    *)
(*            skips the port. A lock file under `std::env::temp_dir()`     *)
(*            (TMPDIR) or under `/tmp` (PrivateTmp) is per runner          *)
(*            (`SharedNamespace = FALSE`); an abstract Unix socket lives   *)
(*            in the network namespace, shared exactly where ports are.   *)
(*   Probe  - bind the port and drop the listener; a bound port releases   *)
(*            the lock and skips. The lock is kept for the process's life. *)
(* Every process's cursor starts at the first port of the range, so under  *)
(* nextest concurrent tests all ask for the same few ports.                *)
(*                                                                          *)
(* The server child binds the port at startup (gents-host `serve.rs`, the  *)
(* "embedded HTTP listener cannot bind" error) or exits. `wait_for_port`   *)
(* (process.rs) returns ready once a TCP connect to the port succeeds,     *)
(* whoever listens, and fails only when its own child has exited. The     *)
(* test then reads and subscribes through that port; when the listener's   *)
(* test ends, its server is killed and a subscription through it sees EOF  *)
(* ("document subscription failed ... unexpected EOF"), and a command      *)
(* addressed to it reaches another node ("session ... was not found on     *)
(* node ...").                                                              *)
(*                                                                          *)
(* `Outbound` is any process's outgoing connection taking a free port as   *)
(* its local port. Linux hands those out from ip_local_port_range          *)
(* (32768-60999 by default) and macOS from 49152-65535; it binds without  *)
(* listening, so a server bind there fails and a connect is refused. With *)
(* `RangeBelowEphemeral` the range stays below both and this cannot happen.*)
(*                                                                          *)
(* `NoCrossTalk` is what every such test assumes: the server that answers  *)
(* on its port is its own. `OwnServerBinds` is that the reserved port is   *)
(* still free when its server binds it. Both hold for one shared         *)
(* namespace below the ephemeral range (green). RED: `PerRunner` is the    *)
(* code before the fix and the failure seen on the shared Linux runners    *)
(* (both jobs' failures fall in the same 33 seconds); `EphemeralOverlap`  *)
(* keeps the shared namespace but lets an outbound connection take the     *)
(* reserved port. `PerRunnerSpread` starts the second process's cursor     *)
(* elsewhere, as one `cargo test` process per binary effectively did:      *)
(* why the per-TMPDIR directory rarely failed before nextest.              *)
(***************************************************************************)

EXTENDS Naturals

CONSTANTS
  Tests,                \* concurrent test processes on one host
  Ports,                \* the allocator's range, in cursor order 1..N
  SharedNamespace,      \* every process locks in one host-wide directory
  RangeBelowEphemeral,  \* no outbound connection is given a port in the range
  CursorStart           \* [Tests -> Ports]: where each process's cursor starts

None == "none"
Out == "outbound"

VARIABLES
  lock,      \* [Namespaces \X Ports -> Tests \cup {None}]
  listener,  \* [Ports -> Tests \cup {None, Out}]: whose socket holds the port
  pc,        \* per test: alloc | locked | reserved | spawned | ready | failed | done | exhausted
  cursor,    \* per test: next candidate port
  port,      \* per test: the port it reserved
  srv,       \* per test: none | starting | listening | exited
  peer       \* per test: whose server answered its readiness check

vars == <<lock, listener, pc, cursor, port, srv, peer>>

Namespace(t) == IF SharedNamespace THEN "host" ELSE t
Namespaces == {Namespace(t) : t \in Tests}
MaxPort == CHOOSE p \in Ports : \A q \in Ports : q <= p

TypeOK ==
  /\ lock \in [Namespaces \X Ports -> Tests \cup {None}]
  /\ listener \in [Ports -> Tests \cup {None, Out}]
  /\ pc \in [Tests -> {"alloc", "locked", "reserved", "spawned", "ready", "failed",
                       "done", "exhausted"}]
  /\ cursor \in [Tests -> 1..(MaxPort + 1)]
  /\ port \in [Tests -> Ports \cup {0}]
  /\ srv \in [Tests -> {"none", "starting", "listening", "exited"}]
  /\ peer \in [Tests -> Tests \cup {None}]

Init ==
  /\ lock = [k \in Namespaces \X Ports |-> None]
  /\ listener = [p \in Ports |-> None]
  /\ pc = [t \in Tests |-> "alloc"]
  /\ cursor = CursorStart
  /\ port = [t \in Tests |-> 0]
  /\ srv = [t \in Tests |-> "none"]
  /\ peer = [t \in Tests |-> None]

Advance(t) ==
  /\ cursor' = [cursor EXCEPT ![t] = cursor[t] + 1]
  /\ pc' = [pc EXCEPT ![t] = IF cursor[t] + 1 > MaxPort THEN "exhausted" ELSE "alloc"]

\* ports.rs: open `<dir>/<port>` and `try_lock`; `WouldBlock` skips the port.
Lock(t) ==
  /\ pc[t] = "alloc"
  /\ LET k == <<Namespace(t), cursor[t]>> IN
       IF lock[k] = None
       THEN /\ lock' = [lock EXCEPT ![k] = t]
            /\ port' = [port EXCEPT ![t] = cursor[t]]
            /\ pc' = [pc EXCEPT ![t] = "locked"]
            /\ UNCHANGED cursor
       ELSE /\ Advance(t)
            /\ UNCHANGED <<lock, port>>
  /\ UNCHANGED <<listener, srv, peer>>

\* ports.rs: `TcpListener::bind` then drop. On failure the reservation file
\* goes out of scope, which releases its lock, and the cursor moves on.
Probe(t) ==
  /\ pc[t] = "locked"
  /\ IF listener[port[t]] = None
     THEN /\ pc' = [pc EXCEPT ![t] = "reserved"]
          /\ UNCHANGED <<lock, cursor, port>>
     ELSE /\ lock' = [lock EXCEPT ![<<Namespace(t), port[t]>>] = None]
          /\ port' = [port EXCEPT ![t] = 0]
          /\ Advance(t)
  /\ UNCHANGED <<listener, srv, peer>>

\* process.rs `spawn_server`: the child starts with `--http-port <port>`.
Spawn(t) ==
  /\ pc[t] = "reserved"
  /\ pc' = [pc EXCEPT ![t] = "spawned"]
  /\ srv' = [srv EXCEPT ![t] = "starting"]
  /\ UNCHANGED <<lock, listener, cursor, port, peer>>

\* serve.rs: bind the HTTP listener or exit with "cannot bind".
ServerBind(t) ==
  /\ srv[t] = "starting"
  /\ IF listener[port[t]] = None
     THEN /\ listener' = [listener EXCEPT ![port[t]] = t]
          /\ srv' = [srv EXCEPT ![t] = "listening"]
     ELSE /\ srv' = [srv EXCEPT ![t] = "exited"]
          /\ UNCHANGED listener
  /\ UNCHANGED <<lock, pc, cursor, port, peer>>

\* process.rs `wait_for_port`: a successful connect is readiness, from any
\* listening socket; otherwise its own child's exit is a failure.
WaitForPort(t) ==
  /\ pc[t] = "spawned"
  /\ \/ /\ listener[port[t]] \in Tests
        /\ pc' = [pc EXCEPT ![t] = "ready"]
        /\ peer' = [peer EXCEPT ![t] = listener[port[t]]]
     \/ /\ listener[port[t]] \notin Tests
        /\ srv[t] = "exited"
        /\ pc' = [pc EXCEPT ![t] = "failed"]
        /\ UNCHANGED peer
  /\ UNCHANGED <<lock, listener, cursor, port, srv>>

\* The test ends: `ServeProcess::drop` kills and reaps its server, then the
\* process exits and the kernel releases its reservation lock.
Finish(t) ==
  /\ pc[t] \in {"ready", "failed"}
  /\ srv[t] # "starting"
  /\ pc' = [pc EXCEPT ![t] = "done"]
  /\ srv' = [srv EXCEPT ![t] = "exited"]
  /\ listener' = IF listener[port[t]] = t
                 THEN [listener EXCEPT ![port[t]] = None]
                 ELSE listener
  /\ lock' = [lock EXCEPT ![<<Namespace(t), port[t]>>] = None]
  /\ UNCHANGED <<cursor, port, peer>>

\* Some process's outgoing connection is given a free port as its local port.
Outbound(p) ==
  /\ ~RangeBelowEphemeral
  /\ listener[p] = None
  /\ listener' = [listener EXCEPT ![p] = Out]
  /\ UNCHANGED <<lock, pc, cursor, port, srv, peer>>

Next ==
  \/ \E t \in Tests :
       Lock(t) \/ Probe(t) \/ Spawn(t) \/ ServerBind(t) \/ WaitForPort(t) \/ Finish(t)
  \/ \E p \in Ports : Outbound(p)

Spec == Init /\ [][Next]_vars

NoCrossTalk == \A t \in Tests : pc[t] = "ready" => peer[t] = t

OwnServerBinds == \A t \in Tests : pc[t] # "failed"

====
