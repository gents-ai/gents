---- MODULE FireBudgetAdmission ----
\* lean: none
\* crates: gents-automation, gents-schemas
\* models: Triggers.FireBudget.commitConcurrent, Triggers.FireBudget.admitOn, TransactionFence.conflicts
\* Concurrent admissions of fires of one Trigger against its rolling budget.
\*
\* Each admission (`lifecycle.admit_trigger_delivery`) is one transaction: it
\* counts the Trigger's receipts in the window (`PreparedFire::stage_budget`),
\* and when the count is below the budget it inserts its receipt
\* (`stage_fire_receipt`). The count is a filtered read the store does not
\* validate at commit, so it does not order two admissions. With
\* `UniqueSequence` each receipt takes `fire_sequence` one past the receipts
\* its admission read, under the unique index `(owner_did, trigger_id,
\* fire_sequence)`: a commit whose sequence another receipt took aborts, and
\* its owner runs the admission again on a fresh read. FALSE is count by scan
\* alone. The window does not move here: every receipt is inside it.
EXTENDS Naturals

CONSTANTS
  Admissions,      \* fires of the Trigger admitted at once
  Budget,          \* max_fires_per_hour
  Seeded,          \* receipts already in the window
  UniqueSequence   \* the receipt's unique fire_sequence

ASSUME /\ Admissions \in Nat \ {0}
       /\ Budget \in Nat \ {0}
       /\ Seeded \in Nat

VARIABLES
  receipts,  \* committed receipts in the window; their sequences are 0..receipts-1
  phase,     \* per admission: "idle", "read", "done"
  seen       \* per admission: the receipt count its transaction read

vars == <<receipts, phase, seen>>
A == 1..Admissions

TypeOK ==
  /\ receipts \in Nat
  /\ phase \in [A -> {"idle", "read", "done"}]
  /\ seen \in [A -> Nat]

Init ==
  /\ receipts = Seeded
  /\ phase = [a \in A |-> "idle"]
  /\ seen = [a \in A |-> 0]

Read(a) ==
  /\ phase[a] = "idle"
  /\ phase' = [phase EXCEPT ![a] = "read"]
  /\ seen' = [seen EXCEPT ![a] = receipts]
  /\ UNCHANGED receipts

\* Below the budget the admission inserts its receipt at sequence seen[a].
Commit(a) ==
  /\ phase[a] = "read"
  /\ IF seen[a] >= Budget
       THEN /\ phase' = [phase EXCEPT ![a] = "done"]
            /\ UNCHANGED <<receipts, seen>>
       ELSE IF UniqueSequence /\ seen[a] # receipts
         THEN /\ phase' = [phase EXCEPT ![a] = "idle"]
              /\ UNCHANGED <<receipts, seen>>
         ELSE /\ receipts' = receipts + 1
              /\ phase' = [phase EXCEPT ![a] = "done"]
              /\ UNCHANGED seen

Next == \E a \in A : Read(a) \/ Commit(a)

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

BudgetBound == receipts <= (IF Seeded > Budget THEN Seeded ELSE Budget)

AllSettle == <>(\A a \in A : phase[a] = "done")
====
