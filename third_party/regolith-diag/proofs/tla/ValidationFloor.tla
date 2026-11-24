---- MODULE ValidationFloor ----
\* A validator captures one read view while writes and flushes race.
\* Old table versions cannot conflict with the transaction's observation.
\* Correspondence: Capture is ReadViewCell::load; Flush publishes the
\* memtable and version together; Check is latest_version_after's ceiling
\* guard. PipelineCheck.old_tables_do_not_change_validation proves the
\* same exclusion for every number of keys and versions.
EXTENDS Naturals, FiniteSets
\* AlwaysSkip plants the bug of ignoring tables with newer versions too.
CONSTANT Mutant
\* Live sources, the sequence, and the captured read view.
VARIABLES mem, tables, seq, ceiling, viewMem, viewTables, viewCeiling, pc, result
\* The complete state.
vars == <<mem, tables, seq, ceiling, viewMem, viewTables, viewCeiling, pc, result>>
\* The transaction observed version one before its validation began.
Floor == 1
\* No version above the observation is allowed to disappear from validation.
Newer(s) == {v \in s : v > Floor} # {}
\* One old version starts in a table, and nothing is captured yet.
Init ==
  \* The live table contains only the observed version.
  /\ mem = {} /\ tables = {1} /\ seq = 1 /\ ceiling = 1
  \* Nothing has been captured yet.
  /\ viewMem = {} /\ viewTables = {} /\ viewCeiling = 0
  \* Validation starts before its view load.
  /\ pc = "capture" /\ result = FALSE
\* A concurrent write creates version two in the active memtable.
Write ==
  \* Only one newer write is needed for the counterexample.
  /\ seq = 1 /\ seq' = 2 /\ mem' = mem \cup {2}
  \* A write changes none of the table or captured state.
  /\ UNCHANGED <<tables, ceiling, viewMem, viewTables, viewCeiling, pc, result>>
\* A flush publishes its table and its ceiling in the same read view.
Flush ==
  \* Move the active versions into the published table set.
  /\ mem # {} /\ tables' = tables \cup mem /\ mem' = {}
  \* The new ceiling bounds every flushed version.
  /\ ceiling' = seq
  \* Flush does not alter an already captured view.
  /\ UNCHANGED <<seq, viewMem, viewTables, viewCeiling, pc, result>>
\* An ingest installs a newer table and its ceiling in the same view.
Ingest ==
  \* The external table arrives at a newly assigned sequence.
  /\ seq = 1 /\ seq' = 2 /\ tables' = tables \cup {2}
  \* The mutant publishes the table without covering its newer sequence.
  /\ ceiling' = IF Mutant = "StaleIngestCeiling" THEN ceiling ELSE 2
  \* An ingest does not rewrite an already captured read view.
  /\ UNCHANGED <<mem, viewMem, viewTables, viewCeiling, pc, result>>
\* One loaded view owns both the sources and the ceiling tested below.
Capture ==
  \* Capture happens exactly once before checking.
  /\ pc = "capture" /\ pc' = "check"
  \* All three captured facts belong to this same read view.
  /\ viewMem' = mem /\ viewTables' = tables /\ viewCeiling' = ceiling
  \* Capturing does not write storage.
  /\ UNCHANGED <<mem, tables, seq, ceiling, result>>
\* Skip tables only when this view proves every table version old enough.
Check ==
  \* Checking completes this validation.
  /\ pc = "check" /\ pc' = "done"
  \* Include memtables and, unless proven old, the tables.
  /\ result' = (Newer(viewMem) \/
       IF Mutant = "AlwaysSkip" \/ viewCeiling <= Floor
       THEN FALSE ELSE Newer(viewTables))
  \* The check changes only its result and phase.
  /\ UNCHANGED <<mem, tables, seq, ceiling, viewMem, viewTables, viewCeiling>>
\* The interleavings before and after the view is loaded.
Next == Write \/ Flush \/ Ingest \/ Capture \/ Check
\* Every table version is covered by its own view's ceiling.
Bounded == \A v \in viewTables : v <= viewCeiling
\* Validation agrees with looking at every source in the captured view.
Exact == pc = "done" => result = Newer(viewMem \cup viewTables)
\* Stuttering permits a completed validation to stop taking steps.
Spec == Init /\ [][Next]_vars
====
