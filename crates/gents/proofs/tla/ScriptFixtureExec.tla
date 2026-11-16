---- MODULE ScriptFixtureExec ----
(***************************************************************************)
(* A test that executes a script it created, beside sibling tests of the  *)
(* same process that spawn children concurrently                           *)
(* (`supplied_path_retains_native_script_fallback` in                       *)
(* `gents-session` `exec::process::spawn::tests`, and every test that      *)
(* writes an executable and then runs it).                                  *)
(*                                                                          *)
(* `F` is the fixture's thread. `Writer` is who opens the script for       *)
(* writing:                                                                 *)
(*   "repo"  - nobody: the script is checked into the repository and       *)
(*             exists, closed, before the test starts.                      *)
(*   "test"  - the test process: `std::fs::write` opens it for writing     *)
(*             with O_CLOEXEC, writes and closes it, then `spawn_child`    *)
(*             (`exec/process/spawn.rs`) execs it. This is the old order   *)
(*             and also the O_CLOEXEC-only proposal: Rust's std already    *)
(*             opens every file with O_CLOEXEC, which closes a copy only   *)
(*             when the child that holds it execs, not when it forks.       *)
(*   "child" - a short-lived writer process `W`                             *)
(*             (`gents_store::test_support::write_executable_script`): the *)
(*             test spawns `/bin/sh`, which opens the script, writes it    *)
(*             with the `printf` builtin, closes it and exits; the test    *)
(*             then sets the mode with chmod, which opens nothing. `W`     *)
(*             forks nothing while it holds the descriptor, so its exit    *)
(*             releases the only writer. With `WaitForWriter` the test     *)
(*             execs only after reaping `W` (`Command::status`).            *)
(*                                                                          *)
(* Each sibling is another test thread whose spawn duplicates the whole    *)
(* test process's descriptor table into its child at the fork (glibc       *)
(* `posix_spawn` clones without CLONE_FILES; a plain fork does the same).   *)
(* O_CLOEXEC closes the copies only when that child execs, so a child      *)
(* forked while the test process holds a write descriptor keeps a writer   *)
(* of the script until its own exec. Siblings fork the test process only:  *)
(* a descriptor held by `W` is never in the table they copy.               *)
(*                                                                          *)
(* `ExecRefusesWriters` is the kernel's rule: Linux execve fails with       *)
(* ETXTBSY while any process holds the file open for writing (fs/exec.c,    *)
(* `deny_write_access` on the opened executable); Darwin's execve has no    *)
(* such check. The spawn owner forwards the native error unchanged           *)
(* (`SpawnFailed`), as execvp would; it has no decision of its own here.    *)
(*                                                                          *)
(* `NoTextBusy` is what the test asserts: its own exec succeeds. It holds   *)
(* for a checked-in fixture (green), for the child writer the test waits   *)
(* for (`ChildWriter`), on Darwin (`Darwin`), and with no sibling          *)
(* spawning (`Alone`). RED: `InProcessWrite` (the old order and the        *)
(* O_CLOEXEC-only proposal on Linux, the failure seen on the Linux runner) *)
(* and `ChildWriterUnwaited` (the child writer without the reap).          *)
(***************************************************************************)

CONSTANTS
  Siblings,           \* other test threads of the same process that spawn
  Writer,             \* "repo" | "test" | "child": who writes the script
  WaitForWriter,      \* the test reaps the writer child before its exec
  ExecRefusesWriters  \* execve refuses a file open for writing (Linux)

VARIABLES
  writers,  \* processes holding a descriptor that may write the script
  f,        \* F: start | open | spawned | closed | ran | busy
  w,        \* W, the writer child: none | forked | writing | exited
  sib       \* sibling child: idle | forked | execed

vars == <<writers, f, w, sib>>

Parent == "parent"
WriterChild == "writer"

TypeOK ==
  /\ Writer \in {"repo", "test", "child"}
  /\ writers \subseteq ({Parent, WriterChild} \cup Siblings)
  /\ f \in {"start", "open", "spawned", "closed", "ran", "busy"}
  /\ w \in {"none", "forked", "writing", "exited"}
  /\ sib \in [Siblings -> {"idle", "forked", "execed"}]

Init ==
  /\ writers = {}
  /\ f = "start"
  /\ w = "none"
  /\ sib = [s \in Siblings |-> "idle"]

\* "repo": the script already exists, closed, before the test.
CheckedIn ==
  /\ f = "start"
  /\ Writer = "repo"
  /\ f' = "closed"
  /\ UNCHANGED <<writers, w, sib>>

\* "test": `std::fs::write` opens the script for writing in the test process.
Open ==
  /\ f = "start"
  /\ Writer = "test"
  /\ f' = "open"
  /\ writers' = writers \cup {Parent}
  /\ UNCHANGED <<w, sib>>

\* "test": the write completes and the test process closes its descriptor.
Close ==
  /\ f = "open"
  /\ f' = "closed"
  /\ writers' = writers \ {Parent}
  /\ UNCHANGED <<w, sib>>

\* "child": the test spawns the writer. Its fork copies the test process's
\* table, which holds no descriptor of the script.
SpawnWriter ==
  /\ f = "start"
  /\ Writer = "child"
  /\ f' = "spawned"
  /\ w' = "forked"
  /\ UNCHANGED <<writers, sib>>

\* `W` has exec'd `/bin/sh`, which opens the script for writing.
WriterOpen ==
  /\ w = "forked"
  /\ w' = "writing"
  /\ writers' = writers \cup {WriterChild}
  /\ UNCHANGED <<f, sib>>

\* `W` closes the script and exits; the kernel releases its descriptors.
WriterExit ==
  /\ w = "writing"
  /\ w' = "exited"
  /\ writers' = writers \ {WriterChild}
  /\ UNCHANGED <<f, sib>>

\* The test proceeds to its exec: after reaping `W` with `WaitForWriter`,
\* at once without it.
WriterDone ==
  /\ f = "spawned"
  /\ WaitForWriter => w = "exited"
  /\ f' = "closed"
  /\ UNCHANGED <<writers, w, sib>>

\* `spawn_child` execs the script; the kernel checks for writers.
Exec ==
  /\ f = "closed"
  /\ f' = IF ExecRefusesWriters /\ writers # {} THEN "busy" ELSE "ran"
  /\ UNCHANGED <<writers, w, sib>>

\* A sibling's spawn forks the test process: its child copies every
\* descriptor the test process holds open.
Fork(s) ==
  /\ sib[s] = "idle"
  /\ sib' = [sib EXCEPT ![s] = "forked"]
  /\ writers' = IF Parent \in writers THEN writers \cup {s} ELSE writers
  /\ UNCHANGED <<f, w>>

\* The sibling's child execs its own program; O_CLOEXEC closes the copies.
ChildExec(s) ==
  /\ sib[s] = "forked"
  /\ sib' = [sib EXCEPT ![s] = "execed"]
  /\ writers' = writers \ {s}
  /\ UNCHANGED <<f, w>>

Next ==
  \/ CheckedIn \/ Open \/ Close \/ Exec
  \/ SpawnWriter \/ WriterOpen \/ WriterExit \/ WriterDone
  \/ \E s \in Siblings : Fork(s) \/ ChildExec(s)

Spec == Init /\ [][Next]_vars

NoTextBusy == f # "busy"

====
