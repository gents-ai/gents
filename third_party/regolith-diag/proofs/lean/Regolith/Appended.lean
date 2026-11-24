/-!
# Appended: a transaction reads its own appends back, oldest first

This file backs `proofs/tla/AppendedEntries.tla`, invariants
`INV_AppendedIsCommitOrder` and `INV_RollbackRestores` (configurations
`MC_AppendedEntries_Green*`), and the RED `MC_AppendedEntries_Red_NewestFirst`.

## The story, for a reader who has never seen the code

`Transaction::append` (src/transaction/append.rs) does not write anything at
once. It puts the entry on a queue, the append buffer, and the commit later
gives the queued entries their positions in the log, in the order they were
queued. A position does not exist before that, so the transaction cannot
read its own appends from the log.

`Transaction::appended` reads the queue instead. The queue is kept newest
first (the buffer's chain walk starts at the newest), so `appended` walks it,
keeps the entries of the one log it was asked about, and reverses the result
to give oldest first.

Tiny example. The queue, newest first, is: `(log 2, "x")`, `(log 1, "b")`,
`(log 1, "a")`. Then `appended 1` is `["a", "b"]` and `appended 2` is `["x"]`.
A savepoint taken when the queue held only `(log 1, "a")` and then a
rollback gives the queue back, so `appended 1` is `["a"]` again.

What could go wrong, and what the theorems here show cannot happen, for any
number of appends and logs:

1. `appended_push_same`: an append to a log adds its entry last.
2. `appended_push_other`: an append to another log leaves the answer alone.
3. `rollback_restores`: a rollback to a savepoint gives the queue the
   savepoint saw, so `appended` returns what it returned then
   (`appended_after_rollback`).
4. `appended_is_commit_order`: the answer is the entries of the log among the
   queue taken in the order the commit numbers them, so entry number `i` of
   the answer is the one that takes the `i`-th of this transaction's
   positions in the log.
5. `newest_first_breaks_order`: a RED as a concrete counterexample. Without
   the reversal the answer is newest first and differs from commit order.
6. `appended_is_reversed_with_appended` and `with_appended_is_reversed_appended`:
   `with_appended` hands over exactly the entries `appended` returns, in the
   reverse order, which is how `appended` is built from it.

How to read the Lean: `def` defines a function, `theorem` states a fact and
its proof follows `:= by`. Lines starting with `--` are comments. A log and an
entry are plain natural numbers.
-/

-- Everything below is named Regolith.Appended.<name>.
namespace Regolith.Appended

/-- One queued append: the log it names and its entry. Mirrors `PendingAppend`
in src/engine/commit/append.rs. -/
abbrev Pending := Nat × Nat

/-- The append buffer, newest append first. Mirrors the chain `walk_chain`
visits: the head of the list is the newest. -/
abbrev Queue := List Pending

/-- `Transaction::append` of `entry` to `log`: the new append goes on the
front, because the front is the newest. -/
def push (log entry : Nat) (q : Queue) : Queue :=
  -- The new append is the newest.
  (log, entry) :: q

/-- The order the commit numbers a queue in: oldest first. -/
def commitOrder (q : Queue) : Queue :=
  -- The queue is newest first, so reversing it gives oldest first.
  q.reverse

/-- `Transaction::appended` for `log`: the entries of that log, oldest first.
The code walks the chain newest first, keeps the entries of the log, and
reverses them. -/
def appended (log : Nat) (q : Queue) : List Nat :=
  -- Keep the entries that name this log, newest first, then reverse them.
  ((q.filter (fun p => decide (p.1 = log))).map (fun p => p.2)).reverse

/-- `Transaction::with_appended` for `log`: the same entries, newest first,
as the walk finds them. Correct for a reader that does not care about order,
and the bug (`newest_first_breaks_order`) when taken for commit order. -/
def appendedNewestFirst (log : Nat) (q : Queue) : List Nat :=
  -- The same walk and filter, with no reversal.
  (q.filter (fun p => decide (p.1 = log))).map (fun p => p.2)

/-- What the commit does with a queue: it numbers the entries of the log among
it, in commit order. This is the list of entries in the order they take
positions. -/
def numbered (log : Nat) (q : Queue) : List Nat :=
  -- Take the oldest-first queue, keep this log's appends, drop the log names.
  ((commitOrder q).filter (fun p => decide (p.1 = log))).map (fun p => p.2)

/-- `appended` keeps the entries of the log, last append last: appending to
the same log adds the entry at the end of the answer. -/
theorem appended_push_same (log entry : Nat) (q : Queue) :
    -- The statement: the old answer with the new entry at the end.
    appended log (push log entry q) = appended log q ++ [entry] := by
  -- Unfold the definitions and let the simplifier do the list algebra.
  simp [appended, push]

/-- An append to another log leaves the answer for this log unchanged. -/
theorem appended_push_other {log other : Nat} (entry : Nat) (q : Queue)
    -- The other append names a different log.
    (h : other ≠ log) :
    -- The statement: the answer for this log is unchanged.
    appended log (push other entry q) = appended log q := by
  -- The new pair names another log, so the filter drops it.
  simp [appended, push, h]

/-- Queueing a list of appends one after another, in the order given. -/
def pushAll (es : List Pending) (q : Queue) : Queue :=
  -- Each one goes on the front of what is there.
  es.foldl (fun acc e => e :: acc) q

/-- Pushing a list one at a time puts the list, reversed, on the front. -/
theorem pushAll_eq (es : List Pending) (q : Queue) :
    -- The statement: the pushes are the reversed list in front of the queue.
    pushAll es q = es.reverse ++ q := by
  -- Generalise the queue, then go through the list one element at a time.
  induction es generalizing q with
  -- No appends: nothing changes.
  | nil => simp [pushAll]
  -- One more append: the induction hypothesis handles the rest.
  | cons e es ih => simp [pushAll] at ih ⊢

/-- A savepoint remembers how many appends the queue held. A rollback to it
drops the newest appends beyond that count. Mirrors `truncate_appends`. -/
def rollback (mark : Nat) (q : Queue) : Queue :=
  -- The extra appends are the newest, which are at the front.
  q.drop (q.length - mark)

/-- A rollback to a savepoint gives the queue back exactly as the savepoint
saw it, whatever was appended after. -/
theorem rollback_restores (q : Queue) (es : List Pending) :
    -- The statement: the rollback gives back the savepoint's queue.
    rollback q.length (pushAll es q) = q := by
  -- Rewrite the pushes as a reversed list on the front.
  rw [pushAll_eq]
  -- The number to drop is the number of appends pushed since the savepoint.
  have hlen : (es.reverse ++ q).length - q.length = es.reverse.length := by
    -- Lengths add, and the extra one cancels.
    simp
  -- Unfold the rollback, then use the length fact.
  unfold rollback
  -- Replace the number to drop by that count.
  rw [hlen]
  -- Dropping exactly the front part leaves the rest.
  exact List.drop_left' rfl

/-- After a rollback to a savepoint, `appended` returns what it returned at
the savepoint. -/
theorem appended_after_rollback (log : Nat) (q : Queue) (es : List Pending) :
    -- The statement: the answer after the rollback is the answer at the savepoint.
    appended log (rollback q.length (pushAll es q)) = appended log q := by
  -- The queue is the one the savepoint saw.
  rw [rollback_restores]

/-- The answer is the log's share of the queue in the order the commit numbers
it: entry `i` of the answer takes the `i`-th of this transaction's positions. -/
theorem appended_is_commit_order (log : Nat) (q : Queue) :
    -- The statement: the answer is the commit's numbering of this log.
    appended log q = numbered log q := by
  -- Filtering and mapping commute with reversal.
  simp [appended, numbered, commitOrder, List.filter_reverse, List.map_reverse]

/-- RED: with no reversal the answer is not in commit order. Two appends to
one log, `7` then `8`, give `[8, 7]` where the commit numbers `[7, 8]`. -/
theorem newest_first_breaks_order :
    -- The statement: the unreversed answer differs from the numbering.
    appendedNewestFirst 1 (push 1 8 (push 1 7 [])) ≠ numbered 1 (push 1 8 (push 1 7 [])) := by
  -- Evaluate both sides: `[8, 7]` against `[7, 8]`.
  decide

/-- `appended` is `with_appended`'s answer reversed: the code collects what
`with_appended` visits and reverses it. -/
theorem appended_is_reversed_with_appended (log : Nat) (q : Queue) :
    -- The statement: reversing the newest-first answer gives `appended`.
    appended log q = (appendedNewestFirst log q).reverse := by
  -- Both sides are the same filter and map, reversed.
  rfl

/-- `with_appended` visits the entries `appended` returns, newest first: the
same entries, none more and none fewer, in the reverse order. -/
theorem with_appended_is_reversed_appended (log : Nat) (q : Queue) :
    -- The statement: the newest-first answer is `appended` read backwards.
    appendedNewestFirst log q = (appended log q).reverse := by
  -- Unfold `appended`; reversing twice gives the list back.
  simp [appended, appendedNewestFirst]

-- Close the namespace.
end Regolith.Appended
