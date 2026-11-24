---- MODULE BatchMergeFold ----
\* Two atomic commits append operands. A snapshot between them must keep
\* its value. Folding [+1,+2] into one operand may save work, but reversing
\* their order, dropping a declined pair, or rewriting the previous commit
\* must fail. Values are sequences so order, not only sums, is observable.
\* Correspondence: Transaction::drain_for_commit settles one transaction;
\* write_buffer::fold_merges folds only that transaction's same-key operands;
\* commit publication and snapshot visibility remain CommitPipeline.tla.
\* Each operand also names its touched parts, as MergeOperator requires.
\* Correspondence (source lines at this change):
\* original: the unfolded reference stream, checked by transaction_merge_folding.rs.
\* stored, Commit: transaction.rs:2105, Transaction::drain_for_commit owns the drained writes.
\* Fold: fold_merges groups only repeated keys; fold_run replaces adjacent balanced subtrees.
\* A unique-key buffer skips folding, the identity step. merge_call.rs dispatches the callback.
\* INV_Reads: the callback's exactness law, proved for every surrounding list in BatchMergeFold.lean.
\* INV_Parts: options.rs:465, MergeOperator::touches; this is a caller contract, not a new check.
\* INV_Size: fold_merges checks both original operands and the combined encoding against the limit.
\* reads, Snapshot: existing CommitPipeline and SnapshotRegistry models; this change edits neither.
\* Cross-key, iterator and storage-source ordering remain the existing read/commit model boundaries.
\* Use numbers, ordered lists and finite sets to describe the tiny history.
EXTENDS Naturals, Sequences, FiniteSets
\* Bug selects a deliberate mistake; Limit is the configured operand size bound.
CONSTANT Bug, Limit
\* Keep reference commits, stored commits and pinned snapshot boundaries.
VARIABLES original, stored, reads
\* List all state that may change in one model step.
vars == <<original, stored, reads>>
\* One caller operand contributes its number, touched part and encoded size.
Op(n) == [value |-> <<n>>, parts |-> {n}, bytes |-> 2]
\* Each atomic commit writes two ordered operands of the same key.
Batch == <<Op(1), Op(2)>>
\* Combining operands preserves order and touched parts unless that contract is deliberately broken.
Join(a, b) == [value |-> a.value \o b.value,
              parts |-> IF Bug = "Parts" THEN a.parts ELSE a.parts \cup b.parts,
              bytes |-> 3] \* A combined encoding saves one byte, but might exceed Limit.
\* Define the value of a list by reading its first operand and then its tail.
RECURSIVE Value(_)
\* Apply operands oldest first, with an empty list contributing nothing.
Value(ops) == IF Len(ops) = 0 THEN <<>> ELSE Head(ops).value \o Value(Tail(ops))
\* Define a list's touched parts by collecting the first operand and its tail.
RECURSIVE Parts(_)
\* A part is touched when any operand in the list touches it.
Parts(ops) == IF Len(ops) = 0 THEN {} ELSE Head(ops).parts \cup Parts(Tail(ops))
\* Define the visible operand stream by joining its commits in order.
RECURSIVE Flatten(_)
\* Empty history contributes nothing; otherwise append each commit's operands.
Flatten(commits) == IF Len(commits) = 0 THEN <<>> ELSE Head(commits) \o Flatten(Tail(commits))
\* Only a successful callback with a size-safe result permits folding.
Fold(accepted) ==
    \* A declined callback keeps its original operands.
    IF ~accepted THEN
        \* The declined-callback mutant incorrectly discards the second write.
        IF Bug = "Declined" THEN <<Batch[1]>> ELSE Batch
    \* Leave valid inputs apart when their combined encoding exceeds the limit.
    ELSE IF Join(Batch[1], Batch[2]).bytes > Limit /\ Bug # "Size" THEN Batch
    \* The order mutant incorrectly applies the newer operand first.
    ELSE IF Bug = "Order" THEN <<Join(Batch[2], Batch[1])>>
    \* Otherwise keep one exact combined operand.
    ELSE <<Join(Batch[1], Batch[2])>>
\* Start without commits or pinned snapshots.
Init == /\ original = <<>> /\ stored = <<>> /\ reads = {}
\* Publish one whole commit, with no snapshot able to observe its intermediate writes.
Commit(accepted) ==
    \* Two commits suffice to expose an older snapshot boundary.
    /\ Len(original) < 2
    \* The reference retains every original operand.
    /\ original' = Append(original, Batch)
    \* The boundary mutant moves new writes into the previous commit.
    /\ stored' = IF Bug = "Boundary" /\ Len(stored) = 1
                 \* Rewriting the older commit changes what an older snapshot sees.
                 THEN <<stored[1] \o Fold(accepted), <<>>>>
                 \* The correct path changes only the new commit.
                 ELSE Append(stored, Fold(accepted))
    \* Existing snapshots stay pinned at their original boundaries.
    /\ UNCHANGED reads
\* Register a snapshot at the current published boundary.
Snapshot ==
    \* Remember this commit count as a boundary that future commits must preserve.
    /\ reads' = reads \cup {Len(original)}
    \* Registration changes no stored or reference data.
    /\ UNCHANGED <<original, stored>>
\* Either publish a commit whose callback accepts or declines, or pin a snapshot.
Next == (\E accepted \in BOOLEAN : Commit(accepted)) \/ Snapshot
\* Latest reads and every pinned snapshot must agree with the unfurled history.
INV_Reads ==
    \* Check the latest boundary as well as all pinned boundaries.
    \A n \in reads \cup {Len(original)} :
        \* Compare the visible stored operand stream with its original stream.
        Value(Flatten(SubSeq(stored, 1, n))) =
        Value(Flatten(SubSeq(original, 1, n))) \* Latest and every pinned read agree.
\* Combining operands must preserve every touched part of that commit.
INV_Parts ==
    \* Compare each stored commit's touched parts with the original writes.
    \A n \in 1..Len(original) : Parts(stored[n]) = Parts(original[n])
\* Folding valid inputs must not introduce an oversized stored operand.
INV_Size ==
    \* Inspect every operand of every stored commit.
    \A n \in 1..Len(stored) : \A i \in 1..Len(stored[n]) :
        \* Its encoded size must stay within the configured bound.
        stored[n][i].bytes <= Limit
\* Explore atomic publications and snapshot registrations, allowing idle steps.
Spec == Init /\ [][Next]_vars
====
