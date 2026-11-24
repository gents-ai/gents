/-!
# Callbacks: transaction callbacks run exactly once, in order, per attempt

## The story, for a reader who has never seen the code

A transaction is a batch of reads and writes that either all take effect
(it "commits") or none do (it "aborts"). A caller can hang callbacks on one
(`src/transaction/callbacks.rs`): `before_commit` runs just before the
commit is decided and may still write, `on_commit` runs after the
transaction committed, `on_abort` after it ended without committing. A
database can also have "hooks" (`TransactionHooks`): the same three points
for every transaction. `transact` (`src/transaction/retry.rs`) runs a
closure in a transaction and, when the commit loses a race (a "conflict"),
runs it again in a fresh one: each run is an "attempt".

Tiny example of what could go wrong. A shop sends "your order is placed"
from `on_commit` and releases the reserved stock from `on_abort`. Attempt 0
conflicts and attempt 1 commits. The customer must get the message once,
for attempt 1; the stock reserved by attempt 0 must be released once; the
message must not go out for attempt 0, nor twice, nor before validation.

What the proofs show: every callback runs exactly once, of the kind its
attempt's outcome names, in the documented order, for its own attempt only,
and on the thread that holds the commit's queue. The numbers in the model
(attempts, callbacks, keys) are unbounded: the theorems hold for all of
them.

## What this file backs

This file backs the TLA+ model `proofs/tla/TxnCallbacks.tla`, invariants
`AtMostOnce`, `ExactlyOnce`, `CallbacksBeforeHooks`, `NoLostUpdate`,
`AttemptIsolation`, `PanicAborts` and `OnCommittingThread` (configurations
`MC_TxnCallbacks_*`). TLC checks there how the outcome reaches a
transaction concurrently, in both forms a commit takes: the blocking commit
that `transact` calls, which tells its outcome when it returns, and
`commit_nowait`, whose pending ticket is resolved and delivered by the poll
or the drop of the committing thread's queue; with `close` and the owner
racing them. Here the outcome
state machine of one `transact` call (plan 3.16) is a sequential machine:
attempts one after another, each registering callbacks, running its
before_commit pass, validating, and ending with exactly one outcome. The
theorems hold for every number of attempts, callbacks and writes.

What is proved, in plain words:

1. `step_inv` and `reachable_inv`: the machine's invariant holds in every
   reachable state.
2. `exactly_once`: every on_commit callback registered in attempt `a` has
   run exactly once if attempt `a` committed and never otherwise; every
   on_abort callback has run exactly once, with the attempt's abort reason,
   if the attempt ended without committing, and never otherwise. The
   database hooks' on_commit and on_abort follow the same law, once per
   attempt. An attempt that has not ended has run none of them.
   `never_both`: no attempt runs both an on_commit and an on_abort
   callback.
3. `order`: within an attempt, every event of an earlier stage comes before
   every event of a later stage: the before_commit callbacks, then the
   hooks' before_commit, then validation, then the transaction's outcome
   callbacks, then the hooks' outcome. `callbacks_before_hooks`: once an
   on_commit callback ran, the hooks' on_commit ran too, after it.
   `finish_commit_runs` and
   `finish_abort_runs`: the outcome callbacks run in the order they were
   registered, the hooks after them. `commit_was_validated`: a committed
   attempt was validated, before its on_commit callbacks.
4. `before_writes_validated`: the write set an attempt validates holds every
   write its before_commit callbacks (and the hooks' before_commit) made.
5. `attempts_isolated`: an outcome callback that ran was registered in the
   attempt whose outcome it reports, so a re-run never runs a previous
   attempt's callbacks; `surviving_callbacks_break_isolation` is the RED
   case, as a counterexample.
6. `panic_aborts`: a panic in before_commit ends the attempt with the
   reason `panic`, runs every on_abort callback of the attempt, and no
   on_commit callback.
7. `delivered_once`: an outcome that comes back through a `CommitTicket` is
   delivered, and its callbacks run, at most once, and exactly once when
   the ticket is ready, however many paths try (the queue's poll, the
   queue's drop, the commit's own fallback); `no_claim_delivers_twice` is
   the RED case.
8. `delivered_on_queue_thread`: every delivery runs on the thread that
   holds the commit's queue, never on a committer that synced it (D53, the
   committing thread).
9. `resolved_after_landing` and `resolved_once_on_queue_thread`: a commit
   that was owed an fsync has its outcome read from its commit's slot only
   once the commit has landed, once, by a deliverer on the thread that holds
   the commit's queue; `early_resolve_is_not_durable` is the RED case.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. `[x, y].Sublist l` says `x` comes before `y` in the list `l`.
`l.count e` is how many times `e` occurs in `l`.
-/

-- Everything below is named Regolith.Callbacks.<name>.
namespace Regolith.Callbacks

/-! ## The objects -/

/-- How an attempt ended: committed, or one of the abort reasons of 3.16
(conflict, rollback, drop, an error, close, a panic in before_commit). -/
inductive Outcome where
  /-- The attempt committed. -/
  | commit
  /-- Validation found a conflict; `transact` may re-run. -/
  | conflict
  /-- The caller rolled back. -/
  | rollback
  /-- The transaction was dropped. -/
  | drop
  /-- The closure, or a before_commit callback, returned an error. -/
  | error
  /-- The database was closed. -/
  | close
  /-- A before_commit callback panicked (`Error::CallbackPanicked`). -/
  | panic
  -- Lean may decide whether two outcomes are equal, so the examples at the end can be computed.
  deriving DecidableEq

/-- Where the current attempt is. -/
inductive Phase where
  /-- The closure runs: callbacks are registered and writes buffered. -/
  | open_
  /-- `commit` was called: the before_commit callbacks run, and may write
  and register more callbacks. -/
  | preparing
  /-- The hooks' before_commit ran: the write set is complete. -/
  | prepared
  /-- Validation ran, with this verdict (`true`: no conflict). -/
  | validated (ok : Bool)
  /-- The outcome's callbacks ran. -/
  | ended
  -- Lean may decide whether two phases are equal.
  deriving DecidableEq

/-- One entry of the trace: a callback, a hook or validation that ran.
The first argument is always the attempt. -/
inductive Ev where
  /-- Before_commit callback `c` of attempt `a` ran. -/
  | before (a c : Nat)
  /-- The database hooks' before_commit ran for attempt `a`. -/
  | hookBefore (a : Nat)
  /-- Attempt `a` was validated with write set `ws`. -/
  | validate (a : Nat) (ws : List Nat)
  /-- On_commit callback `c` of attempt `a` ran. -/
  | onCommit (a c : Nat)
  /-- On_abort callback `c` of attempt `a` ran, with reason `o`. -/
  | onAbort (a c : Nat) (o : Outcome)
  /-- The database hooks' on_commit ran for attempt `a`. -/
  | hookCommit (a : Nat)
  /-- The database hooks' on_abort ran for attempt `a`, with reason `o`. -/
  | hookAbort (a : Nat) (o : Outcome)
  -- Lean may decide whether two events are equal, which counting events in a list needs.
  deriving DecidableEq

/-- The attempt an event belongs to. -/
def Ev.att : Ev → Nat
  -- A before_commit callback's event: its attempt.
  | .before a _ => a
  -- The hooks' before_commit: its attempt.
  | .hookBefore a => a
  -- A validation: its attempt.
  | .validate a _ => a
  -- An on_commit callback's event: its attempt.
  | .onCommit a _ => a
  -- An on_abort callback's event: its attempt.
  | .onAbort a _ _ => a
  -- The hooks' on_commit: its attempt.
  | .hookCommit a => a
  -- The hooks' on_abort: its attempt.
  | .hookAbort a _ => a

/-- The stage of an event inside its attempt: before_commit callbacks (0),
the hooks' before_commit (1), validation (2), the transaction's outcome
callbacks (3), the hooks' outcome (4). -/
def Ev.rank : Ev → Nat
  -- A before_commit callback: stage 0.
  | .before .. => 0
  -- The hooks' before_commit: stage 1.
  | .hookBefore _ => 1
  -- Validation: stage 2.
  | .validate .. => 2
  -- An on_commit callback: stage 3.
  | .onCommit .. => 3
  -- An on_abort callback: stage 3 too.
  | .onAbort .. => 3
  -- The hooks' on_commit: stage 4.
  | .hookCommit _ => 4
  -- The hooks' on_abort: stage 4 too.
  | .hookAbort .. => 4

/-- The latest stage a phase has reached. -/
def Phase.rank : Phase → Nat
  -- The closure runs: nothing has run yet, stage 0.
  | .open_ => 0
  -- The before_commit pass: stage 0.
  | .preparing => 0
  -- The hooks' before_commit ran: stage 1.
  | .prepared => 1
  -- Validated: stage 2.
  | .validated _ => 2
  -- Ended: the last stage, 4.
  | .ended => 4

/-- The state of one `transact` call. -/
structure T where
  /-- The current attempt, counted from 0. -/
  att : Nat
  /-- Where the current attempt is. -/
  phase : Phase
  /-- The current attempt's outcome, once it ended. -/
  cur : Option Outcome
  /-- Before_commit callbacks registered and not yet run, in order. -/
  befores : List Nat
  /-- The current attempt's on_commit callbacks, in registration order. -/
  commits : List Nat
  /-- The current attempt's on_abort callbacks, in registration order. -/
  aborts : List Nat
  /-- The keys the current attempt wrote: its closure, its callbacks and
  the hooks. -/
  writes : List Nat
  /-- Ghost: every event, in order. -/
  log : List Ev
  /-- Ghost: every on_commit registration `(attempt, callback)`. -/
  regC : List (Nat × Nat)
  /-- Ghost: every on_abort registration `(attempt, callback)`. -/
  regA : List (Nat × Nat)
  /-- Ghost: every `(attempt, key)` a before_commit callback or the hooks'
  before_commit wrote. -/
  bw : List (Nat × Nat)
  -- Lean may decide whether two states are equal, so the examples at the end can be computed.
  deriving DecidableEq

/-- The start: attempt 0, open, nothing registered or run. -/
def init : T := ⟨0, .open_, none, [], [], [], [], [], [], [], []⟩

/-- Attempt `a`'s outcome: every earlier attempt ended with a conflict
(`transact` re-runs only on a conflict), the current one has `cur`, and a
later one has none yet. -/
def outcomeOf (s : T) (a : Nat) : Option Outcome :=
  -- An earlier attempt: a conflict. The current one: `cur`. A later one: nothing yet.
  if a < s.att then some .conflict else if a = s.att then s.cur else none

/-- The aborts a phase allows. A conflict comes only from validation;
rollback, drop, an error and close may end an attempt any time before it
is validated. (A panic has its own step.) -/
def Allowed (p : Phase) (o : Outcome) : Prop :=
  -- Either validation found a conflict and the reason is that conflict; or the attempt is not
  -- validated yet and the reason is a rollback, a drop, an error or close.
  (p = .validated false ∧ o = .conflict) ∨
  ((p = .open_ ∨ p = .preparing ∨ p = .prepared) ∧
    (o = .rollback ∨ o = .drop ∨ o = .error ∨ o = .close))

/-- The fresh transaction `transact` re-runs a conflicted attempt in: the
next attempt, open, with no outcome, no callback and no write. -/
def retried (s : T) : T :=
  -- The attempt number goes up by one and everything of the old attempt is cleared. The ghosts (the
  -- log, the registrations, the callback writes) stay.
  { s with att := s.att + 1, phase := .open_, cur := none, befores := [], commits := [],
           aborts := [], writes := [] }

/-- The events of a commit: the transaction's on_commit callbacks in
registration order, then the hooks' on_commit. -/
def commitChunk (s : T) : List Ev :=
  -- One on_commit event per callback, in order, then the hooks' on_commit.
  s.commits.map (Ev.onCommit s.att) ++ [.hookCommit s.att]

/-- The events of an abort with reason `o`: the transaction's on_abort
callbacks in registration order, then the hooks' on_abort. -/
def abortChunk (s : T) (o : Outcome) : List Ev :=
  -- One on_abort event per callback, in order, each with the reason, then the hooks' on_abort.
  s.aborts.map (fun c => Ev.onAbort s.att c o) ++ [.hookAbort s.att o]

/-! ## The steps -/

/-- The design's steps. -/
inductive Step : T → T → Prop
  /-- Register on_commit callback `c` (fresh in this attempt), while the
  closure runs or during the before_commit pass. -/
  | regCommit (s : T) (c : Nat)
      -- The closure runs, or the before_commit pass does.
      (hp : s.phase = .open_ ∨ s.phase = .preparing)
      -- `c` is not an on_commit callback of this attempt yet.
      (hc : (s.att, c) ∉ s.regC) :
      -- After the step: c is at the end of the on_commit list, and the registration is recorded.
      Step s { s with commits := s.commits ++ [c], regC := s.regC ++ [(s.att, c)] }
  /-- Register on_abort callback `c` (fresh in this attempt). -/
  | regAbort (s : T) (c : Nat)
      -- The closure runs, or the before_commit pass does.
      (hp : s.phase = .open_ ∨ s.phase = .preparing)
      -- `c` is not an on_abort callback of this attempt yet.
      (hc : (s.att, c) ∉ s.regA) :
      -- After the step: c is at the end of the on_abort list, and the registration is recorded.
      Step s { s with aborts := s.aborts ++ [c], regA := s.regA ++ [(s.att, c)] }
  /-- Register before_commit callback `c`; one registered during the pass
  runs in the same pass. -/
  | regBefore (s : T) (c : Nat)
      -- The closure runs, or the before_commit pass does.
      (hp : s.phase = .open_ ∨ s.phase = .preparing) :
      -- After the step: c is at the end of the pending before_commit callbacks.
      Step s { s with befores := s.befores ++ [c] }
  /-- The closure writes key `k`. -/
  | write (s : T) (k : Nat)
      -- The closure runs.
      (hp : s.phase = .open_) :
      -- After the step: k is in the attempt's write set.
      Step s { s with writes := s.writes ++ [k] }
  /-- The closure returns and `commit` (or `prepare`) is called. -/
  | commit (s : T)
      -- The closure ran.
      (hp : s.phase = .open_) :
      -- After the step: the before_commit pass runs.
      Step s { s with phase := .preparing }
  /-- The oldest pending before_commit callback runs and writes `ws`
  through the transaction. -/
  | runBefore (s : T) (c : Nat) (rest ws : List Nat)
      -- The before_commit pass runs.
      (hp : s.phase = .preparing)
      -- `c` is the oldest pending before_commit callback.
      (hb : s.befores = c :: rest) :
      -- After the step: c is no longer pending and its run is logged;
      Step s { s with befores := rest, log := s.log ++ [.before s.att c],
                      -- its writes join the write set, and are recorded as callback writes of this
                      -- attempt.
                      writes := s.writes ++ ws, bw := s.bw ++ ws.map (fun k => (s.att, k)) }
  /-- With every before_commit callback run, the hooks' before_commit runs
  and writes `ws`. -/
  | hookBefore (s : T) (ws : List Nat)
      -- The before_commit pass runs.
      (hp : s.phase = .preparing)
      -- Every before_commit callback ran.
      (hb : s.befores = []) :
      -- After the step: the phase is "prepared" and the hooks' run is logged;
      Step s { s with phase := .prepared, log := s.log ++ [.hookBefore s.att],
                      -- the hooks' writes join the write set, and are recorded as callback writes
                      -- of this attempt.
                      writes := s.writes ++ ws, bw := s.bw ++ ws.map (fun k => (s.att, k)) }
  /-- Validation of the whole write set, with verdict `ok`. -/
  | validate (s : T) (ok : Bool)
      -- Every before_commit callback, the hooks' included, ran.
      (hp : s.phase = .prepared) :
      -- After the step: the phase holds the verdict, and the validation is logged with the whole
      -- write set.
      Step s { s with phase := .validated ok, log := s.log ++ [.validate s.att s.writes] }
  /-- A validated attempt commits: its on_commit callbacks run, then the
  hooks'. -/
  | finishCommit (s : T)
      -- Validation found no conflict.
      (hp : s.phase = .validated true) :
      -- After the step: the attempt ended, its outcome is a commit, and the commit's events are
      -- logged.
      Step s { s with phase := .ended, cur := some .commit, log := s.log ++ commitChunk s }
  /-- The attempt ends without committing, for a reason its phase allows:
  its on_abort callbacks run, then the hooks'. -/
  | finishAbort (s : T) (o : Outcome)
      -- The phase allows ending with `o`.
      (ho : Allowed s.phase o) :
      -- After the step: the attempt ended with reason o, and the abort's events are logged.
      Step s { s with phase := .ended, cur := some o, log := s.log ++ abortChunk s o }
  /-- The oldest pending before_commit callback panics: the commit fails
  with `CallbackPanicked`, and the on_abort callbacks run. -/
  | panicBefore (s : T) (c : Nat) (rest : List Nat)
      -- The before_commit pass runs.
      (hp : s.phase = .preparing)
      -- `c` is the oldest pending before_commit callback, the one that panics.
      (hb : s.befores = c :: rest) :
      -- After the step: the attempt ended with the reason `panic`, and c is no longer pending;
      Step s { s with phase := .ended, cur := some .panic, befores := rest,
                      -- the log gets c's run, then the abort's events.
                      log := s.log ++ (Ev.before s.att c :: abortChunk s .panic) }
  /-- `transact` re-runs a conflicted attempt in a fresh transaction: no
  callback and no write carries over (the state `retried s`). -/
  | retry (s : T)
      -- The attempt ended ...
      (hp : s.phase = .ended)
      -- ... with a conflict, the only outcome `transact` re-runs.
      (hc : s.cur = some .conflict) :
      -- After the step: the next attempt, open, with no outcome, no pending callback,
      Step s { s with att := s.att + 1, phase := .open_, cur := none, befores := [],
                      -- no outcome callback and no write.
                      commits := [], aborts := [], writes := [] }

/-- Any number of steps, one after another. -/
inductive Steps : T → T → Prop
  /-- No step at all. -/
  | refl (s : T) : Steps s s
  /-- Some steps, then one more. -/
  | tail {s t u : T} : Steps s t → Step t u → Steps s u

/-! ## The invariant -/

/-- The machine's invariant. -/
structure Inv (s : T) : Prop where
  /-- The current on_commit list is this attempt's registrations, in order. -/
  commits_order : (s.regC.filter (fun p => p.1 == s.att)).map Prod.snd = s.commits
  /-- The current on_abort list is this attempt's registrations, in order. -/
  aborts_order : (s.regA.filter (fun p => p.1 == s.att)).map Prod.snd = s.aborts
  /-- No on_commit callback is registered twice in an attempt. -/
  commits_nodup : s.commits.Nodup
  /-- No on_abort callback is registered twice in an attempt. -/
  aborts_nodup : s.aborts.Nodup
  /-- No registration belongs to a later attempt. -/
  regC_le : ∀ p ∈ s.regC, p.1 ≤ s.att
  /-- No registration belongs to a later attempt. -/
  regA_le : ∀ p ∈ s.regA, p.1 ≤ s.att
  /-- The current attempt has an outcome exactly when it ended. -/
  cur_ended : s.cur = none ↔ s.phase ≠ .ended
  /-- On_commit callback `c` of attempt `a` ran once if registered and the
  attempt committed, else never. -/
  cC : ∀ a c, s.log.count (.onCommit a c) =
    if (a, c) ∈ s.regC ∧ outcomeOf s a = some .commit then 1 else 0
  /-- On_abort callback `c` of attempt `a` ran once with reason `o` if
  registered and the attempt ended aborted with `o`, else never. -/
  cA : ∀ a c o, s.log.count (.onAbort a c o) =
    if (a, c) ∈ s.regA ∧ outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0
  /-- The hooks' on_commit ran once for a committed attempt, else never. -/
  hC : ∀ a, s.log.count (.hookCommit a) = if outcomeOf s a = some .commit then 1 else 0
  /-- The hooks' on_abort ran once with reason `o` for an attempt aborted
  with `o`, else never. -/
  hA : ∀ a o, s.log.count (.hookAbort a o) =
    if outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0
  /-- No event belongs to a later attempt. -/
  att_le : ∀ x ∈ s.log, x.att ≤ s.att
  /-- The current attempt's events are of stages its phase has reached. -/
  rank_le : ∀ x ∈ s.log, x.att = s.att → x.rank ≤ s.phase.rank
  /-- Within an attempt, earlier stages come first. -/
  ordered : ∀ x ∈ s.log, ∀ y ∈ s.log, x.att = y.att → x.rank < y.rank → [x, y].Sublist s.log
  /-- No callback write belongs to a later attempt. -/
  bw_le : ∀ p ∈ s.bw, p.1 ≤ s.att
  /-- The current attempt's callback writes are in its write set. -/
  bw_cur : ∀ k, (s.att, k) ∈ s.bw → k ∈ s.writes
  /-- A validated write set holds every callback write of its attempt. -/
  bw_validated : ∀ a k ws, (a, k) ∈ s.bw → Ev.validate a ws ∈ s.log → k ∈ ws
  /-- A validated current attempt has its validation in the log. -/
  validated_logged : ∀ ok, s.phase = .validated ok → ∃ ws, Ev.validate s.att ws ∈ s.log
  /-- A committed attempt was validated. -/
  commit_validated : ∀ a, outcomeOf s a = some .commit → ∃ ws, Ev.validate a ws ∈ s.log

/-! ## Helpers -/

/-- A one-element list has no repeats. -/
theorem nodup_one {α : Type} (x : α) : [x].Nodup := by simp

/-- Mapping a list without repeats through an injective function keeps it
without repeats. -/
theorem nodup_map {l : List Nat} {f : Nat → Ev}
    -- `f` never maps two numbers to one event.
    (hf : ∀ x y, f x = f y → x = y)
    -- `l` has no repeats.
    (hl : l.Nodup) :
    -- The statement: the mapped list has no repeats.
    (l.map f).Nodup := by
  -- "No repeats" means: any two entries, taken in order, differ.
  unfold List.Nodup at *
  -- For the mapped list: any two entries of l have different images.
  rw [List.pairwise_map]
  -- Two entries of l differ, so their images do: equal images would mean equal entries.
  exact hl.imp (fun h e => h (hf _ _ e))

/-- A list sorted by stage has every earlier-stage element before every
later-stage one. -/
theorem ordered_of_sorted {e : List Ev}
    -- `e` never goes back a stage.
    (hs : e.Pairwise (fun x y => x.rank ≤ y.rank)) :
    -- The statement: for x and y in e with x of an earlier stage, [x, y] appears in e in this
    -- order.
    ∀ x ∈ e, ∀ y ∈ e, x.rank < y.rank → [x, y].Sublist e := by
  -- Induction on the list: first the empty list, then a list with one more entry in front.
  induction e with
  -- The empty list has no entry x.
  | nil => intro x hx; simp at hx
  -- An entry z in front of a list e for which the claim (ih) is already known.
  | cons z e ih =>
    -- Sorted means: z's stage is at most that of everything in e, and e is sorted.
    obtain ⟨hz, he⟩ := List.pairwise_cons.1 hs
    -- Take x and y in the list, x of an earlier stage.
    intro x hx y hy hxy
    -- x is z, or x is in e.
    rcases List.mem_cons.1 hx with rfl | hx'
    -- x is z. And y is z, or y is in e.
    · rcases List.mem_cons.1 hy with rfl | hy'
      -- `x = y = z`: no stage is below itself.
      ·
        -- Arithmetic settles it.
        omega
      -- `z` heads the list and `y` follows it.
      ·
        -- [z] followed by [y], with y in e, sits in z :: e.
        exact List.Sublist.cons_cons _ (List.singleton_sublist.2 hy')
    -- x is in e. And y is z, or y is in e.
    · rcases List.mem_cons.1 hy with rfl | hy'
      -- `y = z` heads the list, so `y`'s stage is at most `x`'s.
      ·
        -- z's stage is at most x's, since x is in e.
        have := hz x hx'
        -- That contradicts x being of an earlier stage.
        omega
      -- Both in the tail.
      ·
        -- The claim for e, and then z is put in front.
        exact (ih he x hx' y hy' hxy).cons z

/-- Appending a stage-sorted chunk of current-attempt events, none of a
stage below what the attempt reached, keeps every attempt ordered. -/
theorem ordered_append {l e : List Ev} {a r : Nat}
    -- The old log is ordered.
    (hord : ∀ x ∈ l, ∀ y ∈ l, x.att = y.att → x.rank < y.rank → [x, y].Sublist l)
    -- The current attempt's old events are of stage at most `r`.
    (hrank : ∀ x ∈ l, x.att = a → x.rank ≤ r)
    -- The chunk belongs to the current attempt.
    (hatt : ∀ y ∈ e, y.att = a)
    -- The chunk's stages are at least `r`.
    (hge : ∀ y ∈ e, r ≤ y.rank)
    -- The chunk is sorted by stage.
    (hs : e.Pairwise (fun x y => x.rank ≤ y.rank)) :
    -- The statement: the same ordering holds for the log followed by the chunk.
    ∀ x ∈ l ++ e, ∀ y ∈ l ++ e, x.att = y.att → x.rank < y.rank → [x, y].Sublist (l ++ e) := by
  -- Take x and y of one attempt, x of an earlier stage.
  intro x hx y hy hxy hlt
  -- Each of them is old (in l) or new (in e): four cases.
  rcases List.mem_append.1 hx with hxl | hxe <;> rcases List.mem_append.1 hy with hyl | hye
  -- Both old.
  ·
    -- Ordered in l, and l sits inside l ++ e.
    exact (hord x hxl y hyl hxy hlt).trans (List.sublist_append_left l e)
  -- `x` old, `y` new: `x` comes first.
  ·
    -- [x] sits in l and [y] in e.
    exact (List.singleton_sublist.2 hxl).append (List.singleton_sublist.2 hye)
  -- `x` new, `y` old, same attempt: `y`'s stage is at most `r`, `x`'s at least `r`, against `x`
  -- being of an earlier stage.
  ·
    -- y is of the current attempt, so its stage is at most r.
    have h1 := hrank y hyl (by rw [← hxy]; exact hatt x hxe)
    -- x is in the chunk, so its stage is at least r.
    have h2 := hge x hxe
    -- Arithmetic: that contradicts x's stage being below y's.
    omega
  -- Both new: the chunk is sorted.
  ·
    -- Ordered in e, and e sits inside l ++ e.
    exact (ordered_of_sorted hs x hxe y hye hlt).trans (List.sublist_append_right l e)

/-- An outcome chunk is sorted: its callbacks share stage 3, the hook is 4. -/
theorem chunk_sorted {l : List Nat} {f : Nat → Ev} {h : Ev}
    -- Every mapped event is of stage 3.
    (hf : ∀ c, (f c).rank = 3)
    -- The hook is of stage 4.
    (hh : h.rank = 4) :
    -- The statement: the chunk never goes back a stage.
    (l.map f ++ [h]).Pairwise (fun x y => x.rank ≤ y.rank) := by
  -- Sorted for two lists joined: each is sorted, and every entry of the first is at most every
  -- entry of the second.
  rw [List.pairwise_append, List.pairwise_map]
  -- The single hook event is sorted by itself; the two other parts are left.
  refine ⟨?_, List.pairwise_singleton _ _, ?_⟩
  -- Equal stages.
  ·
    -- Any two mapped events are of stage 3, and 3 ≤ 3.
    exact List.pairwise_of_forall (fun a b => by rw [hf a, hf b]; exact Nat.le_refl 3)
  -- Take a mapped event x and an entry y of [h].
  · intro x hx y hy
    -- x is f c for some c, and y is h.
    simp only [List.mem_map, List.mem_singleton] at hx hy
    -- Name that c.
    obtain ⟨c, -, rfl⟩ := hx
    -- Replace y by h.
    subst hy
    -- Their stages are 3 and 4.
    rw [hf c, hh]
    -- 3 ≤ 4.
    omega

/-- `outcomeOf` for an attempt other than the current one does not depend
on the current outcome. -/
theorem outcomeOf_ne {s : T} {a : Nat} (o : Option Outcome) (h : a ≠ s.att) :
    -- The statement: changing `cur` does not change attempt a's outcome.
    outcomeOf { s with cur := o } a = outcomeOf s a := by
  -- Unfold the definition: `cur` is read only for a = s.att, which is excluded.
  simp [outcomeOf, h]

/-- The current attempt's outcome is `cur`. -/
theorem outcomeOf_cur (s : T) : outcomeOf s s.att = s.cur := by
  -- Unfold the definition: a = s.att picks `cur`.
  simp [outcomeOf]

/-- `outcomeOf` reads only the attempt and `cur`: in any state `t` at the
same attempt as `s`, the current attempt's outcome is `t.cur` and every
other attempt's is as in `s`. -/
theorem outcomeOf_eq (s t : T)
    -- `t` is at the same attempt as `s`.
    (h : t.att = s.att) (a : Nat) :
    -- The statement: for the current attempt the outcome is t.cur, for any other it is as in s.
    outcomeOf t a = if a = s.att then t.cur else outcomeOf s a := by
  -- Unfold the definition.
  unfold outcomeOf
  -- t's attempt number is s's.
  rw [h]
  -- Split on whether a is an earlier attempt.
  by_cases h1 : a < s.att
  -- An earlier attempt: a conflict in both.
  ·
    -- Then a is not the current attempt.
    have : a ≠ s.att := by omega
    -- Both sides compute to a conflict.
    simp [h1, this]
  -- The current attempt, or a later one.
  ·
    -- Simplify with "not earlier".
    simp [h1]
    -- Split on whether a is the current attempt; both cases compute.
    by_cases h2 : a = s.att <;> simp [h2]

/-- At the start no attempt has an outcome. -/
theorem outcomeOf_init (a : Nat) : outcomeOf init a = none := by
  -- Attempt 0 is current with no outcome; no attempt is earlier.
  by_cases h : a = 0 <;> simp [outcomeOf, init, h]

/-! ## Every step keeps the invariant -/

/-- The start satisfies the invariant. -/
theorem init_inv : Inv init := by
  -- Nothing has run or been registered; no attempt has an outcome.
  refine ⟨rfl, rfl, List.nodup_nil, List.nodup_nil, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_,
    ?_, ?_, ?_⟩ <;> (try simp only [outcomeOf_init]) <;> simp [init]

/-- The current on_commit callbacks are exactly this attempt's
registrations. -/
theorem Inv.mem_commits {s : T}
    -- The invariant holds.
    (h : Inv s) (c : Nat) : c ∈ s.commits ↔ (s.att, c) ∈ s.regC := by
  -- The on_commit list is this attempt's registrations, by the invariant.
  rw [← h.commits_order]
  -- Being in it means: some registration of this attempt carries c.
  simp only [List.mem_map, List.mem_filter, beq_iff_eq]
  -- Two directions.
  constructor
  -- Forward: take that registration, (a, c) with a the current attempt.
  · rintro ⟨⟨a, c'⟩, ⟨hm, ha⟩, rfl⟩
    -- Simplify "a is the current attempt".
    simp only at ha
    -- Replace a by the current attempt.
    subst ha
    -- That registration is the one asked for.
    exact hm
  -- Backward: given the registration,
  · intro hm
    -- it is of this attempt and its callback is c.
    exact ⟨(s.att, c), ⟨hm, rfl⟩, rfl⟩

/-- The current on_abort callbacks are exactly this attempt's
registrations. -/
theorem Inv.mem_aborts {s : T}
    -- The invariant holds.
    (h : Inv s) (c : Nat) : c ∈ s.aborts ↔ (s.att, c) ∈ s.regA := by
  -- The on_abort list is this attempt's registrations, by the invariant.
  rw [← h.aborts_order]
  -- Being in it means: some registration of this attempt carries c.
  simp only [List.mem_map, List.mem_filter, beq_iff_eq]
  -- Two directions.
  constructor
  -- Forward: take that registration, (a, c) with a the current attempt.
  · rintro ⟨⟨a, c'⟩, ⟨hm, ha⟩, rfl⟩
    -- Simplify "a is the current attempt".
    simp only at ha
    -- Replace a by the current attempt.
    subst ha
    -- That registration is the one asked for.
    exact hm
  -- Backward: given the registration,
  · intro hm
    -- it is of this attempt and its callback is c.
    exact ⟨(s.att, c), ⟨hm, rfl⟩, rfl⟩

/-- An attempt that has not ended has no outcome yet. -/
theorem Inv.cur_none {s : T}
    -- The invariant holds.
    (h : Inv s)
    -- The attempt has not ended.
    (hp : s.phase ≠ .ended) : s.cur = none :=
  -- The invariant says: no outcome exactly when the attempt has not ended.
  h.cur_ended.2 hp

/-- Ending the current attempt with outcome `o` by appending the events
`E` (a list without repeats holding exactly that outcome's callbacks and
hook) keeps the four count laws. -/
theorem counts_end {s t : T} {E : List Ev} {o : Outcome}
    -- The invariant held before.
    (hs : Inv s)
    -- The attempt had not ended.
    (hcur : s.cur = none)
    -- The new state: same attempt and registrations, outcome `o`, log
    -- extended by `E`.
    (hatt : t.att = s.att) (hregC : t.regC = s.regC) (hregA : t.regA = s.regA)
    -- The new outcome is o, and the new log is the old one followed by E.
    (htcur : t.cur = some o) (hlog : t.log = s.log ++ E)
    -- `E` has no repeats.
    (hE : E.Nodup)
    -- `E` holds the on_commit callbacks exactly when `o` is a commit.
    (memC : ∀ a c, Ev.onCommit a c ∈ E ↔ a = s.att ∧ o = .commit ∧ c ∈ s.commits)
    -- `E` holds the on_abort callbacks, with reason `o`, exactly when `o`
    -- is an abort.
    (memA : ∀ a c o', Ev.onAbort a c o' ∈ E ↔
      a = s.att ∧ o' = o ∧ o ≠ .commit ∧ c ∈ s.aborts)
    -- `E` holds the hooks' on_commit exactly when `o` is a commit.
    (memHC : ∀ a, Ev.hookCommit a ∈ E ↔ a = s.att ∧ o = .commit)
    -- `E` holds the hooks' on_abort, with reason `o`, exactly when `o` is
    -- an abort.
    (memHA : ∀ a o', Ev.hookAbort a o' ∈ E ↔ a = s.att ∧ o' = o ∧ o ≠ .commit) :
    -- The statement, four laws for the new state. On_commit callbacks: once if registered and
    -- committed, else never.
    (∀ a c, t.log.count (.onCommit a c) =
      if (a, c) ∈ t.regC ∧ outcomeOf t a = some .commit then 1 else 0) ∧
    -- On_abort callbacks: once with the reason if registered and aborted with it, else never.
    (∀ a c o', t.log.count (.onAbort a c o') =
      if (a, c) ∈ t.regA ∧ outcomeOf t a = some o' ∧ o' ≠ .commit then 1 else 0) ∧
    -- The hooks' on_commit: once for a committed attempt, else never.
    (∀ a, t.log.count (.hookCommit a) = if outcomeOf t a = some .commit then 1 else 0) ∧
    -- The hooks' on_abort: once with the reason for an aborted attempt, else never.
    (∀ a o', t.log.count (.hookAbort a o') =
      if outcomeOf t a = some o' ∧ o' ≠ .commit then 1 else 0) := by
  -- Before the step, the current attempt had no outcome, so none of its
  -- outcome events were in the log.
  have hold : outcomeOf s s.att = none := by rw [outcomeOf_cur, hcur]
  -- Four laws to show, one per kind of event.
  refine ⟨fun a c => ?_, fun a c o' => ?_, fun a => ?_, fun a o' => ?_⟩
  -- On_commit callbacks. The count in the new log is the old count plus the count in E; the old
  -- count is given by the invariant, and E, having no repeats, holds each event once or not at all.
  · rw [hlog, List.count_append, hs.cC, hE.count]
    -- Rewrite with what is known of the new state.
    simp only [outcomeOf_eq s t hatt, hregC, memC, htcur]
    -- Split on whether a is the current attempt.
    by_cases ha : a = s.att
    -- The current attempt: nothing before, the chunk now.
    ·
      -- Replace a by the current attempt.
      subst ha
      -- Compute: no old outcome, and "in the on_commit list" is "registered".
      simp [hold, ← hs.mem_commits, and_comm]
    -- Another attempt: nothing of it is in E, and its outcome did not change.
    · simp [ha]
  -- On_abort callbacks. As before: the old count plus the count in E, each known.
  · rw [hlog, List.count_append, hs.cA, hE.count]
    -- Rewrite with what is known of the new state.
    simp only [outcomeOf_eq s t hatt, hregA, memA, htcur]
    -- Split on whether a is the current attempt.
    by_cases ha : a = s.att
    -- The current attempt: nothing before; the chunk holds `c` with reason `o` exactly when `c` is
    -- registered and `o` is an abort.
    ·
      -- Replace a by the current attempt.
      subst ha
      -- "c is in the on_abort list" is "c is registered in this attempt".
      have hm := hs.mem_aborts c
      -- Split on whether the reason asked about is the outcome's.
      by_cases h1 : o' = o
      -- The same reason: replace o' by o.
      · subst h1
        -- Both sides compute to the same.
        simp [hold, hm, and_comm]
      -- Another reason: both sides are 0.
      · simp [hold, h1, Ne.symm h1]
    -- Another attempt: nothing of it is in E, and its outcome did not change.
    · simp [ha]
  -- The hooks' on_commit. The old count plus the count in E, each known.
  · rw [hlog, List.count_append, hs.hC, hE.count]
    -- Rewrite with what is known of the new state.
    simp only [outcomeOf_eq s t hatt, memHC, htcur]
    -- Split on whether a is the current attempt.
    by_cases ha : a = s.att
    -- The current attempt: nothing before, and E holds the event exactly for a commit.
    · subst ha; simp [hold]
    -- Another attempt: unchanged.
    · simp [ha]
  -- The hooks' on_abort. The old count plus the count in E, each known.
  · rw [hlog, List.count_append, hs.hA, hE.count]
    -- Rewrite with what is known of the new state.
    simp only [outcomeOf_eq s t hatt, memHA, htcur]
    -- Split on whether a is the current attempt.
    by_cases ha : a = s.att
    -- The current attempt: replace a by it.
    · subst ha
      -- Split on whether the reason asked about is the outcome's.
      by_cases h1 : o' = o
      -- The same reason: both sides compute to the same.
      · subst h1; simp [hold]
      -- Another reason: both sides are 0.
      · simp [hold, h1, Ne.symm h1]
    -- Another attempt: unchanged.
    · simp [ha]

/-- A step that leaves the attempt, the outcome, the registrations and the
outcome events alone, and appends only events of stages 0 to 2, keeps the
four count laws. -/
theorem counts_append_quiet {s : T} {e : List Ev}
    -- The four count laws held before (`Inv.cC`, `cA`, `hC`, `hA`).
    (hcC : ∀ a c, s.log.count (.onCommit a c) =
      if (a, c) ∈ s.regC ∧ outcomeOf s a = some .commit then 1 else 0)
    -- The on_abort law.
    (hcA : ∀ a c o, s.log.count (.onAbort a c o) =
      if (a, c) ∈ s.regA ∧ outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0)
    -- The hooks' on_commit law.
    (hhC : ∀ a, s.log.count (.hookCommit a) = if outcomeOf s a = some .commit then 1 else 0)
    -- The hooks' on_abort law.
    (hhA : ∀ a o, s.log.count (.hookAbort a o) =
      if outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0)
    -- The appended events are of the early stages.
    (he : ∀ x ∈ e, x.rank ≤ 2) :
    -- The statement: the same four laws for the longer log. On_commit callbacks;
    (∀ a c, (s.log ++ e).count (.onCommit a c) =
      if (a, c) ∈ s.regC ∧ outcomeOf s a = some .commit then 1 else 0) ∧
    -- on_abort callbacks;
    (∀ a c o, (s.log ++ e).count (.onAbort a c o) =
      if (a, c) ∈ s.regA ∧ outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0) ∧
    -- the hooks' on_commit;
    (∀ a, (s.log ++ e).count (.hookCommit a) = if outcomeOf s a = some .commit then 1 else 0) ∧
    -- the hooks' on_abort.
    (∀ a o, (s.log ++ e).count (.hookAbort a o) =
      if outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0) := by
  -- No outcome event is among the appended ones: their stages are 3 and 4.
  have z : ∀ x, 3 ≤ x.rank → e.count x = 0 := by
    -- Take an event x of stage 3 or more.
    intro x hx
    -- Counting 0 means: x is not in e.
    apply List.count_eq_zero.2
    -- Suppose it is.
    intro hm
    -- Then its stage is at most 2,
    have := he x hm
    -- which contradicts stage 3 or more.
    omega
  -- Four laws to show, one per kind of event.
  refine ⟨fun a c => ?_, fun a c o => ?_, fun a => ?_, fun a o => ?_⟩
  -- On_commit callbacks: the count in the longer log is the old count plus 0.
  · rw [List.count_append, z _ (by simp [Ev.rank]), hcC]; rfl
  -- On_abort callbacks: the same.
  · rw [List.count_append, z _ (by simp [Ev.rank]), hcA]; rfl
  -- The hooks' on_commit: the same.
  · rw [List.count_append, z _ (by simp [Ev.rank]), hhC]; rfl
  -- The hooks' on_abort: the same.
  · rw [List.count_append, z _ (by simp [Ev.rank]), hhA]; rfl

/-- What a commit chunk holds: the current on_commit callbacks, and the
hooks' on_commit. -/
theorem mem_commitChunk (s : T) (e : Ev) :
    -- The statement: e is in the chunk exactly when it is the on_commit event of one of the current
    -- callbacks, or the hooks' on_commit event.
    e ∈ commitChunk s ↔ (∃ c ∈ s.commits, e = .onCommit s.att c) ∨ e = .hookCommit s.att := by
  -- Unfold the chunk: being in a mapped list, or in a one-entry list.
  simp [commitChunk, eq_comm]

/-- What an abort chunk holds: the current on_abort callbacks with reason
`o`, and the hooks' on_abort with `o`. -/
theorem mem_abortChunk (s : T) (o : Outcome) (e : Ev) :
    -- The statement: e is in the chunk exactly when it is the on_abort event, with reason o, of one
    -- of the current callbacks, or the hooks' on_abort event with o.
    e ∈ abortChunk s o ↔ (∃ c ∈ s.aborts, e = .onAbort s.att c o) ∨ e = .hookAbort s.att o := by
  -- Unfold the chunk: being in a mapped list, or in a one-entry list.
  simp [abortChunk, eq_comm]

/-- A commit chunk holds on_commit callback `c` of attempt `a` exactly when
`a` is the current attempt and `c` one of its on_commit callbacks. -/
theorem commitChunk_memC (s : T) (a c : Nat) :
    -- The statement. ("commit = commit" is always true; it is there so the shape matches what
    -- `counts_end` asks for.)
    Ev.onCommit a c ∈ commitChunk s ↔ a = s.att ∧ Outcome.commit = .commit ∧ c ∈ s.commits := by
  -- Spell out what the chunk holds.
  rw [mem_commitChunk]
  -- Two directions.
  constructor
  -- Forward: the event is one of the callback events, or the hook event.
  · rintro (⟨c', hc', h⟩ | h)
    -- One of the callback events.
    ·
      -- Two on_commit events are equal when attempt and callback are.
      simp only [Ev.onCommit.injEq] at h
      -- Replace accordingly.
      obtain ⟨rfl, rfl⟩ := h
      -- The current attempt, a commit, and a current callback.
      exact ⟨rfl, rfl, hc'⟩
    -- The hook event is not a callback event.
    ·
      -- The two events are of different kinds: impossible.
      simp at h
  -- Backward: given the current attempt and a current callback c,
  · rintro ⟨rfl, -, hc⟩
    -- its event is one of the callback events.
    exact Or.inl ⟨c, hc, rfl⟩

/-- An abort chunk with reason `o` holds on_abort callback `c` of attempt
`a` with reason `o'` exactly when `a` is the current attempt, `o' = o`, and
`c` is one of its on_abort callbacks. -/
theorem abortChunk_memA (s : T) {o : Outcome}
    -- `o` is an abort reason.
    (hoc : o ≠ .commit) (a c : Nat) (o' : Outcome) :
    -- The statement: the event is in the chunk exactly when the attempt is current, the reason is o
    -- (an abort reason) and c is a current callback.
    Ev.onAbort a c o' ∈ abortChunk s o ↔ a = s.att ∧ o' = o ∧ o ≠ .commit ∧ c ∈ s.aborts := by
  -- Spell out what the chunk holds.
  rw [mem_abortChunk]
  -- Two directions.
  constructor
  -- Forward: the event is one of the callback events, or the hook event.
  · rintro (⟨c', hc', h⟩ | h)
    -- A callback event: two on_abort events are equal when attempt, callback and reason are.
    · simp only [Ev.onAbort.injEq] at h
      -- Replace accordingly.
      obtain ⟨rfl, rfl, rfl⟩ := h
      -- The current attempt, the same reason, an abort, and a current callback.
      exact ⟨rfl, rfl, hoc, hc'⟩
    -- The hook event is not a callback event: impossible.
    · simp at h
  -- Backward: given the current attempt, the reason and a current callback c,
  · rintro ⟨rfl, rfl, -, hc⟩
    -- its event is one of the callback events.
    exact Or.inl ⟨c, hc, rfl⟩

/-- An abort chunk with reason `o` holds the hooks' on_abort of attempt
`a` with reason `o'` exactly when `a` is current and `o' = o`. -/
theorem abortChunk_memHA (s : T) {o : Outcome}
    -- `o` is an abort reason.
    (hoc : o ≠ .commit) (a : Nat) (o' : Outcome) :
    -- The statement: the hook event is in the chunk exactly when the attempt is current and the
    -- reason is o.
    Ev.hookAbort a o' ∈ abortChunk s o ↔ a = s.att ∧ o' = o ∧ o ≠ .commit := by
  -- Spell out what the chunk holds.
  rw [mem_abortChunk]
  -- Two directions.
  constructor
  -- Forward: the event is a callback event, or the hook event.
  · rintro (⟨c', -, h⟩ | h)
    -- A callback event is not a hook event: impossible.
    · simp at h
    -- The hook event: two of them are equal when attempt and reason are.
    · simp only [Ev.hookAbort.injEq] at h
      -- Replace accordingly.
      obtain ⟨rfl, rfl⟩ := h
      -- The current attempt, the same reason, an abort.
      exact ⟨rfl, rfl, hoc⟩
  -- Backward: given the current attempt and the reason,
  · rintro ⟨rfl, rfl, -⟩
    -- the event is the hook event of the chunk.
    exact Or.inr rfl

/-- A commit chunk has no repeats. -/
theorem commitChunk_nodup {s : T}
    -- No on_commit callback is registered twice.
    (hn : s.commits.Nodup) : (commitChunk s).Nodup := by
  -- Unfold the chunk: the callback events followed by the hook event.
  unfold commitChunk
  -- "No repeats" for two lists joined: none in each, and none shared.
  rw [List.nodup_append]
  -- The callback events are distinct because the callbacks are; a single event is; "none shared" is
  -- left.
  refine ⟨nodup_map (fun x y h => by simpa using h) hn, nodup_one _, ?_⟩
  -- A callback event is not the hook event.
  intro a ha b hb
  -- a is a callback event and b is the hook event.
  simp only [List.mem_map, List.mem_singleton] at ha hb
  -- Name the callback.
  obtain ⟨c, -, rfl⟩ := ha
  -- Replace b by the hook event.
  subst hb
  -- The two are of different kinds.
  simp

/-- An abort chunk has no repeats. -/
theorem abortChunk_nodup {s : T} {o : Outcome}
    -- No on_abort callback is registered twice.
    (hn : s.aborts.Nodup) : (abortChunk s o).Nodup := by
  -- Unfold the chunk: the callback events followed by the hook event.
  unfold abortChunk
  -- "No repeats" for two lists joined: none in each, and none shared.
  rw [List.nodup_append]
  -- The callback events are distinct because the callbacks are; a single event is; "none shared" is
  -- left.
  refine ⟨nodup_map (fun x y h => by simpa using h) hn, nodup_one _, ?_⟩
  -- Take a callback event a and an entry b of the one-entry list.
  intro a ha b hb
  -- a is a callback event and b is the hook event.
  simp only [List.mem_map, List.mem_singleton] at ha hb
  -- Name the callback.
  obtain ⟨c, -, rfl⟩ := ha
  -- Replace b by the hook event.
  subst hb
  -- The two are of different kinds.
  simp

/-- Every event of a commit chunk belongs to the current attempt and is
of stage 3 or 4. -/
theorem commitChunk_att {s : T} : ∀ y ∈ commitChunk s, y.att = s.att ∧ 3 ≤ y.rank := by
  -- Take an event y of the chunk.
  intro y hy
  -- It is a callback event or the hook event; either way its attempt is the current one and its
  -- stage 3 or 4, by computation.
  rcases (mem_commitChunk s y).1 hy with ⟨c, -, rfl⟩ | rfl <;> simp [Ev.att, Ev.rank]

/-- Every event of an abort chunk belongs to the current attempt and is of
stage 3 or 4. -/
theorem abortChunk_att {s : T} {o : Outcome} :
    -- The statement: every event of the chunk is of the current attempt and of stage 3 or more.
    ∀ y ∈ abortChunk s o, y.att = s.att ∧ 3 ≤ y.rank := by
  -- Take an event y of the chunk.
  intro y hy
  -- It is a callback event or the hook event; either way its attempt is the current one and its
  -- stage 3 or 4, by computation.
  rcases (mem_abortChunk s o y).1 hy with ⟨c, -, rfl⟩ | rfl <;> simp [Ev.att, Ev.rank]

/-- No event of stage 4 or below exceeds the ended phase's stage. -/
theorem rank_le_four (x : Ev) : x.rank ≤ 4 := by cases x <;> simp [Ev.rank]

/-- **Every step keeps the invariant.** -/
theorem step_inv {s t : T}
    -- The invariant holds before the step.
    (hs : Inv s)
    -- One step of the design.
    (hstep : Step s t) :
    -- The statement: the invariant holds after the step.
    Inv t := by
  -- Split on the kind of step.
  cases hstep with
  -- Case: an on_commit callback c is registered.
  | regCommit c hp hc =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by rcases hp with h | h <;> simp [h])
    -- The invariant's nineteen facts: four are left to show (the on_commit list, its distinctness,
    -- the registrations' attempts and the on_commit count); the rest are unchanged.
    refine ⟨?_, hs.aborts_order, ?_, hs.aborts_nodup, ?_, hs.regA_le, hs.cur_ended, ?_, hs.cA,
      hs.hC, hs.hA, hs.att_le, hs.rank_le, hs.ordered, hs.bw_le, hs.bw_cur, hs.bw_validated,
      hs.validated_logged, hs.commit_validated⟩
    -- `c` joins the end of this attempt's registrations.
    ·
      -- Filtering and mapping the longer registration list gives the old list followed by what the
      -- new entry gives.
      simp only [List.filter_append, List.map_append, hs.commits_order]
      -- The new entry is of this attempt, so it gives [c].
      simp
    -- `c` is fresh in this attempt.
    ·
      -- "No repeats" for the old list followed by [c]: three parts.
      rw [List.nodup_append]
      -- The old list has none; [c] has none; "none shared" is left.
      refine ⟨hs.commits_nodup, nodup_one c, ?_⟩
      -- Take an old callback x and an entry y of [c], and suppose they are equal.
      intro x hx y hy hxy
      -- y is c.
      simp only [List.mem_singleton] at hy
      -- Replace: x is c.
      subst hy; subst hxy
      -- Then c was registered in this attempt already, against the step's condition.
      exact hc ((hs.mem_commits x).1 hx)
    -- The new registration is of the current attempt.
    ·
      -- Take a registration p of the new list.
      intro p hp'
      -- It is old or the new one.
      rcases List.mem_append.1 hp' with h | h
      -- Old: as before.
      · exact hs.regC_le p h
      -- The new one: its attempt is the current one.
      · simp only [List.mem_singleton] at h; subst h; exact Nat.le_refl _
    -- The new registration's attempt has no outcome: no count changes.
    ·
      -- Take an attempt a and a callback c'.
      intro a c'
      -- The log is unchanged, so the count is the old one, given by the invariant.
      rw [hs.cC a c']
      -- Split on whether a is the current attempt.
      by_cases ha : a = s.att
      -- The current attempt has no outcome: both sides are 0.
      · subst ha; simp [outcomeOf, hcur]
      -- Another attempt: the new registration is not of it, so the condition is unchanged.
      · simp [ha, outcomeOf]
  -- Case: an on_abort callback c is registered. The same proof, for the on_abort facts.
  | regAbort c hp hc =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by rcases hp with h | h <;> simp [h])
    -- The nineteen facts: four are left to show; the rest are unchanged.
    refine ⟨hs.commits_order, ?_, hs.commits_nodup, ?_, hs.regC_le, ?_, hs.cur_ended, hs.cC, ?_,
      hs.hC, hs.hA, hs.att_le, hs.rank_le, hs.ordered, hs.bw_le, hs.bw_cur, hs.bw_validated,
      hs.validated_logged, hs.commit_validated⟩
    -- c joins the end of this attempt's on_abort registrations: filter and map the longer list.
    · simp only [List.filter_append, List.map_append, hs.aborts_order]
      -- The new entry is of this attempt, so it gives [c].
      simp
    -- c is fresh: "no repeats" for the old list followed by [c].
    · rw [List.nodup_append]
      -- The old list has none; [c] has none; "none shared" is left.
      refine ⟨hs.aborts_nodup, nodup_one c, ?_⟩
      -- Take an old callback x and an entry y of [c], and suppose they are equal.
      intro x hx y hy hxy
      -- y is c.
      simp only [List.mem_singleton] at hy
      -- Replace: x is c.
      subst hy; subst hxy
      -- Then c was registered in this attempt already, against the step's condition.
      exact hc ((hs.mem_aborts x).1 hx)
    -- The new registration is of the current attempt. Take a registration p.
    · intro p hp'
      -- It is old or the new one.
      rcases List.mem_append.1 hp' with h | h
      -- Old: as before.
      · exact hs.regA_le p h
      -- The new one: its attempt is the current one.
      · simp only [List.mem_singleton] at h; subst h; exact Nat.le_refl _
    -- No count changes. Take an attempt a, a callback c' and a reason o.
    · intro a c' o
      -- The log is unchanged, so the count is the old one.
      rw [hs.cA a c' o]
      -- Split on whether a is the current attempt.
      by_cases ha : a = s.att
      -- The current attempt has no outcome: both sides are 0.
      · subst ha; simp [outcomeOf, hcur]
      -- Another attempt: the condition is unchanged.
      · simp [ha, outcomeOf]
  -- Case: a before_commit callback is registered.
  | regBefore c hp =>
    -- Only the pending before_commit list changes.
    exact ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, hs.cur_ended, hs.cC, hs.cA, hs.hC, hs.hA, hs.att_le, hs.rank_le, hs.ordered,
      hs.bw_le, hs.bw_cur, hs.bw_validated, hs.validated_logged, hs.commit_validated⟩
  -- Case: the closure writes a key.
  | write k hp =>
    -- Only the write set grows.
    exact ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, hs.cur_ended, hs.cC, hs.cA, hs.hC, hs.hA, hs.att_le, hs.rank_le, hs.ordered,
      hs.bw_le, fun k' h => List.mem_append.2 (Or.inl (hs.bw_cur k' h)), hs.bw_validated,
      hs.validated_logged, hs.commit_validated⟩
  -- Case: `commit` is called.
  | commit hp =>
    -- Open becomes preparing: the same stage, still no outcome.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The nineteen facts: three are left to show; the rest are unchanged.
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, ?_, hs.cC, hs.cA, hs.hC, hs.hA, hs.att_le, ?_, hs.ordered, hs.bw_le, hs.bw_cur,
      hs.bw_validated, ?_, hs.commit_validated⟩
    -- No outcome, and the phase is not "ended".
    · simp [hcur]
    -- The stages: take an event x of the current attempt.
    · intro x hx hxa
      -- Its stage was at most the open phase's.
      have := hs.rank_le x hx hxa
      -- Open and preparing are both stage 0.
      simp only [hp, Phase.rank] at this ⊢
      -- So the bound is the same.
      exact this
    -- The phase is "preparing", not "validated": nothing to show.
    · intro ok h; simp at h
  -- Case: the oldest pending before_commit callback runs.
  | runBefore c rest ws hp hb =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The four count laws survive: the new event is of stage 0.
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_append_quiet (e := [Ev.before s.att c])
      -- The four old laws, and: a before_commit event is of stage 0, at most 2.
      hs.cC hs.cA hs.hC hs.hA (by simp [Ev.rank])
    -- The nineteen facts: seven are left to show; the phase is not "validated".
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, hs.cur_ended, hcC, hcA, hhC, hhA, ?_, ?_, ?_, ?_, ?_, ?_,
      fun ok h => by simp [hp] at h, ?_⟩
    -- The new event is of the current attempt.
    ·
      -- Take an event x of the new log.
      intro x hx
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- The new one: its attempt is the current one.
      · simp at h; subst h; simp [Ev.att]
    -- Stage 0, which the preparing phase has reached.
    ·
      -- Take an event x of the current attempt.
      intro x hx hxa
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.rank_le x h hxa
      -- The new one: stage 0, and the phase's stage is 0.
      · simp at h; subst h; simp [Ev.rank, hp, Phase.rank]
    -- The order: by `ordered_append`, with the one new event as the chunk.
    · exact ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- The new event is of the current attempt.
        (fun y hy => by simp at hy; subst hy; rfl)
        -- Its stage, 0, is at least the phase's.
        (fun y hy => by simp at hy; subst hy; simp [Ev.rank, hp, Phase.rank])
        -- A single event is sorted.
        (List.pairwise_singleton _ _)
    -- The callback's writes are of the current attempt.
    ·
      -- Take a recorded callback write p.
      intro p hp'
      -- It is old or new.
      rcases List.mem_append.1 hp' with h | h
      -- Old: as before.
      · exact hs.bw_le p h
      -- New: it is (the current attempt, k) for a key k the callback wrote.
      · simp only [List.mem_map] at h
        -- Name that key.
        obtain ⟨k, -, rfl⟩ := h
        -- Its attempt is the current one.
        exact Nat.le_refl _
    -- The callback's writes join the write set.
    ·
      -- Take a key k recorded as a callback write of the current attempt.
      intro k hk
      -- The record is old or new.
      rcases List.mem_append.1 hk with h | h
      -- Old: k was in the write set, and still is.
      · exact List.mem_append.2 (Or.inl (hs.bw_cur k h))
      -- New: k is one of the keys the callback wrote.
      · simp only [List.mem_map, Prod.mk.injEq] at h
        -- Name it.
        obtain ⟨k', hk', -, rfl⟩ := h
        -- Those keys were added to the write set.
        exact List.mem_append.2 (Or.inr hk')
    -- No validation is added, and the current attempt has none yet.
    ·
      -- Take a callback write (a, k) and a validation of attempt a with write set ws'.
      intro a k ws' hk hv
      -- The validation event is old, or it is the new event.
      rcases List.mem_append.1 hv with hv | hv
      -- Old. The callback write is old or new.
      · rcases List.mem_append.1 hk with h | h
        -- Both old: as before.
        · exact hs.bw_validated a k ws' h hv
        -- A new write: it is of the current attempt.
        · simp only [List.mem_map, Prod.mk.injEq] at h
          -- Replace a by the current attempt.
          obtain ⟨k', -, rfl, rfl⟩ := h
          -- Then the log held a validation of the current attempt, of a stage at most the phase's.
          have := hs.rank_le _ hv rfl
          -- But validation is stage 2 and the preparing phase is stage 0: impossible.
          simp [Ev.rank, hp, Phase.rank] at this
      -- The new event is not a validation: impossible.
      · simp at hv
    -- A committed attempt was validated. Take a committed attempt a.
    · intro a ha
      -- Its validation was in the old log.
      obtain ⟨ws', hws⟩ := hs.commit_validated a ha
      -- So it is in the longer log.
      exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: the hooks' before_commit runs.
  | hookBefore ws hp hb =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The four count laws survive: the new event is of stage 1.
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_append_quiet (e := [Ev.hookBefore s.att])
      -- The four old laws, and: the hooks' before_commit event is of stage 1, at most 2.
      hs.cC hs.cA hs.hC hs.hA (by simp [Ev.rank])
    -- The nineteen facts: seven are left to show; no outcome and not ended; the phase is not
    -- "validated".
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, by simp [hcur], hcC, hcA, hhC, hhA, ?_, ?_, ?_, ?_, ?_, ?_,
      fun ok h => by simp at h, ?_⟩
    -- The new event is of the current attempt. Take an event x of the new log.
    · intro x hx
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- The new one: its attempt is the current one.
      · simp at h; subst h; simp [Ev.att]
    -- Stage 1, the prepared phase's; older events of the attempt are 0.
    ·
      -- Take an event x of the current attempt.
      intro x hx hxa
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: its stage was at most the preparing phase's,
      · have := hs.rank_le x h hxa
        -- which is 0.
        simp only [hp, Phase.rank] at this
        -- The prepared phase's stage is 1.
        simp only [Phase.rank]
        -- 0 ≤ 1.
        omega
      -- The new one: stage 1, the phase's.
      · simp at h; subst h; simp [Ev.rank, Phase.rank]
    -- The order: by `ordered_append`, with the one new event as the chunk.
    · exact ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- The new event is of the current attempt.
        (fun y hy => by simp at hy; subst hy; rfl)
        -- Its stage, 1, is at least the old phase's, 0.
        (fun y hy => by simp at hy; subst hy; simp [Ev.rank, hp, Phase.rank])
        -- A single event is sorted.
        (List.pairwise_singleton _ _)
    -- The hooks' writes are of the current attempt. Take a recorded callback write p.
    · intro p hp'
      -- It is old or new.
      rcases List.mem_append.1 hp' with h | h
      -- Old: as before.
      · exact hs.bw_le p h
      -- New: it is (the current attempt, k) for a key k the hooks wrote.
      · simp only [List.mem_map] at h
        -- Name that key.
        obtain ⟨k, -, rfl⟩ := h
        -- Its attempt is the current one.
        exact Nat.le_refl _
    -- The hooks' writes join the write set. Take a key k recorded as a callback write of the
    -- current attempt.
    · intro k hk
      -- The record is old or new.
      rcases List.mem_append.1 hk with h | h
      -- Old: k was in the write set, and still is.
      · exact List.mem_append.2 (Or.inl (hs.bw_cur k h))
      -- New: k is one of the keys the hooks wrote.
      · simp only [List.mem_map, Prod.mk.injEq] at h
        -- Name it.
        obtain ⟨k', hk', -, rfl⟩ := h
        -- Those keys were added to the write set.
        exact List.mem_append.2 (Or.inr hk')
    -- No validation is added, and the current attempt has none yet. Take a callback write (a, k)
    -- and a validation of attempt a.
    · intro a k ws' hk hv
      -- The validation event is old, or it is the new event.
      rcases List.mem_append.1 hv with hv | hv
      -- Old. The callback write is old or new.
      · rcases List.mem_append.1 hk with h | h
        -- Both old: as before.
        · exact hs.bw_validated a k ws' h hv
        -- A new write: it is of the current attempt.
        · simp only [List.mem_map, Prod.mk.injEq] at h
          -- Replace a by the current attempt.
          obtain ⟨k', -, rfl, rfl⟩ := h
          -- Then the log held a validation of the current attempt, of a stage at most the phase's.
          have := hs.rank_le _ hv rfl
          -- But validation is stage 2 and the preparing phase is stage 0: impossible.
          simp [Ev.rank, hp, Phase.rank] at this
      -- The new event is not a validation: impossible.
      · simp at hv
    -- A committed attempt was validated. Take a committed attempt a.
    · intro a ha
      -- Its validation was in the old log.
      obtain ⟨ws', hws⟩ := hs.commit_validated a ha
      -- So it is in the longer log.
      exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: validation runs.
  | validate ok hp =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The four count laws survive: the new event is of stage 2.
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_append_quiet (e := [Ev.validate s.att s.writes])
      -- The four old laws, and: a validation event is of stage 2.
      hs.cC hs.cA hs.hC hs.hA (by simp [Ev.rank])
    -- The nineteen facts: six are left to show; no outcome and not ended; the rest is unchanged.
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, by simp [hcur], hcC, hcA, hhC, hhA, ?_, ?_, ?_, hs.bw_le, hs.bw_cur, ?_, ?_, ?_⟩
    -- The new event is of the current attempt. Take an event x of the new log.
    · intro x hx
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- The new one: its attempt is the current one.
      · simp at h; subst h; simp [Ev.att]
    -- Stage 2, the validated phase's; older events of the attempt are at most 1.
    ·
      -- Take an event x of the current attempt.
      intro x hx hxa
      -- It is old or the new one.
      rcases List.mem_append.1 hx with h | h
      -- Old: its stage was at most the prepared phase's,
      · have := hs.rank_le x h hxa
        -- which is 1.
        simp only [hp, Phase.rank] at this
        -- The validated phase's stage is 2.
        simp only [Phase.rank]
        -- 1 ≤ 2.
        omega
      -- The new one: stage 2, the phase's.
      · simp at h; subst h; simp [Ev.rank, Phase.rank]
    -- The order: by `ordered_append`, with the one new event as the chunk.
    · exact ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- The new event is of the current attempt.
        (fun y hy => by simp at hy; subst hy; rfl)
        -- Its stage, 2, is at least the old phase's, 1.
        (fun y hy => by simp at hy; subst hy; simp [Ev.rank, hp, Phase.rank])
        -- A single event is sorted.
        (List.pairwise_singleton _ _)
    -- The validation checks the whole write set, so every callback write of the attempt; an older
    -- attempt's validation is unchanged.
    ·
      -- Take a callback write (a, k) and a validation of attempt a with write set ws'.
      intro a k ws' hk hv
      -- The validation event is old, or it is the new one.
      rcases List.mem_append.1 hv with hv | hv
      -- Old: as before.
      · exact hs.bw_validated a k ws' hk hv
      -- The new one: it is the validation of the current attempt with the whole write set.
      · simp only [List.mem_singleton, Ev.validate.injEq] at hv
        -- Replace a and ws' accordingly.
        obtain ⟨rfl, rfl⟩ := hv
        -- The current attempt's callback writes are in its write set.
        exact hs.bw_cur k hk
    -- The validated phase has its validation logged. Take the verdict.
    · intro ok' h
      -- The validation just logged is the witness.
      exact ⟨s.writes, List.mem_append.2 (Or.inr (List.mem_singleton.2 rfl))⟩
    -- A committed attempt was validated. Take a committed attempt a.
    · intro a ha
      -- Its validation was in the old log.
      obtain ⟨ws', hws⟩ := hs.commit_validated a ha
      -- So it is in the longer log.
      exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: a validated attempt commits.
  | finishCommit hp =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The four count laws, by `counts_end` with the commit chunk as E and a commit as the outcome.
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_end (E := commitChunk s) (o := .commit)
      -- The new state.
      (t := { s with phase := .ended, cur := some .commit, log := s.log ++ commitChunk s })
      -- The invariant, no old outcome, five equalities that hold by definition, and: the chunk has
      -- no repeats.
      hs hcur rfl rfl rfl rfl rfl (commitChunk_nodup hs.commits_nodup)
      -- The chunk's on_commit events are the current callbacks'.
      (commitChunk_memC s)
      -- It holds no on_abort event.
      (fun a c o' => by rw [mem_commitChunk]; simp)
      -- It holds the hooks' on_commit of the current attempt.
      (fun a => by rw [mem_commitChunk]; simp)
      -- It holds no hook on_abort event.
      (fun a o' => by rw [mem_commitChunk]; simp)
    -- The nineteen facts: five are left to show; the attempt has an outcome and has ended; the
    -- phase is not "validated".
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, by simp, hcC, hcA, hhC, hhA, ?_, ?_, ?_, hs.bw_le, hs.bw_cur, ?_,
      fun ok h => by simp at h, ?_⟩
    -- The new events are of the current attempt. Take an event x of the new log.
    · intro x hx
      -- It is old or in the chunk.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- In the chunk: its attempt is the current one.
      · exact Nat.le_of_eq (commitChunk_att x h).1
    -- The stages: every event is of stage at most 4, the ended phase's.
    · intro x _ _; simp only [Phase.rank]; exact rank_le_four x
    -- The order: by `ordered_append`, with the commit chunk.
    · refine ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- The chunk is of the current attempt; its stages are at least the phase's, shown below;
        (fun y hy => (commitChunk_att y hy).1) (fun y hy => ?_)
        -- and it is sorted.
        (chunk_sorted (fun c => rfl) rfl)
      -- An event of the chunk is of stage 3 or more.
      have := (commitChunk_att y hy).2
      -- The validated phase's stage is 2.
      simp only [hp, Phase.rank]
      -- 2 ≤ 3.
      omega
    -- No validation is added.
    ·
      -- Take a callback write (a, k) and a validation of attempt a.
      intro a k ws' hk hv
      -- The validation event is old, or in the chunk.
      rcases List.mem_append.1 hv with hv | hv
      -- Old: as before.
      · exact hs.bw_validated a k ws' hk hv
      -- In the chunk: it would be of stage 3 or more,
      · have := (commitChunk_att _ hv).2
        -- but validation is stage 2: impossible.
        simp [Ev.rank] at this
    -- The current attempt was validated; older ones as before.
    ·
      -- Take a committed attempt a.
      intro a ha
      -- Split on whether a is the current attempt.
      by_cases h : a = s.att
      -- The current attempt: replace a by it.
      · subst h
        -- Its phase was "validated", so its validation is in the log.
        obtain ⟨ws', hws⟩ := hs.validated_logged true hp
        -- So it is in the longer log.
        exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
      -- Another attempt: its outcome was a commit before the step too.
      · have ha : outcomeOf s a = some .commit := by simp [outcomeOf, h] at ha ⊢
        -- Its validation was in the old log.
        obtain ⟨ws', hws⟩ := hs.commit_validated a ha
        -- So it is in the longer log.
        exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: the attempt ends without committing, with reason o.
  | finishAbort o ho =>
    -- The phase allows this abort: it has not ended, and `o` is no commit.
    have hne : s.phase ≠ .ended := by
      -- Split on which kind of abort the phase allows.
      rcases ho with ⟨h, -⟩ | ⟨h, -⟩
      -- A conflict after validation: the phase is "validated", not "ended".
      · simp [h]
      -- Open, preparing or prepared: not "ended".
      · rcases h with h | h | h <;> simp [h]
    -- The reason is not a commit:
    have hoc : o ≠ .commit := by
      -- Split on which kind of abort.
      rcases ho with ⟨-, h⟩ | ⟨-, h⟩
      -- A conflict is not a commit.
      · simp [h]
      -- A rollback, a drop, an error or close is not a commit.
      · rcases h with h | h | h | h <;> simp [h]
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none hne
    -- The phase's stage is at most 2:
    have hrank : s.phase.rank ≤ 2 := by
      -- Split on which kind of abort.
      rcases ho with ⟨h, -⟩ | ⟨h, -⟩
      -- Validated: stage 2.
      · simp [h, Phase.rank]
      -- Open, preparing or prepared: stage 0 or 1.
      · rcases h with h | h | h <;> simp [h, Phase.rank]
    -- The four count laws, by `counts_end` with the abort chunk as E and o as the outcome.
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_end (E := abortChunk s o) (o := o)
      -- The new state.
      (t := { s with phase := .ended, cur := some o, log := s.log ++ abortChunk s o })
      -- The invariant, no old outcome, five equalities that hold by definition, and: the chunk has
      -- no repeats.
      hs hcur rfl rfl rfl rfl rfl (abortChunk_nodup hs.aborts_nodup)
      -- It holds no on_commit event.
      (fun a c => by rw [mem_abortChunk]; simp [hoc])
      -- Its on_abort events are the current callbacks', with reason o.
      (abortChunk_memA s hoc)
      -- It holds no hook on_commit event.
      (fun a => by rw [mem_abortChunk]; simp [hoc])
      -- It holds the hooks' on_abort of the current attempt, with reason o.
      (abortChunk_memHA s hoc)
    -- The nineteen facts: five are left to show; the attempt has an outcome and has ended; the
    -- phase is not "validated".
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, by simp, hcC, hcA, hhC, hhA, ?_, ?_, ?_, hs.bw_le, hs.bw_cur, ?_,
      fun ok h => by simp at h, ?_⟩
    -- The new events are of the current attempt. Take an event x of the new log.
    · intro x hx
      -- It is old or in the chunk.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- In the chunk: its attempt is the current one.
      · exact Nat.le_of_eq (abortChunk_att x h).1
    -- The stages: every event is of stage at most 4, the ended phase's.
    · intro x _ _; simp only [Phase.rank]; exact rank_le_four x
    -- The order: by `ordered_append`, with the abort chunk.
    · refine ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- The chunk is of the current attempt; its stages are at least the phase's, shown below;
        (fun y hy => (abortChunk_att y hy).1) (fun y hy => ?_)
        -- and it is sorted.
        (chunk_sorted (fun c => rfl) rfl)
      -- An event of the chunk is of stage 3 or more,
      have := (abortChunk_att y hy).2
      -- and the phase's stage is at most 2.
      omega
    -- No validation is added. Take a callback write (a, k) and a validation of attempt a.
    · intro a k ws' hk hv
      -- The validation event is old, or in the chunk.
      rcases List.mem_append.1 hv with hv | hv
      -- Old: as before.
      · exact hs.bw_validated a k ws' hk hv
      -- In the chunk: it would be of stage 3 or more,
      · have := (abortChunk_att _ hv).2
        -- but validation is stage 2: impossible.
        simp [Ev.rank] at this
    -- The current attempt aborted; older ones as before.
    ·
      -- Take a committed attempt a.
      intro a ha
      -- Split on whether a is the current attempt.
      by_cases h : a = s.att
      -- The current attempt: replace a by it.
      · subst h
        -- Its outcome is `cur`, which is now o.
        rw [outcomeOf_cur] at ha
        -- So o would be a commit.
        simp at ha
        -- It is not.
        exact absurd ha hoc
      -- Another attempt: its outcome was a commit before the step too.
      · have ha : outcomeOf s a = some .commit := by simp [outcomeOf, h] at ha ⊢
        -- Its validation was in the old log.
        obtain ⟨ws', hws⟩ := hs.commit_validated a ha
        -- So it is in the longer log.
        exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: the oldest pending before_commit callback panics.
  | panicBefore c rest hp hb =>
    -- The attempt has not ended, so it has no outcome yet.
    have hcur : s.cur = none := hs.cur_none (by simp [hp])
    -- The panicking callback's event, then the abort chunk.
    have hnd : (Ev.before s.att c :: abortChunk s .panic).Nodup := by
      -- "No repeats" with an entry in front: the entry is not in the rest, and the rest has none.
      rw [List.nodup_cons]
      -- The abort chunk has none; "the callback's event is not in the chunk" is left.
      refine ⟨fun h => ?_, abortChunk_nodup hs.aborts_nodup⟩
      -- If it were, it would be of stage 3 or more,
      have := (abortChunk_att _ h).2
      -- but a before_commit event is stage 0: impossible.
      simp [Ev.rank] at this
    -- The four count laws, by `counts_end` with the callback's event and the abort chunk as E,
    obtain ⟨hcC, hcA, hhC, hhA⟩ := counts_end (E := Ev.before s.att c :: abortChunk s .panic)
      -- and `panic` as the outcome.
      (o := .panic)
      -- The new state.
      (t := { s with phase := .ended, cur := some .panic, befores := rest,
                     log := s.log ++ (Ev.before s.att c :: abortChunk s .panic) })
      -- The invariant, no old outcome, five equalities that hold by definition, and: E has no
      -- repeats.
      hs hcur rfl rfl rfl rfl rfl hnd
      -- E holds no on_commit event.
      (fun a c => by rw [List.mem_cons, mem_abortChunk]; simp)
      -- Its on_abort events are the current callbacks', with reason `panic`.
      (fun a c o' => by
        rw [List.mem_cons, abortChunk_memA s (by simp) a c o']; simp)
      -- E holds no hook on_commit event.
      (fun a => by rw [List.mem_cons, mem_abortChunk]; simp)
      -- It holds the hooks' on_abort of the current attempt, with reason `panic`.
      (fun a o' => by
        rw [List.mem_cons, abortChunk_memHA s (by simp) a o']; simp)
    -- The nineteen facts: five are left to show; the attempt has an outcome and has ended; the
    -- phase is not "validated".
    refine ⟨hs.commits_order, hs.aborts_order, hs.commits_nodup, hs.aborts_nodup, hs.regC_le,
      hs.regA_le, by simp, hcC, hcA, hhC, hhA, ?_, ?_, ?_, hs.bw_le, hs.bw_cur, ?_,
      fun ok h => by simp at h, ?_⟩
    -- The new events are of the current attempt. Take an event x of the new log.
    · intro x hx
      -- It is old or new.
      rcases List.mem_append.1 hx with h | h
      -- Old: as before.
      · exact hs.att_le x h
      -- New: the callback's event, or in the abort chunk.
      · rcases List.mem_cons.1 h with rfl | h
        -- The callback's event: of the current attempt.
        · simp [Ev.att]
        -- In the chunk: of the current attempt.
        · exact Nat.le_of_eq (abortChunk_att x h).1
    -- The stages: every event is of stage at most 4, the ended phase's.
    · intro x _ _; simp only [Phase.rank]; exact rank_le_four x
    -- The order: by `ordered_append`; three parts are shown below
    · refine ordered_append hs.ordered (fun x hx hxa => hs.rank_le x hx hxa)
        -- (the new events' attempt, their stages, and that they are sorted).
        (fun y hy => ?_) (fun y hy => ?_) ?_
      -- Their attempt. A new event is the callback's event or in the chunk.
      · rcases List.mem_cons.1 hy with rfl | h
        -- The callback's event: of the current attempt, by definition.
        · rfl
        -- In the chunk: of the current attempt.
        · exact (abortChunk_att y h).1
      -- Their stages: the preparing phase's stage is 0, and every stage is at least 0.
      · simp only [hp, Phase.rank]; exact Nat.zero_le _
      -- The callback's event (stage 0) leads, then the sorted chunk.
      ·
        -- Sorted with an entry in front: the entry is at most every later one, and the rest is
        -- sorted.
        refine List.Pairwise.cons (fun y hy => ?_) (chunk_sorted (fun c => rfl) rfl)
        -- The callback's event is stage 0, at most anything.
        simp only [Ev.rank]; exact Nat.zero_le _
    -- No validation is added. Take a callback write (a, k) and a validation of attempt a.
    · intro a k ws' hk hv
      -- The validation event is old or new.
      rcases List.mem_append.1 hv with hv | hv
      -- Old: as before.
      · exact hs.bw_validated a k ws' hk hv
      -- New: the callback's event, or in the abort chunk.
      · rcases List.mem_cons.1 hv with h | h
        -- The callback's event is not a validation: impossible.
        · simp at h
        -- In the chunk: it would be of stage 3 or more,
        · have := (abortChunk_att _ h).2
          -- but validation is stage 2: impossible.
          simp [Ev.rank] at this
    -- A committed attempt was validated. Take a committed attempt a.
    · intro a ha
      -- Split on whether a is the current attempt.
      by_cases h : a = s.att
      -- The current attempt: replace a by it.
      · subst h
        -- Its outcome is `panic`, not a commit: impossible.
        simp [outcomeOf] at ha
      -- Another attempt: its outcome was a commit before the step too.
      · have ha : outcomeOf s a = some .commit := by simp [outcomeOf, h] at ha ⊢
        -- Its validation was in the old log.
        obtain ⟨ws', hws⟩ := hs.commit_validated a ha
        -- So it is in the longer log.
        exact ⟨ws', List.mem_append.2 (Or.inl hws)⟩
  -- Case: `transact` re-runs a conflicted attempt.
  | retry hp hc =>
    -- The next attempt starts with nothing registered; every outcome is as
    -- before, since the ended attempt's was a conflict.
    have hout : ∀ a, outcomeOf (retried s) a = outcomeOf s a := by
      -- Take an attempt a.
      intro a
      -- Unfold both definitions.
      unfold outcomeOf retried
      -- Simplify the fields.
      simp only
      -- Split on whether a is before the ended attempt.
      by_cases h1 : a < s.att
      -- An earlier attempt: a conflict either way.
      ·
        -- Both sides compute to a conflict (a is below the new attempt number too).
        simp [h1, show a < s.att + 1 by omega]
      -- Not earlier. Split on whether a is the ended attempt.
      · by_cases h2 : a = s.att
        -- The attempt that just conflicted.
        ·
          -- Before: `cur`, a conflict. After: an earlier attempt, so a conflict.
          subst h2; simp [hc]
        -- A later attempt: no outcome either way.
        ·
          -- a is not below the new attempt number.
          have h3 : ¬ a < s.att + 1 := by omega
          -- Both sides compute to "none".
          simp [h1, h2, h3]
    -- State it for the record the step produces, as the goals show it.
    simp only [retried] at hout
    -- The nineteen facts: the two empty lists have no repeats; no outcome and not ended; the order
    -- and the validated writes are unchanged; the phase is not "validated"; the rest is shown
    -- below.
    refine ⟨?_, ?_, List.nodup_nil, List.nodup_nil, ?_, ?_, by simp, ?_, ?_, ?_, ?_, ?_, ?_,
      hs.ordered, ?_, ?_, hs.bw_validated, fun ok h => by simp at h, ?_⟩
    -- No registration is of the new attempt.
    ·
      -- Simplify the fields.
      simp only
      -- Filtering for the new attempt leaves nothing, provided no registration is of it.
      rw [List.filter_eq_nil_iff.2]
      -- Then both sides are the empty list.
      · rfl
      -- The proviso: take a registration p.
      · intro p hp'
        -- Its attempt is at most the old attempt number,
        have := hs.regC_le p hp'
        -- ("is of the new attempt", as an equation)
        simp only [beq_iff_eq]
        -- so it is not the new one.
        omega
    -- The same for the on_abort registrations. Simplify the fields.
    · simp only
      -- Filtering for the new attempt leaves nothing, provided no registration is of it.
      rw [List.filter_eq_nil_iff.2]
      -- Then both sides are the empty list.
      · rfl
      -- The proviso: take a registration p.
      · intro p hp'
        -- Its attempt is at most the old attempt number,
        have := hs.regA_le p hp'
        -- ("is of the new attempt", as an equation)
        simp only [beq_iff_eq]
        -- so it is not the new one.
        omega
    -- Every on_commit registration's attempt is at most the old number, so at most the new one.
    · intro p hp'; have := hs.regC_le p hp'; simp only; omega
    -- The same for the on_abort registrations.
    · intro p hp'; have := hs.regA_le p hp'; simp only; omega
    -- The on_commit count: the log and the registrations are unchanged, and so is every outcome.
    · intro a c; rw [hs.cC, hout]
    -- The on_abort count: the same.
    · intro a c o; rw [hs.cA, hout]
    -- The hooks' on_commit count: the same.
    · intro a; rw [hs.hC, hout]
    -- The hooks' on_abort count: the same.
    · intro a o; rw [hs.hA, hout]
    -- Every event's attempt is at most the old number, so at most the new one.
    · intro x hx; have := hs.att_le x hx; simp only; omega
    -- No event is of the new attempt.
    ·
      -- Take an event x said to be of the new attempt.
      intro x hx hxa
      -- Its attempt is at most the old number.
      have := hs.att_le x hx
      -- Simplify "of the new attempt".
      simp only at hxa
      -- Both cannot hold.
      omega
    -- Every callback write's attempt is at most the old number, so at most the new one.
    · intro p hp'; have := hs.bw_le p hp'; simp only; omega
    -- No callback write is of the new attempt.
    ·
      -- Take a key k said to be a callback write of the new attempt.
      intro k hk
      -- Simplify the fields.
      simp only at hk
      -- Its attempt is at most the old number.
      have := hs.bw_le _ hk
      -- Simplify the fields.
      simp only at this
      -- Both cannot hold.
      omega
    -- A committed attempt was validated. Take a committed attempt a.
    · intro a ha
      -- Its outcome was a commit before the step too.
      rw [hout] at ha
      -- So its validation is in the log, which did not change.
      exact hs.commit_validated a ha

/-- Several steps keep the invariant. -/
theorem steps_inv {s t : T}
    -- The invariant holds at the start.
    (hs : Inv s)
    -- Any number of steps.
    (hsteps : Steps s t) :
    -- The statement: the invariant holds at the end.
    Inv t := by
  -- Go through the steps one at a time.
  induction hsteps with
  -- No step: nothing changed.
  | refl => exact hs
  -- The steps so far, then one more.
  | tail _ hstep ih => exact step_inv ih hstep

/-- **Every reachable state satisfies the invariant.** -/
theorem reachable_inv {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) : Inv s :=
  -- The start satisfies the invariant, and every step keeps it.
  steps_inv init_inv h

/-! ## The laws -/

/-- **Exactly once.** For attempt `a` and callback `c`: on_commit callback
`c` ran once if it was registered in attempt `a` and `a` committed, and
never otherwise; on_abort callback `c` ran once, with reason `o`, if it was
registered in `a` and `a` ended aborted with `o`, and never otherwise; and
the hooks' outcome ran once for each ended attempt, of the matching kind.
An attempt still running has no outcome, so none of these ran for it.
TLA+: `AtMostOnce`, `ExactlyOnce`. -/
theorem exactly_once {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) (a c : Nat) :
    -- The statement, four laws. The on_commit callback's count;
    (s.log.count (.onCommit a c) =
      if (a, c) ∈ s.regC ∧ outcomeOf s a = some .commit then 1 else 0) ∧
    -- the on_abort callback's count, for every reason;
    (∀ o, s.log.count (.onAbort a c o) =
      if (a, c) ∈ s.regA ∧ outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0) ∧
    -- the hooks' on_commit count;
    (s.log.count (.hookCommit a) = if outcomeOf s a = some .commit then 1 else 0) ∧
    -- the hooks' on_abort count, for every reason.
    (∀ o, s.log.count (.hookAbort a o) = if outcomeOf s a = some o ∧ o ≠ .commit then 1 else 0) :=
  -- The invariant holds in s.
  let hs := reachable_inv h
  -- The four laws are four of its fields.
  ⟨hs.cC a c, hs.cA a c, hs.hC a, hs.hA a⟩

/-- **Never both.** No attempt runs an on_commit callback and an on_abort
callback: once one of its on_commit callbacks ran, none of its on_abort
callbacks has run or will (the outcome is fixed). -/
theorem never_both {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {a c c' : Nat} {o : Outcome}
    -- An on_commit callback of attempt `a` ran.
    (hc : Ev.onCommit a c ∈ s.log) :
    -- The statement: no on_abort callback of attempt a ran.
    Ev.onAbort a c' o ∉ s.log := by
  -- The invariant holds in s.
  have hs := reachable_inv h
  -- Suppose one did.
  intro ha
  -- Each ran, so each one's condition holds: the outcome is a commit and
  -- is an abort.
  have h1 := List.count_pos_iff.2 hc
  -- The on_abort event's count is positive too.
  have h2 := List.count_pos_iff.2 ha
  -- By the invariant, the on_commit count is 1 or 0 according to its condition.
  rw [hs.cC] at h1
  -- The same for the on_abort count.
  rw [hs.cA] at h2
  -- Split on the on_commit condition.
  split at h1
  -- It holds. Split on the on_abort condition.
  · split at h2
    -- Both hold: name them.
    · rename_i hx hy
      -- The outcome is a commit, so the on_abort condition says its reason is a commit
      rw [hx.2] at hy
      -- (simplify),
      simp at hy
      -- and also that it is not: a contradiction.
      exact hy.2.2 hy.2.1.symm
    -- The on_abort condition fails: its count would be 0, yet it is positive.
    · omega
  -- The on_commit condition fails: its count would be 0, yet it is positive.
  · omega

/-- **Order.** Within an attempt, an event of an earlier stage comes before
every event of a later stage: before_commit callbacks, then the hooks'
before_commit, then validation, then the transaction's outcome callbacks,
then the hooks' outcome. TLA+: `CallbacksBeforeHooks`. -/
theorem order {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {x y : Ev}
    -- Both ran.
    (hx : x ∈ s.log) (hy : y ∈ s.log)
    -- In the same attempt.
    (hatt : x.att = y.att)
    -- `x` is of an earlier stage.
    (hr : x.rank < y.rank) :
    -- The statement: [x, y] appears in the log in this order.
    [x, y].Sublist s.log :=
  -- This is the `ordered` field of the invariant.
  (reachable_inv h).ordered x hx y hy hatt hr

/-- **The transaction's callbacks before the hooks'.** When an on_commit
callback of attempt `a` ran, the hooks' on_commit ran too, after it. -/
theorem callbacks_before_hooks {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {a c : Nat}
    -- An on_commit callback of attempt `a` ran.
    (hc : Ev.onCommit a c ∈ s.log) :
    -- The statement: the hooks' on_commit ran, and after the callback.
    Ev.hookCommit a ∈ s.log ∧ [Ev.onCommit a c, Ev.hookCommit a].Sublist s.log := by
  -- The invariant holds in s.
  have hs := reachable_inv h
  -- The callback ran, so `a` committed, so the hook ran once.
  have h1 := List.count_pos_iff.2 hc
  -- By the invariant, that count is 1 or 0 according to its condition.
  rw [hs.cC] at h1
  -- The attempt committed:
  have hcommit : outcomeOf s a = some .commit := by
    -- Split on the condition.
    split at h1
    -- It holds: its second half is "a committed".
    · rename_i hx; exact hx.2
    -- It fails: the count would be 0, yet it is positive.
    · omega
  -- The hooks' on_commit ran:
  have hhook : Ev.hookCommit a ∈ s.log := by
    -- It is enough that its count is positive.
    apply List.count_pos_iff.1
    -- By the invariant its count is 1 for a committed attempt.
    rw [hs.hC]
    -- And a committed.
    simp [hcommit]
  -- Both ran in the same attempt, stage 3 and stage 4: the `ordered` field puts the callback first.
  exact ⟨hhook, hs.ordered _ hc _ hhook rfl (by simp [Ev.rank])⟩

/-- **A commit was validated first.** When an on_commit callback of
attempt `a` ran, attempt `a` was validated, before it. -/
theorem commit_was_validated {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {a c : Nat}
    -- An on_commit callback of attempt `a` ran.
    (hc : Ev.onCommit a c ∈ s.log) :
    -- The statement: some validation of attempt a is in the log, before the callback.
    ∃ ws, Ev.validate a ws ∈ s.log ∧ [Ev.validate a ws, Ev.onCommit a c].Sublist s.log := by
  -- The invariant holds in s.
  have hs := reachable_inv h
  -- The callback ran, so its count is positive.
  have h1 := List.count_pos_iff.2 hc
  -- By the invariant, that count is 1 or 0 according to its condition.
  rw [hs.cC] at h1
  -- The attempt committed:
  have hcommit : outcomeOf s a = some .commit := by
    -- Split on the condition.
    split at h1
    -- It holds: its second half is "a committed".
    · rename_i hx; exact hx.2
    -- It fails: the count would be 0, yet it is positive.
    · omega
  -- A committed attempt has its validation in the log.
  obtain ⟨ws, hws⟩ := hs.commit_validated a hcommit
  -- Same attempt, stage 2 and stage 3: the `ordered` field puts the validation first.
  exact ⟨ws, hws, hs.ordered _ hws _ hc rfl (by simp [Ev.rank])⟩

/-- **Outcome callbacks in registration order.** In any state the
invariant holds in, a commit runs the attempt's on_commit callbacks in the
order they were registered, then the hooks' on_commit. -/
theorem finish_commit_runs {s : T}
    -- The invariant holds.
    (hs : Inv s) :
    -- The statement: the commit's events are the attempt's registrations in order, as on_commit
    -- events, then the hooks' on_commit.
    commitChunk s =
      ((s.regC.filter (fun p => p.1 == s.att)).map Prod.snd).map (Ev.onCommit s.att) ++
        [Ev.hookCommit s.att] := by
  -- Unfold the chunk: it maps the current on_commit list.
  unfold commitChunk
  -- By the invariant that list is the attempt's registrations, in order.
  rw [hs.commits_order]

/-- **Outcome callbacks in registration order.** An abort runs the
attempt's on_abort callbacks in the order they were registered, then the
hooks' on_abort. -/
theorem finish_abort_runs {s : T}
    -- The invariant holds.
    (hs : Inv s) (o : Outcome) :
    -- The statement: the abort's events are the attempt's registrations in order, as on_abort
    -- events with the reason, then the hooks' on_abort.
    abortChunk s o =
      ((s.regA.filter (fun p => p.1 == s.att)).map Prod.snd).map (fun c => Ev.onAbort s.att c o) ++
        [Ev.hookAbort s.att o] := by
  -- Unfold the chunk: it maps the current on_abort list.
  unfold abortChunk
  -- By the invariant that list is the attempt's registrations, in order.
  rw [hs.aborts_order]

/-- **Before_commit writes are validated.** Every key a before_commit
callback (or the hooks' before_commit) of attempt `a` wrote is in the write
set attempt `a` validated. TLA+: `NoLostUpdate`. -/
theorem before_writes_validated {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {a k : Nat} {ws : List Nat}
    -- A before_commit callback of attempt `a` wrote `k`.
    (hk : (a, k) ∈ s.bw)
    -- Attempt `a` was validated with write set `ws`.
    (hv : Ev.validate a ws ∈ s.log) :
    -- The statement: k is in the validated write set.
    k ∈ ws :=
  -- This is the `bw_validated` field of the invariant.
  (reachable_inv h).bw_validated a k ws hk hv

/-- **Attempts are isolated.** An on_commit or on_abort callback that ran
for attempt `a` was registered in attempt `a`, and reports `a`'s own
outcome: a re-run never runs a previous attempt's callbacks. TLA+:
`AttemptIsolation`. -/
theorem attempts_isolated {s : T}
    -- `s` is reachable from the start.
    (h : Steps init s) {a c : Nat} :
    -- The statement. An on_commit callback that ran was registered in attempt a, and a committed;
    (Ev.onCommit a c ∈ s.log → (a, c) ∈ s.regC ∧ outcomeOf s a = some .commit) ∧
    -- an on_abort callback that ran with reason o was registered in attempt a, and a ended with o.
    (∀ o, Ev.onAbort a c o ∈ s.log → (a, c) ∈ s.regA ∧ outcomeOf s a = some o) := by
  -- The invariant holds in s.
  have hs := reachable_inv h
  -- Two parts.
  refine ⟨fun hc => ?_, fun o ha => ?_⟩
  -- It ran, so its condition holds.
  ·
    -- The callback ran, so its count is positive.
    have h1 := List.count_pos_iff.2 hc
    -- By the invariant, that count is 1 or 0 according to its condition.
    rw [hs.cC] at h1
    -- Split on the condition.
    split at h1
    -- It holds: it is what is asked.
    · rename_i hx; exact hx
    -- It fails: the count would be 0, yet it is positive.
    · omega
  -- The on_abort part. It ran, so its count is positive.
  · have h1 := List.count_pos_iff.2 ha
    -- By the invariant, that count is 1 or 0 according to its condition.
    rw [hs.cA] at h1
    -- Split on the condition.
    split at h1
    -- It holds: its first two parts are what is asked.
    · rename_i hx; exact ⟨hx.1, hx.2.1⟩
    -- It fails: the count would be 0, yet it is positive.
    · omega

/-- **A panic in before_commit aborts.** If the next before_commit callback
panics, the attempt ends with reason `panic`: every on_abort callback of
the attempt has run with it, and no on_commit callback. TLA+: `PanicAborts`. -/
theorem panic_aborts {s : T}
    -- The invariant holds.
    (hs : Inv s) {c : Nat} {rest : List Nat}
    -- The before_commit pass is running.
    (hp : s.phase = .preparing)
    -- `c` is the next before_commit callback, and it panics.
    (hb : s.befores = c :: rest) :
    -- t is the state after the panic step: ended with the reason `panic`, c no longer pending, and
    -- the log longer by c's run and the abort's events.
    let t : T := { s with phase := .ended, cur := some .panic, befores := rest,
                          log := s.log ++ (Ev.before s.att c :: abortChunk s .panic) }
    -- The statement: that is a step of the design; the attempt's outcome is `panic`;
    Step s t ∧ outcomeOf t s.att = some .panic ∧
      -- every on_abort callback of the attempt ran with `panic`;
      (∀ c' ∈ s.aborts, Ev.onAbort s.att c' .panic ∈ t.log) ∧
      -- and no on_commit callback of it ran.
      (∀ c', Ev.onCommit s.att c' ∉ t.log) := by
  -- Name the new state t.
  intro t
  -- It is the panic step.
  have hstep : Step s t := Step.panicBefore s c rest hp hb
  -- So the invariant holds in t.
  have ht := step_inv hs hstep
  -- The current attempt's outcome in t is `panic`, by computation.
  have hout : outcomeOf t s.att = some .panic := by simp [outcomeOf, t]
  -- The step and the outcome are done; the two claims about callbacks are left.
  refine ⟨hstep, hout, fun c' hc' => ?_, fun c' hm => ?_⟩
  -- Registered in this attempt, which aborted with `panic`: it ran once.
  ·
    -- It is enough that its count is positive.
    apply List.count_pos_iff.1
    -- Its condition holds: registered in this attempt, the outcome is `panic`, and `panic` is not a
    -- commit.
    have hcond : (s.att, c') ∈ t.regA ∧ outcomeOf t s.att = some .panic ∧ Outcome.panic ≠ .commit :=
      -- The three parts of the condition.
      ⟨(hs.mem_aborts c').1 hc', hout, by simp⟩
    -- By the invariant in t, its count is 1 or 0 according to that condition.
    rw [ht.cA]
    -- The condition holds, so the count is 1.
    simp [hcond]
  -- The attempt did not commit, so no on_commit callback ran.
  ·
    -- Suppose one ran: its count is positive.
    have h1 := List.count_pos_iff.2 hm
    -- By the invariant its count is 1 only if the outcome is a commit; the outcome is `panic`.
    rw [ht.cC, hout] at h1
    -- So the count is 0: a contradiction.
    simp at h1

/-! ## The RED case: callbacks that survive a conflict -/

/-- The defective design: the re-run keeps the conflicted attempt's
on_commit and on_abort callbacks registered on the transaction. -/
inductive StepKeep : T → T → Prop
  /-- Every step of the design. -/
  | step {s t : T}
      -- A step of the design.
      (h : Step s t) : StepKeep s t
  /-- The defect: a re-run that clears the writes but not the callbacks. -/
  | retryKeep (s : T)
      -- The attempt ended ...
      (hp : s.phase = .ended)
      -- ... with a conflict.
      (hc : s.cur = some .conflict) :
      -- After the step: the next attempt, open, with no outcome, no pending callback
      StepKeep s { s with att := s.att + 1, phase := .open_, cur := none, befores := [],
                          -- and no write. The on_commit and on_abort lists are kept: that is the
                          -- bug.
                          writes := [] }

/-- Any number of `StepKeep` steps. -/
inductive StepsKeep : T → T → Prop
  /-- No step at all. -/
  | refl (s : T) : StepsKeep s s
  /-- Some steps, then one more. -/
  | tail {s t u : T} : StepsKeep s t → StepKeep t u → StepsKeep s u

/-- **`surviving_callbacks_break_isolation`.** Attempt 0 registers
on_commit callback 5, is validated with a conflict and aborts. The re-run,
attempt 1, keeps callback 5, is validated cleanly and commits: callback 5
runs for attempt 1's commit although it was registered in attempt 0, whose
outcome was the conflict. `attempts_isolated` fails. -/
theorem surviving_callbacks_break_isolation :
    -- The statement: there is a run of the defective design after which on_commit callback 5 ran
    -- for attempt 1, although attempt 1 never registered it,
    ∃ t, StepsKeep init t ∧ Ev.onCommit 1 5 ∈ t.log ∧ (1, 5) ∉ t.regC ∧
      -- and attempt 0 ended with a conflict.
      Ev.hookAbort 0 .conflict ∈ t.log := by
  -- The run, step by step (ten steps from the start), then three facts checked by computation.
  refine ⟨_, .tail (.tail (.tail (.tail (.tail (.tail (.tail (.tail (.tail (.tail (.refl init)
      -- Attempt 0 registers on_commit callback 5 and commits.
      (.step (.regCommit _ 5 (Or.inl rfl) (by decide))))
      (.step (.commit _ rfl)))
      (.step (.hookBefore _ [] rfl rfl)))
      -- Validation finds a conflict; attempt 0 aborts.
      (.step (.validate _ false rfl)))
      (.step (.finishAbort _ .conflict (Or.inl ⟨rfl, rfl⟩))))
      -- The defective re-run keeps callback 5.
      (.retryKeep _ rfl rfl))
      -- Attempt 1 commits cleanly.
      (.step (.commit _ rfl)))
      (.step (.hookBefore _ [] rfl rfl)))
      (.step (.validate _ true rfl)))
      (.step (.finishCommit _ rfl)), by decide, by decide, by decide⟩

/-! ## Delivery on the committing thread (D53)

Above, an attempt's outcome runs its callbacks once. Here is who runs them
and how often, in the two forms a commit takes in the code.

`transact` calls the blocking `Transaction::commit`
(`src/transaction.rs`): the call waits for its commit to be decided
and, at Immediate durability, synced, and then runs the outcome's callbacks
itself, on the calling thread. One thread, one outcome, nothing to race:
that is the sequential machine above.

`transact_async` calls `Transaction::commit_nowait`
(`src/transaction/nowait.rs`). A commit that is owed no fsync is done
when the call returns, and is told by the calling thread in the same way.
A commit that is owed its fsync returns a `CommitTicket` instead. The
ticket's `Completion` (`src/io_queue/completion.rs`) starts *pending* and
holds a closure that reads the outcome from the commit's slot. Once the
commit has landed (its fsync is done), a deliverer on the thread that holds
the commit's queue takes that closure, which only one can get, and decides
the outcome with it. Then the outcome is *delivered*: one compare-and-swap
from "decided" to "delivering" lets one deliverer in, and only that one
runs the callbacks. Every deliverer runs on the thread that holds the
commit's queue at that moment: the queue's poll, the queue's drop (the
queue may have moved to another thread, which then holds it), or the
commit's own fallback when its queue went away before the ticket was
registered. A thread that ran the fsync is never a deliverer for that, and
never decides a ticket.

Tiny example. Thread A commits with `commit_nowait`; thread B's poll lands
A's commit. B only puts a note in A's inbox. A's next poll takes the closure,
reads the outcome from the commit's slot, claims it and runs A's `on_commit` on A.
If A drops its queue at the same moment, the drop and the poll race for the
closure and for the claim; one wins each, and `on_commit` runs once. -/

/-- Who can decide or run an outcome: the thread holding the commit's
queue, or (only in the RED case) a committer. -/
inductive Runner where
  /-- The thread `t` that holds the commit's queue. -/
  | holder (t : Nat)
  /-- A committer `c` that decided or synced the commit. -/
  | committer (c : Nat)
  -- Two runners can be compared for equality.
  deriving DecidableEq

/-- Where a ticket's outcome is (`Completion`'s state word). -/
inductive DState where
  /-- Not decided yet. -/
  | pending
  /-- Decided, read from the landed commit's slot; not delivered. -/
  | decided
  /-- One deliverer won the claim and is running the callbacks. -/
  | delivering
  /-- Delivered: the ticket is ready. -/
  | ready
  -- Two states can be compared for equality.
  deriving DecidableEq

/-- The ticket of one commit that was owed its fsync. -/
structure Ticket where
  /-- The thread holding the commit's queue now. -/
  holder : Nat
  /-- The outcome's state. -/
  st : DState
  /-- Ghost: every run of the outcome callbacks, by who ran it. -/
  ran : List Runner
  /-- The commit has landed: its fsync is done. -/
  landed : Bool
  /-- The ticket still holds the closure that reads the outcome from the
  commit's slot (`Completion::resolve`). -/
  resolver : Bool
  /-- Ghost: every decision of the outcome, by who made it. -/
  decider : List Runner

/-- A fresh ticket, its queue on thread `t`: pending, its commit not landed,
its closure in place, nothing decided and nothing run. -/
def fresh (t : Nat) : Ticket := ⟨t, .pending, [], false, true, []⟩

/-- The steps of one ticket, one atomic step each. -/
inductive DStep : Ticket → Ticket → Prop
  /-- The commit lands: some thread finished its fsync. That thread
  touches nothing of the ticket. -/
  | land (k : Ticket) :
      -- The state after the step:
      DStep k { k with landed := true }
  /-- The queue moves to thread `t`, which holds it from now on (the queue is
  `Send`). -/
  | move (k : Ticket) (t : Nat) :
      -- The state after the step:
      DStep k { k with holder := t }
  /-- A deliverer on the holding thread takes the closure and decides the
  outcome with it (`Completion::deliver`: `resolve.take`, then `decide`). -/
  | resolve (k : Ticket)
      -- The commit has landed: only then is the queue told, and a delivery begins.
      (hl : k.landed = true)
      -- The closure is still there: nobody took it.
      (hr : k.resolver = true)
      -- The outcome is not decided.
      (hp : k.st = .pending) :
      -- The state after the step: the closure is gone, the outcome is decided,
      DStep k { k with resolver := false, st := .decided,
                       -- and the holder's thread made the decision.
                       decider := .holder k.holder :: k.decider }
  /-- A deliverer on the holding thread (the poll, the drop, or the commit's
  own fallback) claims the decided outcome with one CAS and runs the
  callbacks there. -/
  | deliver (k : Ticket)
      -- The CAS succeeds only on a decided outcome.
      (h : k.st = .decided) :
      -- The state after the step:
      DStep k { k with st := .delivering, ran := .holder k.holder :: k.ran }
  /-- The deliverer finishes: the ticket is ready. -/
  | finish (k : Ticket)
      -- It is the winner, delivering.
      (h : k.st = .delivering) :
      -- The state after the step:
      DStep k { k with st := .ready }

/-- Any number of ticket steps, one after another. -/
inductive DSteps : Ticket → Ticket → Prop
  /-- No step at all. -/
  | refl (k : Ticket) : DSteps k k
  /-- Some steps, then one more. -/
  | tail {a b c : Ticket} : DSteps a b → DStep b c → DSteps a c

/-- What holds of a ticket in every reachable state. -/
structure DInv (k : Ticket) : Prop where
  /-- Before the claim nobody ran the callbacks. -/
  before : k.st = .pending ∨ k.st = .decided → k.ran = []
  /-- After it, exactly one run. -/
  after : k.st = .delivering ∨ k.st = .ready → k.ran.length = 1
  /-- Every run was by a thread holding the queue. -/
  holder : ∀ r ∈ k.ran, ∃ t, r = .holder t
  /-- An outcome that is decided was read from a commit that had landed. -/
  landed_first : k.st ≠ .pending → k.landed = true
  /-- While the closure is in the ticket the outcome is pending and nobody
  has decided it. -/
  unresolved : k.resolver = true → k.st = .pending ∧ k.decider = []
  /-- Once the closure is gone, exactly one decision was made. -/
  resolved : k.resolver = false → k.decider.length = 1
  /-- Every decision was by a thread holding the queue. -/
  decider_holder : ∀ r ∈ k.decider, ∃ t, r = .holder t

/-- A fresh ticket satisfies the invariant. -/
theorem dinv_init (t : Nat) : DInv (fresh t) := by
  -- Seven facts about a pending ticket with its closure in place and two empty lists: each
  -- one computes.
  constructor <;> simp [fresh]

/-- **One step keeps the invariant.** -/
theorem dstep_inv {k k' : Ticket}
    -- The invariant holds before the step.
    (hi : DInv k)
    -- One step.
    (h : DStep k k') :
    -- Then:
    DInv k' := by
  -- Look at the step.
  cases h with
  -- Landing changes only "landed", to true: every fact still holds.
  | land =>
    -- Six facts are untouched; "decided means landed" holds because the commit has landed now.
    exact ⟨hi.before, hi.after, hi.holder, fun _ => rfl, hi.unresolved, hi.resolved,
      hi.decider_holder⟩
  -- Moving the queue changes only who holds it.
  | move t =>
    -- The state, the runs, the closure and the decisions are untouched.
    exact ⟨hi.before, hi.after, hi.holder, hi.landed_first, hi.unresolved, hi.resolved,
      hi.decider_holder⟩
  -- Resolving: the closure is taken and the outcome decided, by the holder.
  | resolve hl hr hp =>
    -- Nothing ran before, as the outcome was pending; and nobody had decided.
    have hran : k.ran = [] := hi.before (Or.inl hp)
    -- (nobody had decided)
    have hdec : k.decider = [] := (hi.unresolved hr).2
    -- Seven facts, one after another.
    refine ⟨fun _ => hran, fun h => ?_, hi.holder, fun _ => hl, fun h => ?_, fun _ => ?_,
      fun r hm => ?_⟩
    -- Decided is neither delivering nor ready.
    · rcases h with h | h <;> cases h
    -- The closure is gone, so "the closure is in the ticket" is false.
    · cases h
    -- One decision: the new one on an empty list.
    · simp [hdec]
    -- The only decision is the holder's.
    ·
      -- With the old list empty, r is the new decision.
      simp [hdec] at hm
      -- It is `.holder` of the thread that held the queue.
      exact ⟨k.holder, hm⟩
  -- The claim: one run, by the holder.
  | deliver hd =>
    -- Nothing ran before, as the outcome was only decided.
    have hr : k.ran = [] := hi.before (Or.inr hd)
    -- The outcome was decided, so the closure is gone.
    have hres : k.resolver = false := by
      -- Split on whether the closure is there.
      cases hc : k.resolver with
      -- Gone: that is the claim.
      | false => rfl
      -- There: then the outcome would be pending, but it is decided.
      | true => have := (hi.unresolved hc).1; rw [hd] at this; cases this
    -- Seven facts, one after another.
    refine ⟨fun h => ?_, fun _ => ?_, fun r hm => ?_, fun _ => ?_, fun h => ?_, hi.resolved,
      hi.decider_holder⟩
    -- Delivering is neither pending nor decided.
    · rcases h with h | h <;> cases h
    -- One run: the new one on an empty list.
    · simp [hr]
    -- The only run is the holder's.
    ·
      -- With the old list empty, r is the new run.
      simp [hr] at hm
      -- It is `.holder` of the thread that held the queue.
      exact ⟨k.holder, hm⟩
    -- The commit had landed when the outcome was decided.
    · exact hi.landed_first (by rw [hd]; exact fun h => by cases h)
    -- The closure is gone, so "the closure is in the ticket" is false.
    · rw [hres] at h; cases h
  -- Finishing: ready, the runs untouched.
  | finish hd =>
    -- The single run stays.
    have hl : k.ran.length = 1 := hi.after (Or.inl hd)
    -- The closure was gone before, as the outcome was not pending.
    have hres : k.resolver = false := by
      -- Split on whether the closure is there.
      cases hc : k.resolver with
      -- Gone: that is the claim.
      | false => rfl
      -- There: then the outcome would be pending, but it is being delivered.
      | true => have := (hi.unresolved hc).1; rw [hd] at this; cases this
    -- Seven facts, one after another.
    refine ⟨fun h => ?_, fun _ => hl, hi.holder, fun _ => ?_, fun h => ?_, hi.resolved,
      hi.decider_holder⟩
    -- Ready is neither pending nor decided.
    · rcases h with h | h <;> cases h
    -- The commit had landed when the outcome was decided.
    · exact hi.landed_first (by rw [hd]; exact fun h => by cases h)
    -- The closure is gone, so "the closure is in the ticket" is false.
    · rw [hres] at h; cases h

/-- **The invariant holds after any number of steps.** -/
theorem dsteps_inv {k k' : Ticket}
    -- Steps from `k` to `k'`.
    (h : DSteps k k')
    -- The invariant holds at `k`.
    (hi : DInv k) :
    -- Then:
    DInv k' := by
  -- By induction on the steps.
  induction h with
  -- No step: the same state.
  | refl => exact hi
  -- Steps, then one more: the last step keeps it.
  | tail _ hs ih => exact dstep_inv ih hs

/-- **An outcome is delivered at most once, and exactly once when the
ticket is ready**, however many paths (the poll, the drop, the fallback)
try. TLA+: `AtMostOnce` and `ExactlyOnce` (`TxnCallbacks.tla`, with
`MC_TxnCallbacks_Red_DeliverNoClaim` as the RED). Rules out: the poll and
the queue's drop both running A's `on_commit`. -/
theorem delivered_once {t : Nat} {k : Ticket}
    -- `k` is reachable from a fresh ticket.
    (h : DSteps (fresh t) k) :
    -- Then:
    k.ran.length ≤ 1 ∧ (k.st = .ready → k.ran.length = 1) := by
  -- The invariant of `k`.
  have hi := dsteps_inv h (dinv_init t)
  -- The second half is one field.
  refine ⟨?_, fun hr => hi.after (Or.inr hr)⟩
  -- Every state is before or after the claim; count the runs in each.
  cases hs : k.st with
  -- Pending: no run.
  | pending => simp [hi.before (Or.inl hs)]
  -- Decided: no run.
  | decided => simp [hi.before (Or.inr hs)]
  -- Delivering: one run.
  | delivering => simp [hi.after (Or.inl hs)]
  -- Ready: one run.
  | ready => simp [hi.after (Or.inr hs)]

/-- **Every delivery runs on the thread that holds the commit's queue**,
never on a committer. TLA+: `OnCommittingThread`, with
`MC_TxnCallbacks_Red_HelperDelivers` as the RED. Rules out: thread B, which
landed A's commit, running A's `on_commit` on B. -/
theorem delivered_on_queue_thread {t : Nat} {k : Ticket}
    -- `k` is reachable from a fresh ticket.
    (h : DSteps (fresh t) k)
    -- A run of the outcome callbacks.
    {r : Runner} (hr : r ∈ k.ran) :
    -- Then:
    ∃ q, r = .holder q :=
  -- One field of the invariant.
  (dsteps_inv h (dinv_init t)).holder r hr

/-- **A commit owed an fsync is resolved only after it has landed.**
A ticket whose outcome is decided, being delivered or ready belongs to a
commit whose fsync is done. TLA+: `ResolvedAfterLanding`. Rules out: a
caller that blocks on the ticket's decision is told "committed", and the
machine loses power before the fsync. -/
theorem resolved_after_landing {t : Nat} {k : Ticket}
    -- `k` is reachable from a fresh ticket.
    (h : DSteps (fresh t) k)
    -- Its outcome is no longer pending.
    (hs : k.st ≠ .pending) :
    -- Then:
    k.landed = true :=
  -- One field of the invariant.
  (dsteps_inv h (dinv_init t)).landed_first hs

/-- **The outcome is decided at most once, on the thread that holds the
commit's queue.** Only the deliverer that got the closure decides, and it
runs on the queue's thread; the thread that landed the commit decides
nothing. TLA+: the steps `DeliverBegins` and `DeliverDecides`, with
`MC_TxnCallbacks_Red_HelperDelivers` as the RED. -/
theorem resolved_once_on_queue_thread {t : Nat} {k : Ticket}
    -- `k` is reachable from a fresh ticket.
    (h : DSteps (fresh t) k) :
    -- Then: at most one decision, and every decision by a holder of the queue.
    k.decider.length ≤ 1 ∧ ∀ r ∈ k.decider, ∃ q, r = .holder q := by
  -- The invariant of `k`.
  have hi := dsteps_inv h (dinv_init t)
  -- The second half is one field.
  refine ⟨?_, hi.decider_holder⟩
  -- Is the closure still in the ticket?
  cases hc : k.resolver with
  -- Gone: exactly one decision.
  | false => simp [hi.resolved hc]
  -- There: no decision yet.
  | true => simp [(hi.unresolved hc).2]

/-! ### RED: delivering without the claim

The bug: a deliverer checks that the outcome is decided and runs the
callbacks, but does not CAS the state word, so a second path delivers
too. -/

/-- The broken deliver: the outcome stays decided. -/
def deliverNoClaim (k : Ticket) : Ticket :=
  -- One more run by the holder; the state word is left as it was.
  { k with ran := .holder k.holder :: k.ran }

/-- **RED: without the claim an outcome is delivered twice.** The poll and
the queue's drop both find the outcome decided and both run its
callbacks. TLA+: `MC_TxnCallbacks_Red_DeliverNoClaim` breaks `AtMostOnce`. -/
theorem no_claim_delivers_twice :
    -- Two broken deliveries of a decided outcome make two runs.
    (deliverNoClaim (deliverNoClaim ⟨1, .decided, [], true, false, [.holder 1]⟩)).ran.length
      = 2 := by
  -- Two runs, one per broken delivery.
  rfl

/-! ### RED: resolving before the commit has landed

The bug: a delivery begins before the commit is synced, so the
closure reads an outcome that is not durable yet. -/

/-- The broken resolve: it does not ask whether the commit has landed. -/
def resolveEarly (k : Ticket) : Ticket :=
  -- The closure is taken and the outcome decided, whatever "landed" says.
  { k with resolver := false, st := .decided, decider := .holder k.holder :: k.decider }

/-- **RED: without the wait an outcome is decided before its fsync.** A
fresh ticket, resolved at once, is decided while its commit has not landed.
TLA+: `MC_TxnCallbacks_Red_CommitBeforeDurable` breaks `ResolvedAfterLanding`
and then `CommitAfterDurable`. -/
theorem early_resolve_is_not_durable :
    -- The broken resolve of a fresh ticket: decided, and not landed.
    (resolveEarly (fresh 1)).st = .decided ∧ (resolveEarly (fresh 1)).landed = false := by
  -- Both halves compute.
  exact ⟨rfl, rfl⟩

-- End of the namespace opened at the top.
end Regolith.Callbacks
