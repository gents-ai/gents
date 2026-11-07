---- MODULE BackgroundAdmissionContention ----
\* Concurrent writers of one request's background budget (#3592).
\*
\* Each accepted `spawn_process` call runs two DefraDB transactions:
\*   1. admission (`tool_call.admit_spawned_background`): reads the request's
\*      background `AgentToolCall` rows to count live ones, bumps
\*      `AgentRequest.background_admission_sequence`, then creates the child;
\*      a full budget refuses before writing;
\*   2. receipt publication (`tool_call.complete_delivery`, or
\*      `tool_call.fail_raw_with_presentation` for a refusal): rereads the
\*      request's tool rows, takes the next `AgentMessage` session sequence and
\*      refreshes the `AgentSession` request observation.
\* A running child's start and completion write its own tool row and the
\* session observation.
\*
\* Keys: "seq" is the `AgentRequest` document; "sess" abstracts the
\* session-scoped documents every call reads and writes (the request's
\* `AgentToolCall` rows, the `AgentMessage` sequence index entry and the
\* `AgentSession` document). Regolith conflicts are per document blob: a read
\* of any part of a document conflicts with a later merge into it.
\*
\* DefraDB transactions are optimistic: a commit aborts when a key it read or
\* wrote changed after its snapshot. A predicate read does not see a sibling's
\* newly created row, which is why admission bumps the sequence. Every attempt
\* budget is bounded (`Attempts`); exhausting it fails the call ("tool call
\* persistence failed: DefraDB transaction conflict") with no owner to
\* re-drive it.
\*
\* `SequentialAdmission` selects the owned loop's order: it dispatches a
\* non-read call such as `spawn_process` alone, and a read batch
\* (`run_batch`) awaits each call's `on_tool_call` (admission and receipt)
\* before the next, so sibling admissions of one turn never overlap. FALSE lets every
\* sibling run both transactions at once, the schedule the regression drove.
EXTENDS Naturals, FiniteSets

CONSTANTS
  N,                    \* calls in one assistant turn, admitted in order 1..N
  Max,                  \* MAX_BACKGROUNDED_TOOLS_PER_PARENT
  Attempts,             \* attempts a contended transaction may make
  SequentialAdmission,  \* the owned loop's per-call admission order
  BumpSequence          \* admission writes the request admission sequence

ASSUME /\ N \in Nat \ {0}
       /\ Max \in Nat \ {0}
       /\ Attempts \in Nat \ {0}
       /\ SequentialAdmission \in BOOLEAN
       /\ BumpSequence \in BOOLEAN

Calls == 1..N
Keys == {"seq", "sess"}

\* Read-and-write sets. Admission reads the session's tool rows; when it bumps
\* the sequence it also reads and writes `seq`.
AdmitKeys == IF BumpSequence THEN {"seq", "sess"} ELSE {"sess"}
PublishKeys == {"sess"}

VARIABLES
  ver,      \* committed version of each key
  phase,    \* "admit" | "publish" | "settled" | "lost"
  open,     \* an attempt of the call's current transaction is in flight
  snap,     \* key versions the open attempt observed
  liveSnap, \* live background count the open admission attempt observed
  tries,    \* attempts spent on the current transaction
  refused,  \* the admission refused on the budget
  child,    \* "none" | "running" | "done"
  receipts  \* receipts published for the call

vars == <<ver, phase, open, snap, liveSnap, tries, refused, child, receipts>>

Live == Cardinality({c \in Calls : child[c] = "running"})

KeysOf(c) == IF phase[c] = "admit" THEN AdmitKeys ELSE PublishKeys

TypeOK ==
  /\ ver \in [Keys -> Nat]
  /\ phase \in [Calls -> {"admit", "publish", "settled", "lost"}]
  /\ open \in [Calls -> BOOLEAN]
  /\ snap \in [Calls -> [Keys -> Nat]]
  /\ liveSnap \in [Calls -> Nat]
  /\ tries \in [Calls -> Nat]
  /\ refused \in [Calls -> BOOLEAN]
  /\ child \in [Calls -> {"none", "running", "done"}]
  /\ receipts \in [Calls -> Nat]

Init ==
  /\ ver = [k \in Keys |-> 0]
  /\ phase = [c \in Calls |-> "admit"]
  /\ open = [c \in Calls |-> FALSE]
  /\ snap = [c \in Calls |-> [k \in Keys |-> 0]]
  /\ liveSnap = [c \in Calls |-> 0]
  /\ tries = [c \in Calls |-> 0]
  /\ refused = [c \in Calls |-> FALSE]
  /\ child = [c \in Calls |-> "none"]
  /\ receipts = [c \in Calls |-> 0]

\* The owned loop admits call c only after every earlier call's
\* `on_tool_call` returned its receipt.
MayStart(c) ==
  \/ ~SequentialAdmission
  \/ \A d \in Calls : d < c => phase[d] \in {"settled", "lost"}

Begin(c) ==
  /\ phase[c] \in {"admit", "publish"}
  /\ ~open[c]
  /\ MayStart(c)
  /\ open' = [open EXCEPT ![c] = TRUE]
  /\ snap' = [snap EXCEPT ![c] = ver]
  /\ liveSnap' = [liveSnap EXCEPT ![c] = Live]
  /\ tries' = [tries EXCEPT ![c] = @ + 1]
  /\ UNCHANGED <<ver, phase, refused, child, receipts>>

Conflicts(c) == \E k \in KeysOf(c) : ver[k] # snap[c][k]

\* A conflicting commit aborts. The attempt loop retries only while its
\* budget lasts; the last abort fails the call with no owner to re-drive it.
Abort(c) ==
  /\ open[c]
  /\ Conflicts(c)
  /\ open' = [open EXCEPT ![c] = FALSE]
  /\ phase' = IF tries[c] >= Attempts
                THEN [phase EXCEPT ![c] = "lost"]
                ELSE phase
  /\ UNCHANGED <<ver, snap, liveSnap, tries, refused, child, receipts>>

Bump(keys) == [k \in Keys |-> IF k \in keys THEN ver[k] + 1 ELSE ver[k]]

\* A refusal returns before any write, so it commits nothing and moves to
\* the refusal receipt.
CommitAdmission(c) ==
  /\ open[c]
  /\ phase[c] = "admit"
  /\ ~Conflicts(c)
  /\ open' = [open EXCEPT ![c] = FALSE]
  /\ phase' = [phase EXCEPT ![c] = "publish"]
  /\ tries' = [tries EXCEPT ![c] = 0]
  /\ IF liveSnap[c] < Max
       THEN /\ child' = [child EXCEPT ![c] = "running"]
            /\ ver' = Bump(AdmitKeys \cap {"seq"})
            /\ UNCHANGED refused
       ELSE /\ refused' = [refused EXCEPT ![c] = TRUE]
            /\ UNCHANGED <<child, ver>>
  /\ UNCHANGED <<snap, liveSnap, receipts>>

CommitReceipt(c) ==
  /\ open[c]
  /\ phase[c] = "publish"
  /\ ~Conflicts(c)
  /\ open' = [open EXCEPT ![c] = FALSE]
  /\ phase' = [phase EXCEPT ![c] = "settled"]
  /\ tries' = [tries EXCEPT ![c] = 0]
  /\ ver' = Bump(PublishKeys)
  /\ receipts' = [receipts EXCEPT ![c] = @ + 1]
  /\ UNCHANGED <<snap, liveSnap, refused, child>>

\* A running child finishes and writes its row and the session observation.
\* Background completion recovery re-drives its transaction, so it is modeled
\* as one commit.
ChildCompletes(c) ==
  /\ child[c] = "running"
  /\ child' = [child EXCEPT ![c] = "done"]
  /\ ver' = Bump({"sess"})
  /\ UNCHANGED <<phase, open, snap, liveSnap, tries, refused, receipts>>

Next ==
  \E c \in Calls :
    \/ Begin(c)
    \/ Abort(c)
    \/ CommitAdmission(c)
    \/ CommitReceipt(c)
    \/ ChildCompletes(c)

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ \A c \in Calls :
       /\ WF_vars(Begin(c))
       /\ WF_vars(Abort(c))
       /\ WF_vars(CommitAdmission(c))
       /\ WF_vars(CommitReceipt(c))

\* The budget never admits more than Max live children.
BudgetBound == Live <= Max

\* No accepted call loses its admission or receipt to a storage conflict.
NoConflictLoss == \A c \in Calls : phase[c] # "lost"

\* Each accepted call publishes at most one receipt, and only once settled.
ReceiptOnce ==
  \A c \in Calls :
    /\ receipts[c] <= 1
    /\ (receipts[c] = 1) <=> (phase[c] = "settled")

\* Every accepted call eventually publishes its one receipt.
EveryCallSettles == <>(\A c \in Calls : phase[c] = "settled")

====
