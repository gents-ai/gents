---- MODULE ScriptFixtureExec ----
(***************************************************************************)
(* A test that executes a script it created, beside sibling tests of the  *)
(* same process that spawn children concurrently                           *)
(* (`supplied_path_retains_native_script_fallback` in                       *)
(* `gents-session` `exec::process::spawn::tests`).                          *)
(*                                                                          *)
(* `F` is the fixture's thread. With `InProcessWrite` it creates the        *)
(* script as the test did: `std::fs::write` opens it for writing           *)
(* (O_CLOEXEC) in the test process, writes and closes it, then             *)
(* `run_managed_exec` spawns it (`spawn_child` in                           *)
(* `exec/process/spawn.rs`). Without it the script is created and closed  *)
(* outside this process (a fixture checked into the repository), so the   *)
(* test process never holds a descriptor that may write it.                *)
(*                                                                          *)
(* Each sibling is another test thread whose spawn duplicates the whole    *)
(* process's descriptor table into its child at the fork (glibc            *)
(* `posix_spawn` clones without CLONE_FILES; a plain fork does the same).   *)
(* O_CLOEXEC closes the copies only when that child execs, so a child      *)
(* forked while `F` holds its write descriptor keeps a writer of the       *)
(* script until its own exec.                                              *)
(*                                                                          *)
(* `ExecRefusesWriters` is the kernel's rule: Linux execve fails with       *)
(* ETXTBSY while any process holds the file open for writing (fs/exec.c,    *)
(* `deny_write_access` on the opened executable); Darwin's execve has no    *)
(* such check. The spawn owner forwards the native error unchanged           *)
(* (`SpawnFailed`), as execvp would; it has no decision of its own here.    *)
(*                                                                          *)
(* `NoTextBusy` is what the test asserts: its own exec succeeds. It holds   *)
(* when the fixture is never write-open in the spawning process (green),    *)
(* on Darwin (`Darwin`), and with no sibling spawning (`Alone`). RED:        *)
(* `InProcessWrite` is the test's order on Linux and the failure seen on     *)
(* the Linux runner.                                                        *)
(***************************************************************************)

CONSTANTS
  Siblings,           \* other test threads of the same process that spawn
  InProcessWrite,     \* the test process itself opens the script for writing
  ExecRefusesWriters  \* execve refuses a file open for writing (Linux)

VARIABLES
  writers,  \* processes holding a descriptor that may write the script
  f,        \* F: start | open | closed | ran | busy
  sib       \* sibling child: idle | forked | execed

vars == <<writers, f, sib>>

Parent == "parent"

TypeOK ==
  /\ writers \subseteq ({Parent} \cup Siblings)
  /\ f \in {"start", "open", "closed", "ran", "busy"}
  /\ sib \in [Siblings -> {"idle", "forked", "execed"}]

Init ==
  /\ writers = {}
  /\ f = "start"
  /\ sib = [s \in Siblings |-> "idle"]

\* `std::fs::write` opens the script for writing in the test process.
Open ==
  /\ f = "start"
  /\ InProcessWrite
  /\ f' = "open"
  /\ writers' = writers \cup {Parent}
  /\ UNCHANGED sib

\* The write completes and the descriptor is closed; without
\* `InProcessWrite` the script already exists, closed, before the test.
Close ==
  /\ \/ f = "open"
     \/ f = "start" /\ ~InProcessWrite
  /\ f' = "closed"
  /\ writers' = writers \ {Parent}
  /\ UNCHANGED sib

\* `spawn_child` execs the script; the kernel checks for writers.
Exec ==
  /\ f = "closed"
  /\ f' = IF ExecRefusesWriters /\ writers # {} THEN "busy" ELSE "ran"
  /\ UNCHANGED <<writers, sib>>

\* A sibling's spawn forks: its child copies every open descriptor.
Fork(s) ==
  /\ sib[s] = "idle"
  /\ sib' = [sib EXCEPT ![s] = "forked"]
  /\ writers' = IF Parent \in writers THEN writers \cup {s} ELSE writers
  /\ UNCHANGED f

\* The sibling's child execs its own program; O_CLOEXEC closes the copies.
ChildExec(s) ==
  /\ sib[s] = "forked"
  /\ sib' = [sib EXCEPT ![s] = "execed"]
  /\ writers' = writers \ {s}
  /\ UNCHANGED f

Next ==
  \/ Open \/ Close \/ Exec
  \/ \E s \in Siblings : Fork(s) \/ ChildExec(s)

Spec == Init /\ [][Next]_vars

NoTextBusy == f # "busy"

====
