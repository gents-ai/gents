---- MODULE MemoryAliasMerge ----
\* lean: none
\* crates: gents-tools
\* models: Tools.Memory.Experience.resolve, Tools.Memory.Experience.mergeAdmitted, TransactionFence.conflicts, TransactionReplay.disposition
\* Concurrent requests of one agent writing its experience memory: problems
\* under a unique `problem_key`, append-only attempts citing a key as given
\* (possibly a merged one), and append-only alias rows (`AgentMemoryAlias`,
\* unique `from_problem_key` -> `to_problem_key`) that `memory merge` writes.
\*
\* Each operation is one transaction of the owner the memory tool writes
\* through (`ConfigAccess::transact_write`, `TransactionMode::ConflictRetry`):
\*   open     reads the problem by its unique key and inserts it when absent;
\*   attempt  reads the cited problem by its unique key and, when it exists,
\*            inserts an attempt row citing that key;
\*   merge    reads the problems and every alias row of the agent, asks Lean
\*            `mergeAdmitted` (refused for one key, an unknown key, or a target
\*            that already resolves to the merged key) and inserts the alias
\*            row; with `Guard` it also writes the agent's one alias-guard
\*            document. The unique index refuses a key already merged.
\* `memory carry`, run concurrently by the agent, copies another agent's alias
\* rows (`Carried`, translated to the agent's own problem keys), one row per
\* transaction: with `CarryAdmits` it asks `mergeAdmitted` over the problems
\* and alias rows it reads inside that transaction and inserts the row only
\* when admitted, reporting a refusal instead; with `CarryGuard` an insert
\* also writes the alias-guard document. Without `CarryAdmits` it inserts any
\* row whose keys differ and whose merged key has no alias row yet, the checks
\* the copy made before admission.
\* Readers resolve a cited key with Lean `resolve`: follow the alias rows with
\* fuel (rows + 1); a walk that comes back to a key lands on the minimum key
\* of the cycle it closed (`MinBreak`; FALSE stops at the revisited key, the
\* `ClientShell.SessionTurn.resolveSeen` rule).
\*
\* Premise (`TransactionFence`, evidenced by gents-store's
\* `isolation_premise_tests`): a commit aborts when a transaction that
\* committed after it began wrote a document it also writes or inserted a
\* unique value it also inserts. A filtered read is not validated against a
\* concurrent insert, and a unique-key read that found nothing records
\* nothing, so a merge's read of the alias rows orders it against no other
\* merge: 1 -> 2 and 2 -> 1 insert different `from_problem_key` values and
\* both commit. With `Guard` any two merges of the agent write one document
\* and the later commit aborts (`a_common_write_fences_both_orders`); a guard
\* the first merge creates under a unique key collides the same way. The
\* owner runs an aborted callback again on a fresh transaction
\* (`TransactionReplay`); a refusal writes nothing and is not repeated. An
\* operation aborts only for another actor's commit that writes the guard or
\* inserts the unique value it inserts: the other request's at most `Steps`
\* = 2 operations and the carry's one row, so a request's operation is
\* aborted at most three times, and while the guard keeps the alias rows a
\* forest over the three keys at most two commits write it, so the carry's is
\* aborted at most twice; neither exhausts the owner's four attempts. A
\* concurrent identical open aborts on the unique index and its rebuild finds
\* the winner's row.
\*
\* REDs, one safeguard removed each: `NoGuard` merges without the guard
\* document; `AdmitOutside` decides admission on aliases read before its
\* transaction, so the rebuild after the guard's conflict commits the stale
\* decision; `Rewrite` merges by moving the attempts its filtered read
\* returned to the target and deleting the merged problem instead of
\* appending an alias; `CarryUnadmitted` copies alias rows without admission
\* or guard; `CarryNoGuard` admits each copied row inside its transaction but
\* writes no guard. The minimum-key break matters only once the alias rows
\* hold a cycle, which the guard prevents: `NoGuardResolves` checks it over
\* the cycles `NoGuard` reaches, and `RevisitBreak` removes it there.
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Requests,    \* concurrent requests of the agent
  Keys,        \* problem keys; their order stands for the order of the key strings
  Seeded,      \* problems opened before the requests start
  Steps,       \* memory operations each request runs
  Guard,       \* every merge also writes the agent's alias-guard document
  AdmitInTxn,  \* a merge, and its rebuild, decide on aliases read inside the transaction
  MinBreak,    \* resolve lands a cycle on its minimum key
  AppendOnly,  \* merge appends an alias row and changes no attempt or problem
  Carried,     \* alias rows the carry copies, in order, as <<from, to>>
  CarryAdmits, \* the carry admits each row with `mergeAdmitted` inside its transaction
  CarryGuard   \* a carried insert also writes the alias-guard document

ASSUME /\ Keys \subseteq Nat \ {0}
       /\ Seeded \subseteq Keys
       /\ Steps \in Nat \ {0}
       /\ Guard \in BOOLEAN
       /\ AdmitInTxn \in BOOLEAN
       /\ MinBreak \in BOOLEAN
       /\ AppendOnly \in BOOLEAN
       /\ \A i \in 1..Len(Carried) : Carried[i] \in Keys \X Keys
       /\ CarryAdmits \in BOOLEAN
       /\ CarryGuard \in BOOLEAN

None == 0

\* Attempt rows: the request and the operation that writes it.
Ids == Requests \X (1..Steps)

Ops == [kind : {"open", "attempt"}, key : Keys, to : {None}]
         \cup [kind : {"merge"}, key : Keys, to : Keys]

NoOp == [kind |-> "none", key |-> None, to |-> None]

MaxCommits == Cardinality(Requests) * Steps + Len(Carried)

VARIABLES
  problems,  \* keys with a problem row
  alias,     \* committed alias rows: alias[k] is the key k was merged into, or None
  cites,     \* per attempt row: the key it cites, or None while unwritten
  written,   \* ghost: attempt rows ever written
  guard,     \* commits of the alias-guard document
  pc,        \* per request: "idle", or "open" while its transaction is open
  op,        \* per request: its current operation
  left,      \* per request: operations still to run
  began,     \* per request: the guard commits its transaction began after
  writes,    \* per request: the transaction writes what its callback staged
  moved,     \* per request, `AppendOnly` FALSE: the attempt rows its merge read
  cpos,      \* carry: the position in `Carried` of the row it copies next
  cpc,       \* carry: "idle", or "open" while its transaction is open
  cbegan,    \* carry: the guard commits its transaction began after
  cwrites    \* carry: the transaction inserts the row its callback admitted

vars == <<problems, alias, cites, written, guard, pc, op, left, began, writes, moved,
          cpos, cpc, cbegan, cwrites>>

requestVars == <<pc, op, left, began, writes, moved>>

carryVars == <<cpos, cpc, cbegan, cwrites>>

Min(S) == CHOOSE m \in S : \A n \in S : m <= n

\* The keys of the cycle a walk closed by coming back to k: k and every key
\* it visited after k.
Cycle(seen, k) ==
  LET at == CHOOSE i \in 1..Len(seen) : seen[i] = k
  IN {seen[i] : i \in at..Len(seen)}

\* Lean `Tools.Memory.Experience.resolve` from k, with `fuel` steps left after
\* visiting `seen` in order.
RECURSIVE Walk(_, _, _, _)
Walk(al, fuel, seen, k) ==
  IF fuel = 0 THEN k
  ELSE IF \E i \in 1..Len(seen) : seen[i] = k
       THEN (IF MinBreak THEN Min(Cycle(seen, k)) ELSE k)
       ELSE IF al[k] = None THEN k
            ELSE Walk(al, fuel - 1, Append(seen, k), al[k])

Resolve(al, k) == Walk(al, Cardinality({j \in Keys : al[j] # None}) + 1, <<>>, k)

\* Lean `Tools.Memory.Experience.mergeAdmitted` over what the merge read.
MergeAdmitted(probs, al, from, to) ==
  /\ from \in probs
  /\ to \in probs
  /\ from # to
  /\ al[from] = None
  /\ Resolve(al, to) # from

\* Whether an operation's callback, over the committed rows, stages a write.
Stages(o) ==
  CASE o.kind = "open" -> o.key \notin problems
    [] o.kind = "attempt" -> o.key \in problems
    [] o.kind = "merge" -> MergeAdmitted(problems, alias, o.key, o.to)

\* The attempt rows a rewriting merge's filtered read returns.
Moved(o) ==
  IF o.kind = "merge" /\ ~AppendOnly THEN {a \in Ids : cites[a] = o.key} ELSE {}

\* `TransactionFence.conflicts`: the later of two inserts of one unique value,
\* or of two writes of the guard document, aborts. An attempt writes only its
\* own fresh row, and no row a transaction found by key is written after its
\* insert (the `Rewrite` delete is not validated against such a read here).
Aborts(r) ==
  LET o == op[r] IN
  CASE o.kind = "open" -> o.key \in problems
    [] o.kind = "merge" -> \/ Guard /\ guard # began[r]
                           \/ alias[o.key] # None
    [] OTHER -> FALSE

TypeOK ==
  /\ problems \subseteq Keys
  /\ alias \in [Keys -> Keys \cup {None}]
  /\ cites \in [Ids -> Keys \cup {None}]
  /\ written \subseteq Ids
  /\ guard \in 0..MaxCommits
  /\ pc \in [Requests -> {"idle", "open"}]
  /\ op \in [Requests -> Ops \cup {NoOp}]
  /\ left \in [Requests -> 0..Steps]
  /\ began \in [Requests -> 0..MaxCommits]
  /\ writes \in [Requests -> BOOLEAN]
  /\ moved \in [Requests -> SUBSET Ids]
  /\ cpos \in 1..(Len(Carried) + 1)
  /\ cpc \in {"idle", "open"}
  /\ cbegan \in 0..MaxCommits
  /\ cwrites \in BOOLEAN

Init ==
  /\ problems = Seeded
  /\ alias = [k \in Keys |-> None]
  /\ cites = [a \in Ids |-> None]
  /\ written = {}
  /\ guard = 0
  /\ pc = [r \in Requests |-> "idle"]
  /\ op = [r \in Requests |-> NoOp]
  /\ left = [r \in Requests |-> Steps]
  /\ began = [r \in Requests |-> 0]
  /\ writes = [r \in Requests |-> FALSE]
  /\ moved = [r \in Requests |-> {}]
  /\ cpos = 1
  /\ cpc = "idle"
  /\ cbegan = 0
  /\ cwrites = FALSE

\* A request starts its next operation: the transaction begins and its
\* callback reads the committed rows and stages its writes.
Begin(r, o) ==
  /\ pc[r] = "idle"
  /\ left[r] > 0
  /\ pc' = [pc EXCEPT ![r] = "open"]
  /\ op' = [op EXCEPT ![r] = o]
  /\ left' = [left EXCEPT ![r] = @ - 1]
  /\ began' = [began EXCEPT ![r] = guard]
  /\ writes' = [writes EXCEPT ![r] = Stages(o)]
  /\ moved' = [moved EXCEPT ![r] = Moved(o)]
  /\ UNCHANGED <<problems, alias, cites, written, guard>>
  /\ UNCHANGED carryVars

\* The staged writes land in one commit.
Apply(r) ==
  LET o == op[r]
      id == <<r, Steps - left[r]>>
  IN CASE o.kind = "open" ->
            /\ problems' = problems \cup {o.key}
            /\ UNCHANGED <<alias, cites, written, guard>>
       [] o.kind = "attempt" ->
            /\ cites' = [cites EXCEPT ![id] = o.key]
            /\ written' = written \cup {id}
            /\ UNCHANGED <<problems, alias, guard>>
       [] o.kind = "merge" ->
            /\ guard' = IF Guard THEN guard + 1 ELSE guard
            /\ IF AppendOnly
                 THEN /\ alias' = [alias EXCEPT ![o.key] = o.to]
                      /\ UNCHANGED <<problems, cites, written>>
                 ELSE /\ cites' = [a \in Ids |-> IF a \in moved[r] THEN o.to ELSE cites[a]]
                      /\ problems' = problems \ {o.key}
                      /\ UNCHANGED <<alias, written>>

\* `TransactionReplay`: the conflict runs the callback again on a fresh
\* transaction. With `AdmitInTxn` FALSE a merge keeps the admission it decided
\* before its transaction; only the unique index is checked again by the insert.
Rebuild(r) ==
  LET o == op[r] IN
  /\ began' = [began EXCEPT ![r] = guard]
  /\ writes' = [writes EXCEPT ![r] =
       IF o.kind = "merge" /\ ~AdmitInTxn THEN @ /\ alias[o.key] = None ELSE Stages(o)]
  /\ moved' = [moved EXCEPT ![r] = Moved(o)]
  /\ UNCHANGED <<problems, alias, cites, written, guard, pc, op, left>>

\* The transaction commits what its callback staged, or aborts and is rebuilt.
Commit(r) ==
  /\ pc[r] = "open"
  /\ IF writes[r] /\ Aborts(r)
       THEN Rebuild(r)
       ELSE /\ pc' = [pc EXCEPT ![r] = "idle"]
            /\ IF writes[r] THEN Apply(r) ELSE UNCHANGED <<problems, alias, cites, written, guard>>
            /\ UNCHANGED <<op, left, began, writes, moved>>
  /\ UNCHANGED carryVars

\* The alias row the carry copies next.
Carry == Carried[cpos]

\* Whether the carry's callback, over the committed rows, inserts its row.
CarryStages ==
  IF CarryAdmits THEN MergeAdmitted(problems, alias, Carry[1], Carry[2])
  ELSE Carry[1] # Carry[2] /\ alias[Carry[1]] = None

\* The carry starts copying its next row: the transaction begins and its
\* callback reads the committed rows.
CarryBegin ==
  /\ cpc = "idle"
  /\ cpos <= Len(Carried)
  /\ cpc' = "open"
  /\ cbegan' = guard
  /\ cwrites' = CarryStages
  /\ UNCHANGED <<problems, alias, cites, written, guard, cpos>>
  /\ UNCHANGED requestVars

\* The carry commits its insert, or aborts on the guard or the unique index
\* and is rebuilt on a fresh transaction that decides again; a refusal
\* commits nothing and the carry reports it.
CarryCommit ==
  /\ cpc = "open"
  /\ IF cwrites /\ ((CarryGuard /\ guard # cbegan) \/ alias[Carry[1]] # None)
       THEN /\ cbegan' = guard
            /\ cwrites' = CarryStages
            /\ UNCHANGED <<problems, alias, cites, written, guard, cpos, cpc>>
       ELSE /\ cpc' = "idle"
            /\ cpos' = cpos + 1
            /\ alias' = IF cwrites THEN [alias EXCEPT ![Carry[1]] = Carry[2]] ELSE alias
            /\ guard' = IF cwrites /\ CarryGuard THEN guard + 1 ELSE guard
            /\ UNCHANGED <<problems, cites, written, cbegan, cwrites>>
  /\ UNCHANGED requestVars

Next == \/ \E r \in Requests : (\E o \in Ops : Begin(r, o)) \/ Commit(r)
        \/ CarryBegin
        \/ CarryCommit

Spec == Init /\ [][Next]_vars

\* The keys k reaches through at most n alias rows.
RECURSIVE Reach(_, _, _)
Reach(al, k, n) ==
  IF n = 0 \/ al[k] = None THEN {} ELSE {al[k]} \cup Reach(al, al[k], n - 1)

\* Every attempt row ever written is still stored and resolves to a problem
\* that exists.
NoAttemptLost ==
  \A a \in written : cites[a] # None /\ Resolve(alias, cites[a]) \in problems

\* Every reader resolves a key to one problem, whether its walk starts at
\* that key or reaches it through an alias row: the keys of one merged
\* problem resolve alike.
ResolveDeterministic ==
  \A k \in Keys : alias[k] # None => Resolve(alias, k) = Resolve(alias, alias[k])

\* No key reaches itself through the committed alias rows.
AliasAcyclic == \A k \in Keys : k \notin Reach(alias, k, Cardinality(Keys))
====
