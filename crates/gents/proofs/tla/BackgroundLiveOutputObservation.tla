---- MODULE BackgroundLiveOutputObservation ----
\* lean: none
\* crates: gents-toolcalls, gents-tools
\* A background process observed mid-run, as
\* `list_background_tools_reports_running_live_output_bytes` drives it: the
\* test spawns a gated child through `spawn_process`, polls `list_processes`
\* until the child's row is running with the 4 bytes of "live", reads the same
\* output through `read_process`, releases the gate, and waits for completion.
\*
\* Child: prints "live", then waits at its gate; once released it prints
\* "done" and exits. `SelfExpire` is any child-side wall-clock end of the
\* gate: the script's bounded wait (200 x 50 ms) or the tool's `timeout_secs`
\* (20 s). Either one ends the gate after `K` ticks whether or not the test
\* has released it. The host's other load decides how many ticks pass between
\* two parent steps.
\*
\* Parent: each poll lists the rows (the runtime reports the output captured
\* so far). `PollCap` is the test's 60 x 100 ms poll budget, after which it
\* fails "gate window closed". The read reports the output at the moment it
\* runs.
\*
\* `ReadAgrees` is what the test asserts at line 572: the read after a
\* running/4 listing returns the same 4 bytes. `NoWindowFailure` and
\* `Completes` are the rest of its promise. The product's answer is right in
\* every state (a list or read reports the current output); only the test's
\* clocks decide whether the gate is still closed when the read runs.
\*
\* `Timed` (both clocks, the code before the fix) violates `ReadAgrees`: the
\* listing sees "live", ticks pass, the gate expires, the child prints "done",
\* and the read returns 8 bytes, the observed `left: Some(8)`. `PollBudget`
\* violates `NoWindowFailure`. Green has neither clock: the gate opens only on
\* the test's release (or the removal of its directory), and the poll ends on
\* the observation or a terminal row.
EXTENDS Naturals

CONSTANTS
  SelfExpire,  \* the child's gate ends on its own after K ticks
  K,
  PollCap,     \* the parent gives up after Cap empty polls
  Cap

ASSUME /\ SelfExpire \in BOOLEAN /\ PollCap \in BOOLEAN
       /\ K \in Nat /\ Cap \in Nat

VARIABLES
  child,     \* spawned | gated | done
  out,       \* bytes of canonical output: 0, 4 ("live"), 8 ("livedone")
  ticks,     \* wall time the child has waited at its gate
  released,
  parent,    \* polling | listed | read | released | waited | failed
  polls,
  readBytes,
  killed     \* the tool timeout ended the child (status failed)

vars == <<child, out, ticks, released, parent, polls, readBytes, killed>>

Init ==
  /\ child = "spawned"
  /\ out = 0
  /\ ticks = 0
  /\ released = FALSE
  /\ parent = "polling"
  /\ polls = 0
  /\ readBytes = 0
  /\ killed = FALSE

ChildPrint ==
  /\ child = "spawned"
  /\ child' = "gated" /\ out' = 4
  /\ UNCHANGED <<ticks, released, parent, polls, readBytes, killed>>

\* Wall time passes while the child waits at its gate.
Tick ==
  /\ SelfExpire /\ child = "gated" /\ ~released /\ ticks < K
  /\ ticks' = ticks + 1
  /\ UNCHANGED <<child, out, released, parent, polls, readBytes, killed>>

ChildFinish ==
  /\ child = "gated"
  /\ released \/ (SelfExpire /\ ticks = K)
  /\ child' = "done" /\ out' = 8
  /\ UNCHANGED <<ticks, released, parent, polls, readBytes, killed>>

\* The tool timeout kills the child at the gate: the row fails with "live".
ChildKilled ==
  /\ SelfExpire /\ child = "gated" /\ ~released /\ ticks = K
  /\ child' = "done" /\ killed' = TRUE
  /\ UNCHANGED <<out, ticks, released, parent, polls, readBytes>>

\* `list_processes`: the row is running with its output so far, or terminal.
Poll ==
  /\ parent = "polling"
  /\ IF child = "gated" /\ out = 4
       THEN /\ parent' = "listed" /\ UNCHANGED polls
       ELSE IF child = "done"
       THEN /\ parent' = "failed" /\ UNCHANGED polls
       ELSE IF PollCap /\ polls + 1 >= Cap
       THEN /\ parent' = "failed" /\ polls' = polls + 1
       ELSE /\ polls' = IF PollCap THEN polls + 1 ELSE polls
            /\ UNCHANGED parent
  /\ UNCHANGED <<child, out, ticks, released, readBytes, killed>>

\* `read_process` reports the output at the moment it runs.
Read ==
  /\ parent = "listed"
  /\ parent' = "read" /\ readBytes' = out
  /\ UNCHANGED <<child, out, ticks, released, polls, killed>>

Release ==
  /\ parent = "read"
  /\ parent' = "released" /\ released' = TRUE
  /\ UNCHANGED <<child, out, ticks, polls, readBytes, killed>>

Wait ==
  /\ parent = "released" /\ child = "done"
  /\ parent' = "waited"
  /\ UNCHANGED <<child, out, ticks, released, polls, readBytes, killed>>

Next == ChildPrint \/ Tick \/ ChildFinish \/ ChildKilled \/ Poll \/ Read \/ Release \/ Wait

Spec == Init /\ [][Next]_vars
          /\ WF_vars(ChildPrint) /\ WF_vars(ChildFinish)
          /\ WF_vars(Poll) /\ WF_vars(Read) /\ WF_vars(Release) /\ WF_vars(Wait)

TypeOK ==
  /\ child \in {"spawned", "gated", "done"}
  /\ out \in {0, 4, 8}
  /\ ticks \in 0..K
  /\ released \in BOOLEAN
  /\ parent \in {"polling", "listed", "read", "released", "waited", "failed"}
  /\ polls \in Nat
  /\ readBytes \in {0, 4, 8}
  /\ killed \in BOOLEAN

ReadAgrees == parent \in {"read", "released", "waited"} => readBytes = 4
NoWindowFailure == parent # "failed"
Completes == <>(parent = "waited")
\* `wait_process` reports the child completed, not failed (line 583).
CompletedOk == parent = "waited" => ~killed
====
