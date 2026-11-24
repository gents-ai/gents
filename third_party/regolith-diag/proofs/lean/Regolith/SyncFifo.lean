/-!
# SyncFifo: the FIFO waiter queue with handoff before wake (regolith::sync::Notify)

## The story, for a reader who has never seen the code

Think of a counter with one clerk and a line of people. A "permit" is the
clerk's attention. Whoever arrives while the clerk is busy joins the back
of the line and falls asleep. When the clerk becomes free, the permit is
put straight into the hand of the person at the front of the line, and only
then is that person woken. Nobody who arrives later can take the permit in
between, because it is never left lying on the counter while someone waits.

In the code this is the waiter queue of `Notify` (src/sync/notify.rs, on
top of `WaitQueue` in src/sync/queue.rs): a task that calls `notified()`
waits in line, and `notify_one()` hands one notification to the oldest
waiter and then wakes it.

Tiny example of what could go wrong, and does in the RED cases at the end
of this file. Request 1 holds the permit and request 2 waits. Request 1
releases. If the permit were only put back on the counter and request 2
woken, a newcomer, request 3, could grab it before request 2 gets there:
2 arrived first and is served last (`barging_breaks_fifo`). If request 2
were woken before the permit is in its hand, it could look, find nothing,
and fall asleep again, and then nobody would ever wake it
(`wake_before_handoff_strands`). If request 2 were handed the permit and
then gave up without passing it on, the permit would be gone and request 3
would wait for ever (`cancel_without_pass_on_strands`).

The theorems show that the design, where the permit is handed over first
and a waiter that gives up passes it on, has none of these problems, for
any number of permits, requests and steps.

## What this file backs

This file backs the TLA+ models of `regolith::sync` that keep FIFO
handoff in `proofs/tla`:

* `SyncNotify.tla`, invariants `FifoHandoff`, `NoLostWakeup` and
  `CancelPassesOn` (configurations `MC_SyncNotify_*`): Notify hands a
  notification to its oldest waiter before waking it;
* the waiter list of `SyncLatch.tla`, which uses the same registration and
  re-check.

Before D49 this file backed the locks too. D49 replaced FIFO handoff by
barging with a bypass bound for the lock-like primitives; their proofs are
`Regolith/Sync.lean`. Notify keeps its semantics, so the queue laws here
still back it: FIFO service, no waiter stranded beside a stored permit,
handoff before wake, and the RED cases. The model holds a fixed number of
permits, which Notify does not (notify_one creates one, notified() consumes
it), so the counting laws `permits_bounded` and `at_most_one_owner` are
about the fixed-permit queue only.

TLC checks those models step by atomic step for three tasks. Here the queue
is a sequential state machine: each step is one of the abstract actions
(fast acquire, enqueue, release, wake, poll, cancel), and a reachable state
is the result of any sequence of them, so every interleaving of the actions
is covered. The theorems hold for every number of permits, requests and
steps. The model has `n` permits and every request asks for one; Notify
is `n = 1`.

What is proved, in plain words:

1. `step_inv` and `reachable_inv`: the queue's invariant holds in every
   reachable state.
2. `permits_bounded` and `at_most_one_owner`: at most `n` requests hold a
   permit; with one permit, at most one owner exists at any time.
3. `fifo_served` and `served_in_order`: requests are served in the order
   they entered the line, and no request is ever served while one that
   entered before it still waits. Cancelled waiters are skipped.
4. `no_stranded_waiter`: while anyone waits, no permit is free; a release
   hands its permit to the oldest waiter instead of freeing it.
5. `handed_holds` and `no_lost_handoff`: a woken waiter always owns its
   permit (the handoff comes before the wake), and a waiter handed a permit
   is woken or owed a wake.
6. `position_never_grows` and `handoff_advances`: the waiters ahead of a
   waiter only ever leave, and every handoff removes one of them or serves
   it, so a waiter that is not cancelled is served after at most as many
   handoffs as there were waiters ahead of it, plus one.
7. Cancellation with pass-on is one of the steps, so all of the above hold
   with it. Three RED cases, as counterexamples:
   `barging_breaks_fifo` (a fast path that ignores the queue),
   `wake_before_handoff_strands` (waking before handing over loses the
   wake for good) and `cancel_without_pass_on_strands` (a cancelled waiter
   that keeps its permit leaves the next waiter unserved forever).

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. `l.Sublist m` (written `<+` in some texts) says the elements of
`l` appear in `m` in the same order, possibly with others between them;
`[a, b].Sublist l` therefore says that `a` comes before `b` in `l`.
-/

-- Everything below is named Regolith.SyncFifo.<name>.
namespace Regolith.SyncFifo

/-! ## The state -/

/-- The waiter queue's state. A request is named by a natural number,
fresh for every acquire. -/
structure Q where
  /-- Permits nobody holds. -/
  free : Nat
  /-- The waiting requests, oldest first. -/
  queue : List Nat
  /-- Requests that hold a permit and know it: they took it on the fast
  path, or were handed it and have been polled since. -/
  owners : List Nat
  /-- Requests a release handed a permit to, not yet polled. They hold
  the permit already. -/
  handed : List Nat
  /-- Requests whose Waker must still be called. -/
  owed : List Nat
  /-- Requests whose Waker was called, not yet polled. -/
  woken : List Nat
  /-- Ghost: every request that entered the line (took the fast path or
  enqueued), in order. -/
  enq : List Nat
  /-- Ghost: every request that was given a permit, in order. -/
  served : List Nat
  -- Lean may compare two states for equality, so the counterexamples at the end can be computed.
  deriving DecidableEq

/-- The start: `n` free permits, nobody waiting or holding. -/
def init (n : Nat) : Q := ⟨n, [], [], [], [], [], [], []⟩

/-- A permit comes back (a release, or a cancelled waiter passing on what
it was handed). If someone waits, the oldest waiter is handed the permit
and owed a wake; only if nobody waits does the permit become free. -/
def handoff (s : Q) : Q :=
  -- Look at the queue.
  match s.queue with
  -- Nobody waits: the permit is free.
  | [] => { s with free := s.free + 1 }
  -- The oldest waiter `h` is handed the permit, and only then owed a wake.
  | h :: t => { s with queue := t, handed := s.handed ++ [h], owed := s.owed ++ [h],
                       served := s.served ++ [h] }

/-! ## The steps -/

/-- The design's steps. -/
inductive Step : Q → Q → Prop
  /-- `try_acquire`, or `acquire` uncontended: a fresh request `r` takes a
  free permit, but only when nobody waits (it never jumps the queue). -/
  | fast (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- Nobody waits.
      (hq : s.queue = [])
      -- A permit is free.
      (hf : 0 < s.free) :
      -- After the step: one free permit fewer, r is an owner,
      Step s { s with free := s.free - 1, owners := r :: s.owners,
                      -- r has entered the line, and r has been served.
                      enq := s.enq ++ [r], served := s.served ++ [r] }
  /-- A contended `acquire`: a fresh request `r` joins the back of the
  queue. Contended means someone waits or no permit is free: the re-check
  after the push (SyncNotify.tla's drain) takes a free permit itself otherwise. -/
  | enqueue (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- The fast path is shut.
      (hc : s.queue ≠ [] ∨ s.free = 0) :
      -- After the step: r is at the back of the queue and of the line.
      Step s { s with queue := s.queue ++ [r], enq := s.enq ++ [r] }
  /-- An owner drops its guard: its permit goes through `handoff`. -/
  | release (s : Q) (r : Nat)
      -- `r` owns a permit.
      (hr : r ∈ s.owners) :
      -- After the step: r is no longer an owner, and its permit went through `handoff`.
      Step s (handoff { s with owners := s.owners.erase r })
  /-- `Waker::wake` for a request owed a wake. -/
  | wake (s : Q) (r : Nat)
      -- `r` is owed a wake.
      (hr : r ∈ s.owed) :
      -- After the step: r is no longer owed a wake, and r is woken.
      Step s { s with owed := s.owed.filter (· != r), woken := s.woken ++ [r] }
  /-- The executor polls a woken request, which finds its permit and
  becomes an owner. -/
  | poll (s : Q) (r : Nat)
      -- `r` was woken.
      (hw : r ∈ s.woken)
      -- `r` was handed a permit.
      (hh : r ∈ s.handed) :
      -- After the step: r is no longer "woken" or "handed",
      Step s { s with woken := s.woken.filter (· != r), handed := s.handed.erase r,
                      -- and r is an owner.
                      owners := r :: s.owners }
  /-- A waiting request's future is dropped: it leaves the queue. -/
  | cancelWaiting (s : Q) (r : Nat)
      -- `r` waits.
      (hr : r ∈ s.queue) :
      -- After the step: r has left the queue.
      Step s { s with queue := s.queue.erase r }
  /-- A request's future is dropped after it was handed a permit but
  before it was polled: it passes the permit on through `handoff`. -/
  | cancelHanded (s : Q) (r : Nat)
      -- `r` was handed a permit.
      (hr : r ∈ s.handed) :
      -- After the step: r is no longer "handed",
      Step s (handoff { s with handed := s.handed.erase r,
                               -- no longer owed a wake,
                               owed := s.owed.filter (· != r),
                               -- no longer "woken", and its permit went through `handoff`.
                               woken := s.woken.filter (· != r) })

/-- Any number of steps, one after another. -/
inductive Steps : Q → Q → Prop
  /-- No step at all. -/
  | refl (s : Q) : Steps s s
  /-- Some steps, then one more. -/
  | tail {s t u : Q} : Steps s t → Step t u → Steps s u

/-! ## The invariant -/

/-- An element of a filtered list is an element of the list. -/
theorem mem_of_mem_filter {l : List Nat} {p : Nat → Bool} {a : Nat}
    -- `a` survived the filter.
    (h : a ∈ l.filter p) :
    -- The statement: a is in the list itself.
    a ∈ l :=
  -- Being in a filtered list means being in the list and passing the filter: take the first half.
  (List.mem_filter.1 h).1

/-- Everything about a state except the permit count. -/
structure Core (s : Q) : Prop where
  /-- While anyone waits, no permit is free. -/
  no_stranded : s.queue ≠ [] → s.free = 0
  /-- The served requests, then the waiting ones, appear in the line in
  this order: service follows entry order. -/
  line : (s.served ++ s.queue).Sublist s.enq
  /-- Every request entered the line once. -/
  enq_nodup : s.enq.Nodup
  /-- No request holds two permits. -/
  holders_nodup : (s.owners ++ s.handed).Nodup
  /-- Every holder was served. -/
  holders_served : ∀ r, r ∈ s.owners ∨ r ∈ s.handed → r ∈ s.served
  /-- A request owed a wake holds its permit already. -/
  owed_handed : ∀ r ∈ s.owed, r ∈ s.handed
  /-- A woken request holds its permit already. -/
  woken_handed : ∀ r ∈ s.woken, r ∈ s.handed
  /-- A request handed a permit is owed a wake or was woken. -/
  handed_woken : ∀ r ∈ s.handed, r ∈ s.owed ∨ r ∈ s.woken
  /-- No request is both owed a wake and woken. -/
  owed_not_woken : ∀ r ∈ s.owed, r ∉ s.woken

/-- The invariant for `n` permits: the core, and every permit is free or
held. -/
structure Inv (n : Nat) (s : Q) : Prop where
  /-- Everything but the count. -/
  core : Core s
  /-- Free permits plus held permits are `n`. -/
  conserve : s.free + s.owners.length + s.handed.length = n

/-- `[b, a]` appears in `served ++ queue` when `b` was served and `a`
waits: whoever is served sits before whoever waits. -/
theorem served_before_waiting {s : Q} {a b : Nat}
    -- `b` was served.
    (hb : b ∈ s.served)
    -- `a` waits.
    (ha : a ∈ s.queue) :
    -- The statement: [b, a] appears, in this order, in served followed by queue.
    [b, a].Sublist (s.served ++ s.queue) :=
  -- `[b]` sits inside `served` and `[a]` inside `queue`.
  (List.singleton_sublist.2 hb).append (List.singleton_sublist.2 ha)

/-- The served and waiting requests are all distinct. -/
theorem Core.line_nodup {s : Q}
    -- The core invariant holds.
    (h : Core s) : (s.served ++ s.queue).Nodup :=
  -- A sublist of a list without repeats has none.
  h.enq_nodup.sublist h.line

/-- A waiting request holds nothing and was never served. -/
theorem Core.waiting_fresh {s : Q}
    -- The core invariant holds.
    (h : Core s) {r : Nat}
    -- `r` waits.
    (hr : r ∈ s.queue) :
    -- The statement: r was not served, is no owner, and was not handed a permit.
    r ∉ s.served ∧ r ∉ s.owners ∧ r ∉ s.handed := by
  -- `served ++ queue` has no repeats, so `r` is not in `served`.
  have hns : r ∉ s.served := by
    -- Suppose r was served.
    intro hs
    -- Then r would be in both "served" and "queue", which share nobody.
    exact (List.nodup_append.1 h.line_nodup).2.2 r hs r hr rfl
  -- Every holder was served, so `r` holds nothing.
  -- r was not served; r is no owner, since owners were served;
  exact ⟨hns, fun ho => hns (h.holders_served r (Or.inl ho)),
    -- and r was not handed a permit, since those were served too.
    fun hh => hns (h.holders_served r (Or.inr hh))⟩

/-- No request is both an owner and handed a permit. -/
theorem Core.not_owner_of_handed {s : Q}
    -- The core invariant holds.
    (h : Core s) {r : Nat}
    -- `r` was handed a permit.
    (hr : r ∈ s.handed) :
    -- The statement: r is no owner.
    r ∉ s.owners :=
  -- `owners ++ handed` has no repeats.
  fun ho => (List.nodup_append.1 h.holders_nodup).2.2 r ho r hr rfl

/-- The handed requests are distinct. -/
theorem Core.handed_nodup {s : Q}
    -- The core invariant holds.
    (h : Core s) : s.handed.Nodup :=
  -- "owners followed by handed" is distinct, so "handed" on its own is.
  (List.nodup_append.1 h.holders_nodup).2.1

/-! ## Every step keeps the invariant -/

/-- A permit coming back through `handoff` restores the count: if `s` is
one permit short, `handoff s` is not. And the core survives. -/
theorem handoff_inv {n : Nat} {s : Q}
    -- The core holds before the permit comes back.
    (hc : Core s)
    -- The count is one short.
    (hn : s.free + s.owners.length + s.handed.length + 1 = n) :
    -- The statement: after the handoff, the invariant holds for n permits.
    Inv n (handoff s) := by
  -- Unfold `handoff`.
  unfold handoff
  -- Is anyone waiting?
  cases hq : s.queue with
  -- Case: nobody waits.
  | nil =>
    -- Nobody waits: the permit becomes free, and nothing else changes.
    -- The core's nine facts: "nobody waits" makes the first one empty; the line is left to show;
    -- the others are unchanged.
    refine ⟨⟨fun h => absurd rfl h, ?_, hc.enq_nodup, hc.holders_nodup, hc.holders_served,
      -- (the remaining unchanged facts), and the count is left to show.
      hc.owed_handed, hc.woken_handed, hc.handed_woken, hc.owed_not_woken⟩, ?_⟩
    -- The line still holds `served` in order.
    ·
      -- The old line fact.
      have := hc.line
      -- The queue is empty.
      rw [hq] at this
      -- So it says exactly what is needed.
      simpa using this
    -- The count: one more free permit makes up the missing one.
    · simp only; omega
  -- Case: a head h and a tail t.
  | cons h t =>
    -- `h` waited: it is fresh to every holder list.
    have hh : h ∈ s.queue := by rw [hq]; exact List.mem_cons_self
    -- So h was not served, is no owner, and was not handed a permit.
    obtain ⟨hns, hno, hnh⟩ := hc.waiting_fresh hh
    -- `h` was not woken either: woken requests hold permits.
    have hnw : h ∉ s.woken := fun hw => hnh (hc.woken_handed h hw)
    -- The core's nine facts (the entries stay distinct), then the count.
    refine ⟨⟨?_, ?_, hc.enq_nodup, ?_, ?_, ?_, ?_, ?_, ?_⟩, ?_⟩
    -- Others still wait only if the free count was already 0.
    ·
      -- Suppose somebody still waits.
      intro _
      -- Somebody waited before (the queue was h :: t), so nothing was free.
      exact hc.no_stranded (by rw [hq]; exact List.cons_ne_nil h t)
    -- `served ++ [h] ++ t` is `served ++ (h :: t)`, the old line.
    ·
      -- The old line fact.
      have := hc.line
      -- The queue was h :: t.
      rw [hq] at this
      -- Regrouping the lists gives exactly what is needed.
      simpa [List.append_assoc] using this
    -- `h` joins the holders, to which it is new.
    ·
      -- Simplify the fields after the step.
      simp only
      -- Regroup: (owners ++ handed) followed by [h]; "distinct" splits into three parts.
      rw [← List.append_assoc, List.nodup_append]
      -- The old holders are distinct; [h] is; and no old holder equals h, which is left.
      refine ⟨hc.holders_nodup, by simp, ?_⟩
      -- Take an old holder a and a member b of [h].
      intro a ha b hb
      -- b is h.
      simp only [List.mem_singleton] at hb
      -- Replace b by h.
      subst hb
      -- Suppose a equals h.
      intro hab
      -- Replace a by h.
      subst hab
      -- h would be an owner or handed.
      rcases List.mem_append.1 ha with ho | hd
      -- An owner: it is not.
      · exact hno ho
      -- Handed: it is not.
      · exact hnh hd
    -- Every holder was served; `h` now is.
    ·
      -- Take a holder r after the step.
      intro r hr
      -- Spell out the lists: r is an owner, or handed before, or h; and "served" is the old list or
      -- h.
      simp only [List.mem_append, List.mem_singleton] at hr ⊢
      -- Split on which.
      rcases hr with ho | hd | rfl
      -- An owner: served before.
      · exact Or.inl (hc.holders_served r (Or.inl ho))
      -- Handed before: served before.
      · exact Or.inl (hc.holders_served r (Or.inr hd))
      -- h: served now.
      · exact Or.inr rfl
    -- `h` is owed a wake and holds its permit.
    ·
      -- Take a request r owed a wake after the step.
      intro r hr
      -- It was owed before, or it is h; "handed" is the old list or h.
      simp only [List.mem_append, List.mem_singleton] at hr ⊢
      -- Split on which.
      rcases hr with hr | rfl
      -- Owed before: handed before.
      · exact Or.inl (hc.owed_handed r hr)
      -- h: handed now.
      · exact Or.inr rfl
    -- Woken requests are unchanged and still hold.
    ·
      -- Take a woken request r.
      intro r hr
      -- "Handed" is the old list or h.
      simp only [List.mem_append]
      -- r was handed before.
      exact Or.inl (hc.woken_handed r hr)
    -- `h` is owed a wake; every other handed request as before.
    ·
      -- Take a request r handed after the step.
      intro r hr
      -- It was handed before, or it is h.
      simp only [List.mem_append, List.mem_singleton] at hr ⊢
      -- Split on which.
      rcases hr with hr | rfl
      -- Handed before: it was owed a wake or woken.
      · rcases hc.handed_woken r hr with ho | hw
        -- Owed: still owed.
        · exact Or.inl (Or.inl ho)
        -- Woken: still woken.
        · exact Or.inr hw
      -- h: owed a wake now.
      · exact Or.inl (Or.inr rfl)
    -- `h` was not woken, so it is owed without being woken.
    ·
      -- Take a request r owed a wake after the step.
      intro r hr
      -- It was owed before, or it is h.
      simp only [List.mem_append, List.mem_singleton] at hr
      -- Split on which.
      rcases hr with hr | rfl
      -- Owed before: not woken, as before.
      · exact hc.owed_not_woken r hr
      -- h: not woken, as shown above.
      · exact hnw
    -- One more holder, one fewer missing permit.
    ·
      -- The handed list is one longer.
      simp only [List.length_append, List.length_singleton]
      -- Arithmetic.
      omega

/-- The start satisfies the invariant. -/
-- The statement: the start state satisfies the invariant for n permits.
theorem init_inv (n : Nat) : Inv n (init n) := by
  -- Everything is empty, and every permit is free.
  -- The core's facts and the count: with every list empty, the first four are immediate,
  refine ⟨⟨fun h => absurd rfl h, List.Sublist.slnil, List.nodup_nil, List.nodup_nil,
    -- and the rest follow by computing on the empty lists.
    ?_, ?_, ?_, ?_, ?_⟩, ?_⟩ <;> simp [init]

/-- **Every step keeps the invariant.** -/
theorem step_inv {n : Nat} {s t : Q}
    -- The invariant holds before the step.
    (hinv : Inv n s)
    -- One step of the design.
    (hstep : Step s t) :
    -- The statement: the invariant holds after the step.
    Inv n t := by
  -- Take the invariant apart: the core, and the count.
  obtain ⟨hc, hn⟩ := hinv
  -- Split on the kind of step.
  cases hstep with
  -- Case: the fast path, for a fresh request r.
  | fast r hr hq hf =>
    -- `r` is new to the line, so new to every list drawn from it.
    have hserved : r ∉ s.served := fun h =>
      -- (a served request is in the line, and r is not in the line)
      hr ((List.sublist_append_left _ _).trans hc.line |>.subset h)
    -- The core's nine facts: nobody waits; four are left to show; the wake facts are unchanged;
    refine ⟨⟨fun h => absurd hq h, ?_, ?_, ?_, ?_, hc.owed_handed, hc.woken_handed,
      -- then the count.
      hc.handed_woken, hc.owed_not_woken⟩, ?_⟩
    -- Nobody waits, so the line is `served`, and `r` joins both ends.
    ·
      -- The old line fact.
      have := hc.line
      -- The queue is empty, before and after.
      rw [hq] at this ⊢
      -- Adding r at the end of both lists keeps one inside the other.
      simpa using this.append (List.Sublist.refl [r])
    -- `r` is fresh.
    ·
      -- "Distinct" for the old entries followed by [r] splits into three parts.
      rw [List.nodup_append]
      -- The old entries were distinct; [r] is; and no old entry equals r, which is left.
      refine ⟨hc.enq_nodup, by simp, ?_⟩
      -- Take an old entry a and a member b of [r].
      intro a ha b hb
      -- b is r.
      simp only [List.mem_singleton] at hb
      -- Replace b by r.
      subst hb
      -- Suppose a equals r.
      intro hab
      -- Replace a by r.
      subst hab
      -- But r had not entered.
      exact hr ha
    -- `r` holds nothing yet: it was never served.
    ·
      -- The holders are now r followed by the old ones: "distinct" means r is not among the old
      -- ones, and they are distinct.
      simp only [List.cons_append, List.nodup_cons]
      -- They were distinct; "r is not among them" is left.
      refine ⟨?_, hc.holders_nodup⟩
      -- Suppose r is among the old holders.
      intro hm
      -- It would be an owner or handed.
      rcases List.mem_append.1 hm with ho | hd
      -- An owner: then it was served. It was not.
      · exact hserved (hc.holders_served r (Or.inl ho))
      -- Handed: then it was served. It was not.
      · exact hserved (hc.holders_served r (Or.inr hd))
    -- `r` is served now; the others were before.
    ·
      -- Take a holder x after the step.
      intro x hx
      -- To show: x was served before, or x is r.
      rw [List.mem_append, List.mem_singleton]
      -- "x is an owner" now means: x is r or an old owner.
      simp only [List.mem_cons] at hx
      -- Split on which.
      rcases hx with (rfl | ho) | hd
      -- r: served now.
      · exact Or.inr rfl
      -- An old owner: served before.
      · exact Or.inl (hc.holders_served x (Or.inl ho))
      -- Handed: served before.
      · exact Or.inl (hc.holders_served x (Or.inr hd))
    -- One permit moves from free to `r`.
    ·
      -- The owners list is one longer.
      simp only [List.length_cons]
      -- Arithmetic: one fewer free, one more held.
      omega
  -- Case: a fresh request r joins the queue.
  | enqueue r hr hcont =>
    -- The core's nine facts: three are left to show; the holders and the wake facts are unchanged;
    refine ⟨⟨?_, ?_, ?_, hc.holders_nodup, hc.holders_served, hc.owed_handed,
      -- the count is unchanged.
      hc.woken_handed, hc.handed_woken, hc.owed_not_woken⟩, hn⟩
    -- Someone waited already (so nothing was free), or nothing was free.
    ·
      -- Suppose somebody waits after the step.
      intro _
      -- Split on why the fast path was shut.
      rcases hcont with hq | hf
      -- Somebody waited: so nothing was free.
      · exact hc.no_stranded hq
      -- Nothing was free: that is the claim.
      · exact hf
    -- `r` joins the back of the queue and of the line.
    ·
      -- Adding r at the end of both lists keeps one inside the other.
      have := hc.line.append (List.Sublist.refl [r])
      -- Regrouping gives exactly what is needed.
      simpa [List.append_assoc] using this
    -- `r` is fresh.
    ·
      -- "Distinct" for the old entries followed by [r] splits into three parts.
      rw [List.nodup_append]
      -- The old entries were distinct; [r] is; and no old entry equals r, which is left.
      refine ⟨hc.enq_nodup, by simp, ?_⟩
      -- Take an old entry a and a member b of [r].
      intro a ha b hb
      -- b is r.
      simp only [List.mem_singleton] at hb
      -- Replace b by r.
      subst hb
      -- Suppose a equals r.
      intro hab
      -- Replace a by r.
      subst hab
      -- But r had not entered.
      exact hr ha
  -- Case: an owner r drops its guard.
  | release r hr =>
    -- Drop `r` from the owners; the permit comes back through `handoff`.
    -- It is enough to show the core for the state without r, and that its count is one short.
    apply handoff_inv
    -- The core's nine facts: two are left to show; the others are unchanged.
    · refine ⟨hc.no_stranded, hc.line, hc.enq_nodup, ?_, ?_, hc.owed_handed, hc.woken_handed,
        -- (the unchanged wake facts)
        hc.handed_woken, hc.owed_not_woken⟩
      -- Fewer owners: still no repeats.
      ·
        -- Removing an owner leaves a sublist, and a sublist of a distinct list is distinct.
        exact hc.holders_nodup.sublist (List.erase_sublist.append (List.Sublist.refl _))
      -- Fewer owners: still all served.
      ·
        -- Take a holder x of the state without r.
        intro x hx
        -- It is an owner or handed.
        rcases hx with ho | hd
        -- An owner: an owner before, so served.
        · exact hc.holders_served x (Or.inl (List.mem_of_mem_erase ho))
        -- Handed: served.
        · exact hc.holders_served x (Or.inr hd)
    -- One owner fewer.
    ·
      -- Simplify the fields.
      simp only
      -- Removing r makes the owners list one shorter.
      have := List.length_erase_of_mem hr
      -- The owners list was not empty, since r was in it.
      have : 0 < s.owners.length := List.length_pos_of_mem hr
      -- Arithmetic: the count is one short.
      omega
  -- Case: a request r owed a wake is woken.
  | wake r hr =>
    -- The core's nine facts: four wake facts are left to show; the others are unchanged;
    refine ⟨⟨hc.no_stranded, hc.line, hc.enq_nodup, hc.holders_nodup, hc.holders_served,
      -- the count is unchanged.
      ?_, ?_, ?_, ?_⟩, hn⟩
    -- Fewer owed requests: still holders.
    ·
      -- Take a request x still owed a wake.
      intro x hx
      -- It was owed before, so it was handed a permit.
      exact hc.owed_handed x (mem_of_mem_filter hx)
    -- `r` is woken, and holds its permit since it was owed.
    ·
      -- Take a request x woken after the step.
      intro x hx
      -- It was woken before, or it is r.
      simp only [List.mem_append, List.mem_singleton] at hx
      -- Split on which.
      rcases hx with hx | rfl
      -- Woken before: handed.
      · exact hc.woken_handed x hx
      -- r: it was owed a wake, so it was handed.
      · exact hc.owed_handed x hr
    -- `r` moves from owed to woken; the rest stay where they were.
    ·
      -- Take a handed request x.
      intro x hx
      -- Split on whether x is r.
      by_cases hxr : x = r
      -- It is r:
      · subst hxr
        -- r is woken now.
        exact Or.inr (List.mem_append.2 (Or.inr (List.mem_singleton.2 rfl)))
      -- It is not: it was owed or woken before.
      · rcases hc.handed_woken x hx with ho | hw
        -- Owed: still owed, since only r was removed.
        · exact Or.inl (List.mem_filter.2 ⟨ho, by simpa using hxr⟩)
        -- Woken: still woken.
        · exact Or.inr (List.mem_append.2 (Or.inl hw))
    -- An owed request other than `r` was not woken, and is not `r`.
    ·
      -- Take a request x still owed a wake.
      intro x hx
      -- It was owed before, and it passed the filter.
      have hx' := List.mem_filter.1 hx
      -- So x is not r.
      have hxr : x ≠ r := by simpa using hx'.2
      -- "Woken" now means: woken before, or r. Neither holds:
      simp only [List.mem_append, List.mem_singleton, not_or]
      -- x was not woken before, and x is not r.
      exact ⟨hc.owed_not_woken x hx'.1, hxr⟩
  -- Case: a woken request r is polled.
  | poll r hw hh =>
    -- The handed requests are distinct.
    have hhn := hc.handed_nodup
    -- r, being handed, is no owner.
    have hro := hc.not_owner_of_handed hh
    -- `r` is not owed: it is woken.
    have hnot_owed : r ∉ s.owed := fun ho => hc.owed_not_woken r ho hw
    -- The core's nine facts: six are left to show; then the count.
    refine ⟨⟨hc.no_stranded, hc.line, hc.enq_nodup, ?_, ?_, ?_, ?_, ?_, ?_⟩, ?_⟩
    -- `r` moves from handed to owners: still no repeats.
    ·
      -- The holders are now r, the old owners, and the handed ones without r.
      simp only [List.cons_append, List.nodup_cons]
      -- Two parts: r is not among the rest; the rest is distinct.
      refine ⟨?_, ?_⟩
      -- Suppose r is among the rest.
      · intro hm
        -- It would be an old owner, or handed-without-r.
        rcases List.mem_append.1 hm with ho | hd
        -- An old owner: it is not.
        · exact hro ho
        -- In "handed without r": the handed list had r once, so removing it leaves no r.
        · exact List.Nodup.not_mem_erase hhn hd
      -- The rest is distinct: split "distinct" over the two lists.
      · rw [List.nodup_append]
        -- Take the old fact apart: owners distinct, handed distinct, and the two share nobody.
        obtain ⟨ho, hd, hdis⟩ := List.nodup_append.1 hc.holders_nodup
        -- Owners as before; handed without r is distinct; and they still share nobody.
        exact ⟨ho, hd.erase r, fun a ha b hb => hdis a ha b (List.mem_of_mem_erase hb)⟩
    -- Holders were served; `r` was a holder.
    ·
      -- Take a holder x after the step.
      intro x hx
      -- "x is an owner" now means: x is r or an old owner.
      simp only [List.mem_cons] at hx
      -- Split on which.
      rcases hx with (rfl | ho) | hd
      -- r: it was handed, so served.
      · exact hc.holders_served x (Or.inr hh)
      -- An old owner: served.
      · exact hc.holders_served x (Or.inl ho)
      -- Handed and not r: handed before, so served.
      · exact hc.holders_served x (Or.inr (List.mem_of_mem_erase hd))
    -- Owed requests are not `r`, so still handed.
    ·
      -- Take a request x owed a wake.
      intro x hx
      -- x is not r, since r is not owed.
      have hxr : x ≠ r := fun h => hnot_owed (h ▸ hx)
      -- x was handed, and is not r: still handed.
      exact (List.mem_erase_of_ne hxr).2 (hc.owed_handed x hx)
    -- Woken requests other than `r` are still handed.
    ·
      -- Take a request x woken after the step.
      intro x hx
      -- It was woken before, and passed the filter.
      have hx' := List.mem_filter.1 hx
      -- So x is not r.
      have hxr : x ≠ r := by simpa using hx'.2
      -- x was handed, and is not r: still handed.
      exact (List.mem_erase_of_ne hxr).2 (hc.woken_handed x hx'.1)
    -- A handed request other than `r` is still owed or still woken.
    ·
      -- Take a request x handed after the step.
      intro x hx
      -- It was handed before, and is not r.
      have hx' := (List.Nodup.mem_erase_iff hhn).1 hx
      -- It was owed or woken.
      rcases hc.handed_woken x hx'.2 with ho | hwx
      -- Owed: still owed.
      · exact Or.inl ho
      -- Woken: still woken, since only r was removed.
      · exact Or.inr (List.mem_filter.2 ⟨hwx, by simpa using hx'.1⟩)
    -- Fewer woken: the owed ones still are not woken.
    ·
      -- Take a request x owed a wake, and suppose it is woken after the step.
      intro x hx hxw
      -- Then it was woken before, against the old fact.
      exact hc.owed_not_woken x hx (mem_of_mem_filter hxw)
    -- `r` moves from handed to owners: the count is unchanged.
    ·
      -- The owners list is one longer.
      simp only [List.length_cons]
      -- The handed list is one shorter.
      have := List.length_erase_of_mem hh
      -- It was not empty, since r was in it.
      have : 0 < s.handed.length := List.length_pos_of_mem hh
      -- Arithmetic.
      omega
  -- Case: a waiting request r is dropped.
  | cancelWaiting r hr =>
    -- The core's nine facts: two are left to show; the others are unchanged;
    refine ⟨⟨?_, ?_, hc.enq_nodup, hc.holders_nodup, hc.holders_served, hc.owed_handed,
      -- the count is unchanged.
      hc.woken_handed, hc.handed_woken, hc.owed_not_woken⟩, hn⟩
    -- `r` waited, so someone waited, so nothing was free.
    ·
      -- Suppose somebody still waits.
      intro _
      -- Somebody waited before (r did), so nothing was free.
      exact hc.no_stranded (List.ne_nil_of_mem hr)
    -- Removing a waiter keeps the order of the rest.
    ·
      -- served followed by the queue-without-r is inside served followed by the queue, which is
      -- inside the line.
      exact ((List.Sublist.refl _).append List.erase_sublist).trans hc.line
  -- Case: a request r handed a permit is dropped before it was polled.
  | cancelHanded r hr =>
    -- The handed requests are distinct.
    have hhn := hc.handed_nodup
    -- Drop `r` everywhere; its permit comes back through `handoff`.
    -- It is enough to show the core for the state without r, and that its count is one short.
    apply handoff_inv
    -- The core's nine facts: six are left to show.
    · refine ⟨hc.no_stranded, hc.line, hc.enq_nodup, ?_, ?_, ?_, ?_, ?_, ?_⟩
      -- Fewer handed: still no repeats.
      ·
        -- Removing a handed request leaves a sublist of a distinct list.
        exact hc.holders_nodup.sublist ((List.Sublist.refl _).append List.erase_sublist)
      -- Fewer handed: still all served.
      ·
        -- Take a holder x of the state without r.
        intro x hx
        -- It is an owner or handed.
        rcases hx with ho | hd
        -- An owner: served.
        · exact hc.holders_served x (Or.inl ho)
        -- Handed and not r: handed before, so served.
        · exact hc.holders_served x (Or.inr (List.mem_of_mem_erase hd))
      -- Owed requests other than `r` are still handed.
      ·
        -- Take a request x still owed a wake.
        intro x hx
        -- It was owed before, and passed the filter.
        have hx' := List.mem_filter.1 hx
        -- So x is not r.
        have hxr : x ≠ r := by simpa using hx'.2
        -- x was handed, and is not r: still handed.
        exact (List.mem_erase_of_ne hxr).2 (hc.owed_handed x hx'.1)
      -- Woken requests other than `r` are still handed.
      ·
        -- Take a request x still woken.
        intro x hx
        -- It was woken before, and passed the filter.
        have hx' := List.mem_filter.1 hx
        -- So x is not r.
        have hxr : x ≠ r := by simpa using hx'.2
        -- x was handed, and is not r: still handed.
        exact (List.mem_erase_of_ne hxr).2 (hc.woken_handed x hx'.1)
      -- A handed request other than `r` is still owed or still woken.
      ·
        -- Take a request x still handed.
        intro x hx
        -- It was handed before, and is not r.
        have hx' := (List.Nodup.mem_erase_iff hhn).1 hx
        -- "x is not r", as the filters test it.
        have hxr : (x != r) = true := by simpa using hx'.1
        -- It was owed or woken.
        rcases hc.handed_woken x hx'.2 with ho | hw
        -- Owed: still owed.
        · exact Or.inl (List.mem_filter.2 ⟨ho, hxr⟩)
        -- Woken: still woken.
        · exact Or.inr (List.mem_filter.2 ⟨hw, hxr⟩)
      -- Fewer owed and woken: still disjoint.
      ·
        -- Take a request x still owed, and suppose it is still woken.
        intro x hx hxw
        -- Then it was both before, against the old fact.
        exact hc.owed_not_woken x (mem_of_mem_filter hx) (mem_of_mem_filter hxw)
    -- One handed request fewer.
    ·
      -- Simplify the fields.
      simp only
      -- Removing r makes the handed list one shorter.
      have := List.length_erase_of_mem hr
      -- It was not empty, since r was in it.
      have : 0 < s.handed.length := List.length_pos_of_mem hr
      -- Arithmetic: the count is one short.
      omega

/-- Several steps keep the invariant. -/
theorem steps_inv {n : Nat} {s t : Q}
    -- The invariant holds at the start.
    (hinv : Inv n s)
    -- Any number of steps.
    (hsteps : Steps s t) :
    -- The statement: the invariant holds at the end.
    Inv n t := by
  -- Go through the steps one at a time.
  induction hsteps with
  -- No step: nothing changed.
  | refl => exact hinv
  -- The steps so far, then one more.
  | tail _ hstep ih => exact step_inv ih hstep

/-- **Every state reachable from the start satisfies the invariant.** -/
theorem reachable_inv {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) : Inv n s :=
  -- The start satisfies the invariant, and every step keeps it.
  steps_inv (init_inv n) h

/-! ## The laws -/

/-- **At most `n` holders.** No more requests hold a permit than there
are permits. (A law of the fixed-permit queue. Notify creates and consumes
notifications, so SyncNotify.tla has no counterpart; the laws that back it
are the FIFO, stranding and handoff ones below.) -/
theorem permits_bounded {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) :
    -- The statement: the owners and the handed requests together are at most n.
    (s.owners ++ s.handed).length ≤ n := by
  -- The count from the invariant: free + owners + handed = n.
  have := (reachable_inv h).conserve
  -- The length of two lists joined is the sum of their lengths.
  simp only [List.length_append]
  -- Arithmetic: a part of a sum is at most the sum.
  omega

/-- **At most one owner.** With one permit, any two holders are the same
request. (A law of the fixed-permit queue, as above.) -/
theorem at_most_one_owner {s : Q}
    -- `s` is reachable from the start, with one permit.
    (h : Steps (init 1) s) {a b : Nat}
    -- `a` holds a permit.
    (ha : a ∈ s.owners ++ s.handed)
    -- `b` holds a permit.
    (hb : b ∈ s.owners ++ s.handed) :
    -- The statement: a and b are the same request.
    a = b := by
  -- The holders are at most one, so any two of them coincide.
  have hlen := permits_bounded h
  -- Call the list of holders l, and forget where it came from.
  generalize s.owners ++ s.handed = l at ha hb hlen
  -- Look at the shape of l: it has a member and at most one entry, so it is a single entry.
  match l, ha, hb, hlen with
  -- The only shape possible: one entry x.
  | [x], ha, hb, _ =>
    -- Being in [x] means being x: so a is x and b is x.
    simp only [List.mem_singleton] at ha hb
    -- Replace both by x: x = x.
    rw [ha, hb]

/-- No list without repeats has two elements in both orders. -/
theorem no_two_orders {l : List Nat} {a b : Nat}
    -- `l` has no repeats.
    (hl : l.Nodup)
    -- `a` comes before `b` in `l`.
    (hab : [a, b].Sublist l)
    -- `b` comes before `a` in `l`.
    (hba : [b, a].Sublist l) :
    -- The statement: this cannot happen.
    False := by
  -- Induction on the list: first the empty list, then a list with one more entry in front.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- The empty list holds neither.
    exact absurd (hab.subset (List.mem_cons_self)) (by simp)
  -- Case: an entry c in front of a list l for which the claim (ih) is already known.
  | cons c l ih =>
    -- No repeats in c :: l means: c is not in l, and l has no repeats.
    obtain ⟨hc, hl'⟩ := List.nodup_cons.1 hl
    -- [a, b] inside c :: l: either it is inside l, or a is c and [b] is inside l.
    rcases List.sublist_cons_iff.1 hab with hab' | ⟨r, hr, hr'⟩
    -- First case, [a, b] inside l. The same split for [b, a].
    · rcases List.sublist_cons_iff.1 hba with hba' | ⟨r', hr2, hr2'⟩
      -- Both orders inside the tail: the tail is the smaller case.
      ·
        -- The claim for l settles it.
        exact ih hl' hab' hba'
      -- `b = c` heads the list, yet `b` also sits in the tail after `a`.
      ·
        -- Equal lists have equal heads and equal tails: b is c, and the rest is [a].
        simp only [List.cons.injEq] at hr2
        -- Replace c by b everywhere.
        obtain ⟨rfl, rfl⟩ := hr2
        -- b is in the tail (it is in [a, b], which is inside the tail), but the head b is not in
        -- the tail.
        exact hc (hab'.subset (List.mem_cons_of_mem _ List.mem_cons_self))
    -- `a = c` heads the list.
    ·
      -- Equal lists have equal heads and equal tails: a is c, and the rest is [b].
      simp only [List.cons.injEq] at hr
      -- Replace c by a everywhere.
      obtain ⟨rfl, rfl⟩ := hr
      -- The same split for [b, a]: inside the tail, or b is the head too.
      rcases List.sublist_cons_iff.1 hba with hba' | ⟨r', hr2, hr2'⟩
      -- `a` also sits in the tail, after `b`: a repeat.
      ·
        -- a is in the tail (it is in [b, a], which is inside the tail), but the head a is not in
        -- the tail.
        exact hc (hba'.subset (List.mem_cons_of_mem _ List.mem_cons_self))
      -- Then `b = c = a` too, and `b` sits in the tail: a repeat.
      ·
        -- Equal lists have equal heads and equal tails: b is the head as well.
        simp only [List.cons.injEq] at hr2
        -- Replace accordingly.
        obtain ⟨rfl, rfl⟩ := hr2
        -- b is in the tail ([b] is inside it), but the head, which is b, is not in the tail.
        exact hc (hr'.subset List.mem_cons_self)

/-- **FIFO handoff, no barging.** If request `a` entered the line before
request `b`, and `b` has been served, then `a` no longer waits: nobody is
served ahead of an earlier request still in the queue. TLA+: `FifoHandoff`. -/
theorem fifo_served {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) {a b : Nat}
    -- `a` entered the line before `b`.
    (hab : [a, b].Sublist s.enq)
    -- `b` was served.
    (hb : b ∈ s.served) :
    -- The statement: a is not waiting.
    a ∉ s.queue := by
  -- Suppose a waits.
  intro ha
  -- The core facts of the invariant.
  have hc := (reachable_inv h).core
  -- In `served ++ queue` the served `b` precedes the waiting `a`, and that
  -- order carries into the line, against `a` before `b` there.
  exact no_two_orders hc.enq_nodup hab ((served_before_waiting hb ha).trans hc.line)

/-- **Served in order.** The served requests appear in the order they
entered the line. -/
theorem served_in_order {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) :
    -- The statement: the served requests appear, in order, inside the line.
    s.served.Sublist s.enq :=
  -- `served` is inside `served ++ queue`, which is inside the line.
  (List.sublist_append_left _ _).trans (reachable_inv h).core.line

/-- **No stranded waiter.** While anyone waits, no permit is free: a free
permit always goes to the oldest waiter. TLA+: `NoLostWakeup` (Stranded). -/
theorem no_stranded_waiter {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s)
    -- Someone waits.
    (hq : s.queue ≠ []) :
    -- The statement: no permit is free.
    s.free = 0 :=
  -- This is one of the core facts of the invariant.
  (reachable_inv h).core.no_stranded hq

/-- **Handoff before wake.** A woken request already owns its permit, so
the poll the wake causes always finds it. TLA+: `NoLostWakeup`
(GrantedWoken). -/
theorem handed_holds {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) {r : Nat}
    -- `r` was woken.
    (hr : r ∈ s.woken) :
    -- The statement: r was handed its permit.
    r ∈ s.handed :=
  -- This is one of the core facts of the invariant.
  (reachable_inv h).core.woken_handed r hr

/-- **No lost handoff.** A request handed a permit is woken or owed a
wake. TLA+: `NoLostWakeup` (GrantedWoken). -/
theorem no_lost_handoff {n : Nat} {s : Q}
    -- `s` is reachable from the start.
    (h : Steps (init n) s) {r : Nat}
    -- `r` was handed a permit.
    (hr : r ∈ s.handed) :
    -- The statement: r is owed a wake, or was woken.
    r ∈ s.owed ∨ r ∈ s.woken :=
  -- This is one of the core facts of the invariant.
  (reachable_inv h).core.handed_woken r hr

/-- **A release serves the head.** When someone waits, a release hands the
oldest waiter the permit and owes it a wake. -/
theorem release_serves_head {s : Q} {h r : Nat} {t : List Nat}
    -- `h` is the oldest waiter.
    (hq : s.queue = h :: t) :
    -- s' is the state after the release: r leaves the owners and its permit goes through `handoff`.
    let s' := handoff { s with owners := s.owners.erase r }
    -- The statement: h is handed the permit, is owed a wake, is served, and the queue is what was
    -- behind h.
    h ∈ s'.handed ∧ h ∈ s'.owed ∧ h ∈ s'.served ∧ s'.queue = t := by
  -- Unfold `handoff` on the queue h :: t and compute.
  simp [handoff, hq]

/-- **Waiters ahead only leave.** If `r` waits behind the requests `pre`,
then after any step `r` was served, or left the queue (cancelled), or still
waits behind requests `pre'` drawn from `pre`, in order. -/
theorem position_never_grows {n : Nat} {s t : Q}
    -- The invariant holds before the step.
    (hinv : Inv n s)
    -- One step.
    (hstep : Step s t)
    -- The requests ahead of r, those behind it, and r itself.
    {pre post : List Nat} {r : Nat}
    -- `r` waits behind `pre`.
    (hq : s.queue = pre ++ r :: post) :
    -- The statement: r was served, or r left the queue, or r still waits behind a list pre' drawn,
    -- in order, from pre.
    r ∈ t.served ∨ r ∉ t.queue ∨
      ∃ pre' post', t.queue = pre' ++ r :: post' ∧ pre'.Sublist pre := by
  -- What a handoff does to the queue: it serves the head, which is `r`
  -- itself when nobody is ahead, or else one of those ahead.
  have hand : ∀ u : Q, u.queue = pre ++ r :: post →
      r ∈ (handoff u).served ∨ ∃ pre' post', (handoff u).queue = pre' ++ r :: post' ∧
        pre'.Sublist pre := by
    -- Take a state u whose queue is pre ++ r :: post.
    intro u hu
    -- Split on whether anybody is ahead of r.
    cases pre with
    -- Nobody ahead: r is the head, and the handoff serves it.
    | nil => left; simp [handoff, hu]
    -- Somebody ahead: p first, then pre''.
    | cons p pre'' =>
      -- The second outcome: r still waits.
      right
      -- The handoff serves p; r waits behind pre'', which is pre without its first entry.
      exact ⟨pre'', post, by simp [handoff, hu], List.sublist_cons_self p pre''⟩
  -- Split on the kind of step.
  cases hstep with
  -- Case: the fast path.
  | fast x _ hq0 _ =>
    -- Nobody waits on the fast path, yet `r` waits: impossible.
    simp [hq0] at hq
  -- Case: a fresh request x joins the queue.
  | enqueue x _ _ =>
    -- `x` joins behind `r`: nobody new ahead.
    right; right
    -- r waits behind the same pre; x is at the very back.
    exact ⟨pre, post ++ [x], by simp [hq], List.Sublist.refl _⟩
  -- Case: an owner x drops its guard.
  | release x _ =>
    -- The owner's permit comes back through a handoff.
    rcases hand { s with owners := s.owners.erase x } hq with h | h
    -- The handoff served r.
    · exact Or.inl h
    -- r still waits, behind the same requests or fewer.
    · exact Or.inr (Or.inr h)
  -- Case: a wake. The queue does not change.
  | wake x _ =>
    -- The third outcome: r still waits behind the same pre.
    right; right; exact ⟨pre, post, hq, List.Sublist.refl _⟩
  -- Case: a poll. The queue does not change.
  | poll x _ _ =>
    -- The third outcome: r still waits behind the same pre.
    right; right; exact ⟨pre, post, hq, List.Sublist.refl _⟩
  -- Case: a waiting request x is dropped.
  | cancelWaiting x hx =>
    -- The queue has no repeats.
    have hqn : s.queue.Nodup := (List.nodup_append.1 hinv.core.line_nodup).2.1
    -- Split on whether the dropped request is r.
    by_cases hxr : x = r
    -- `r` itself left.
    ·
      -- Replace x by r.
      subst hxr
      -- The second outcome: r is no longer in the queue.
      right; left
      -- The queue held r once, so removing it leaves no r.
      exact List.Nodup.not_mem_erase hqn
    -- x is not r. The third outcome: r still waits.
    · right; right
      -- Split on whether x was ahead of r.
      by_cases hxp : x ∈ pre
      -- One of those ahead left.
      ·
        -- r waits behind pre without x, which is drawn from pre; the shape of the queue is left to
        -- show.
        refine ⟨pre.erase x, post, ?_, List.erase_sublist⟩
        -- Write the queue as pre ++ r :: post.
        simp only [hq]
        -- Removing x, which is in pre, removes it from pre.
        exact List.erase_append_left _ hxp
      -- Someone behind left.
      ·
        -- r waits behind the same pre; the shape of the queue is left to show.
        refine ⟨pre, post.erase x, ?_, List.Sublist.refl _⟩
        -- Write the queue as pre ++ r :: post.
        simp only [hq]
        -- x is not in pre and is not r, so it is removed from post.
        rw [List.erase_append_right _ hxp, List.erase_cons_tail (by simpa using Ne.symm hxr)]
  -- Case: a request x handed a permit is dropped.
  | cancelHanded x _ =>
    -- The dropped waiter's permit comes back through a handoff.
    rcases hand { s with handed := s.handed.erase x, owed := s.owed.filter (· != x),
                         woken := s.woken.filter (· != x) } hq with h | h
    -- The handoff served r.
    · exact Or.inl h
    -- r still waits, behind the same requests or fewer.
    · exact Or.inr (Or.inr h)

/-- **Every handoff advances a waiter.** A permit coming back while `r`
waits behind `pre` serves `r`, or removes one of those ahead of it. So a
waiter that is not cancelled is served within `pre.length + 1` handoffs. -/
theorem handoff_advances {s : Q} {pre post : List Nat} {r : Nat}
    -- `r` waits behind `pre`.
    (hq : s.queue = pre ++ r :: post) :
    -- The statement: r is served, or r still waits behind a strictly shorter list.
    r ∈ (handoff s).served ∨
      ∃ pre' post', (handoff s).queue = pre' ++ r :: post' ∧ pre'.length < pre.length := by
  -- Split on whether anybody is ahead of r.
  cases pre with
  -- Nobody ahead: `r` is the head, and is served.
  | nil => left; simp [handoff, hq]
  -- Someone ahead: the head is served, and one fewer is ahead.
  | cons p pre'' => right; exact ⟨pre'', post, by simp [handoff, hq], by simp⟩

/-! ## The RED cases -/

/-- The barging design: a release frees the permit and wakes the oldest
waiter (which stays queued and must retry), and the fast path ignores the
queue. -/
inductive StepBarge : Q → Q → Prop
  /-- The fast path takes a free permit even while others wait. -/
  | fast (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- A permit is free; nobody checks the queue.
      (hf : 0 < s.free) :
      -- After the step: one free permit fewer, r is an owner,
      StepBarge s { s with free := s.free - 1, owners := r :: s.owners,
                           -- r has entered the line, and r has been served.
                           enq := s.enq ++ [r], served := s.served ++ [r] }
  /-- As `Step.enqueue`. -/
  | enqueue (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- The fast path is shut.
      (hc : s.queue ≠ [] ∨ s.free = 0) :
      -- After the step: r is at the back of the queue and of the line.
      StepBarge s { s with queue := s.queue ++ [r], enq := s.enq ++ [r] }
  /-- The defect: the permit is freed, the head is only woken. -/
  | release (s : Q) (r : Nat) (h : Nat) (t : List Nat)
      -- `r` owns a permit.
      (hr : r ∈ s.owners)
      -- `h` is the oldest waiter.
      (hq : s.queue = h :: t) :
      -- After the step: r is no owner, a permit is free,
      StepBarge s { s with owners := s.owners.erase r, free := s.free + 1,
                           -- and h is woken while it stays in the queue.
                           woken := s.woken ++ [h] }

/-- Request 1 owns the lock; request 2 waits. -/
def bargeWait : Q := ⟨0, [2], [1], [], [], [], [1, 2], [1]⟩
/-- Request 1 released: the lock is free and request 2 is woken but still
queued. -/
def bargeFreed : Q := ⟨1, [2], [], [], [], [2], [1, 2], [1]⟩
/-- Request 3 took the lock on the fast path ahead of request 2. -/
def barged : Q := ⟨0, [2], [3], [], [], [2], [1, 2, 3], [1, 3]⟩

/-- **`barging_breaks_fifo`.** Under the barging design, request 3, which
entered the line after request 2, is served while request 2 still waits:
the FIFO law of `fifo_served` fails. -/
theorem barging_breaks_fifo :
    -- The statement, as four steps and three facts. Step 1: request 1 takes the lock on the fast
    -- path.
    StepBarge (init 1) ⟨0, [], [1], [], [], [], [1], [1]⟩ ∧
    -- Step 2: request 2 finds the lock taken and queues.
    StepBarge ⟨0, [], [1], [], [], [], [1], [1]⟩ bargeWait ∧
    -- Step 3: request 1 releases; the lock is free and request 2 is only woken.
    StepBarge bargeWait bargeFreed ∧
    -- Step 4: request 3 takes the free lock on the fast path.
    StepBarge bargeFreed barged ∧
    -- The facts: 2 entered the line before 3, 3 is served, and 2 still waits.
    [2, 3].Sublist barged.enq ∧ 3 ∈ barged.served ∧ 2 ∈ barged.queue := by
  -- Request 1 takes the lock, 2 queues, 1 releases (freeing the lock), and
  -- 3 takes it on the fast path; each fact is checked by computation.
  refine ⟨StepBarge.fast (init 1) 1 (by decide) (by decide),
    StepBarge.enqueue _ 2 (by decide) (by decide),
    StepBarge.release bargeWait 1 2 [] (by decide) (by decide),
    StepBarge.fast bargeFreed 3 (by decide) (by decide),
    by decide, by decide, by decide⟩

/-- The wake-before-handoff design: a release wakes the oldest waiter
first and hands it the permit in a later step; a woken waiter that finds no
permit registers its waker again. -/
inductive StepWake : Q → Q → Prop
  /-- As `Step.fast`. -/
  | fast (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- Nobody waits.
      (hq : s.queue = [])
      -- A permit is free.
      (hf : 0 < s.free) :
      -- After the step: one free permit fewer, r is an owner,
      StepWake s { s with free := s.free - 1, owners := r :: s.owners,
                          -- r has entered the line, and r has been served.
                          enq := s.enq ++ [r], served := s.served ++ [r] }
  /-- As `Step.enqueue`. -/
  | enqueue (s : Q) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- The fast path is shut.
      (hc : s.queue ≠ [] ∨ s.free = 0) :
      -- After the step: r is at the back of the queue and of the line.
      StepWake s { s with queue := s.queue ++ [r], enq := s.enq ++ [r] }
  /-- The defect: the owner's guard drops and the oldest waiter is woken;
  the permit is in transit, not yet handed. -/
  | release (s : Q) (r h : Nat) (t : List Nat)
      -- `r` owns a permit.
      (hr : r ∈ s.owners)
      -- `h` is the oldest waiter.
      (hq : s.queue = h :: t) :
      -- After the step: r is no owner and h is woken; no permit is free and none is handed yet.
      StepWake s { s with owners := s.owners.erase r, woken := s.woken ++ [h] }
  /-- A permit in transit reaches the oldest waiter, with no wake. (The
  step is not tied to an earlier release; that only gives this defective
  design more behaviours, and the stranding below holds in all of them.) -/
  | grant (s : Q) (h : Nat) (t : List Nat)
      -- `h` is the oldest waiter.
      (hq : s.queue = h :: t) :
      -- After the step: h leaves the queue, holds the permit, and counts as served.
      StepWake s { s with queue := t, handed := s.handed ++ [h], served := s.served ++ [h] }
  /-- A woken waiter is polled, finds no permit, and registers its waker
  again: it is no longer woken. -/
  | repark (s : Q) (r : Nat)
      -- `r` was woken ...
      (hw : r ∈ s.woken)
      -- ... but holds no permit.
      (hh : r ∉ s.handed) :
      -- After the step: r is no longer woken.
      StepWake s { s with woken := s.woken.filter (· != r) }
  /-- As `Step.wake`. -/
  | wake (s : Q) (r : Nat)
      -- `r` is owed a wake.
      (hr : r ∈ s.owed) :
      -- After the step: r is no longer owed a wake, and r is woken.
      StepWake s { s with owed := s.owed.filter (· != r), woken := s.woken ++ [r] }
  /-- As `Step.poll`. -/
  | poll (s : Q) (r : Nat)
      -- `r` was woken ...
      (hw : r ∈ s.woken)
      -- ... and holds its permit.
      (hh : r ∈ s.handed) :
      -- After the step: r is no longer woken or handed,
      StepWake s { s with woken := s.woken.filter (· != r), handed := s.handed.erase r,
                          -- and r is an owner.
                          owners := r :: s.owners }
  /-- As `Step.cancelWaiting`. -/
  | cancelWaiting (s : Q) (r : Nat)
      -- `r` waits.
      (hr : r ∈ s.queue) :
      -- After the step: r has left the queue.
      StepWake s { s with queue := s.queue.erase r }

/-- Any number of `StepWake` steps. -/
inductive StepsWake : Q → Q → Prop
  /-- No step at all. -/
  | refl (s : Q) : StepsWake s s
  /-- Some steps, then one more. -/
  | tail {s t u : Q} : StepsWake s t → StepWake t u → StepsWake s u

/-- Request 1 owns the lock; request 2 waits. -/
def wakeWait : Q := ⟨0, [2], [1], [], [], [], [1, 2], [1]⟩
/-- Request 1 released; request 2 is woken, the permit in transit. -/
def wakeEarly : Q := ⟨0, [2], [], [], [], [2], [1, 2], [1]⟩
/-- Request 2 was polled, found nothing, and registered its waker again. -/
def wakeReparked : Q := ⟨0, [2], [], [], [], [], [1, 2], [1]⟩
/-- The permit reaches request 2, with nobody left to wake it. -/
def wakeLost : Q := ⟨0, [], [], [2], [], [], [1, 2], [1, 2]⟩

/-- Request 2 is stuck: it holds the permit, left the queue, is not woken,
is owed no wake and is not an owner. -/
abbrev Stuck2 (s : Q) : Prop :=
  -- In the line, not queued, not woken, owed no wake, and no owner.
  2 ∈ s.enq ∧ 2 ∉ s.queue ∧ 2 ∉ s.woken ∧ 2 ∉ s.owed ∧ 2 ∉ s.owners

/-- Under the wake-before-handoff design nothing ever wakes a stuck request
2: wakes only ever go to the head of the queue or to owed requests. -/
theorem stuck2_step {s t : Q}
    -- Request 2 is stuck before the step.
    (hs : Stuck2 s)
    -- One step of the wake-before-handoff design.
    (hstep : StepWake s t) : Stuck2 t := by
  -- Take the five facts apart.
  obtain ⟨he, hq, hw, ho, hown⟩ := hs
  -- Split on the kind of step.
  cases hstep with
  -- Case: the fast path, for a fresh request r.
  | fast r hr _ _ =>
    -- The fresh request is not 2, which entered the line already.
    have : r ≠ 2 := fun h => hr (h ▸ he)
    -- 2 stays in the line; queue, woken and owed are unchanged; "2 is no owner" is left.
    refine ⟨List.mem_append.2 (Or.inl he), hq, hw, ho, ?_⟩
    -- The owners are now r followed by the old ones: 2 must be neither.
    simp only [List.mem_cons, not_or]
    -- 2 is not r, and 2 was no owner.
    exact ⟨Ne.symm this, hown⟩
  -- Case: a fresh request r joins the queue.
  | enqueue r hr _ =>
    -- The fresh request is not 2, which entered the line already.
    have : r ≠ 2 := fun h => hr (h ▸ he)
    -- 2 stays in the line; "2 is not queued" is left; the rest is unchanged.
    refine ⟨List.mem_append.2 (Or.inl he), ?_, hw, ho, hown⟩
    -- The queue is now the old one followed by r: 2 must be in neither.
    simp only [List.mem_append, List.mem_singleton, not_or]
    -- 2 was not queued, and 2 is not r.
    exact ⟨hq, Ne.symm this⟩
  -- Case: an owner r releases and the head h is woken.
  | release r h t _ hq0 =>
    -- The woken head is not 2, which is not queued.
    have : h ≠ 2 := fun e => hq (by rw [hq0, e]; exact List.mem_cons_self)
    -- Line, queue and owed are unchanged; "2 is not woken" is left; the owners only shrink.
    refine ⟨he, hq, ?_, ho, fun hm => hown (List.mem_of_mem_erase hm)⟩
    -- The woken list is now the old one followed by h: 2 must be in neither.
    simp only [List.mem_append, List.mem_singleton, not_or]
    -- 2 was not woken, and 2 is not h.
    exact ⟨hw, Ne.symm this⟩
  -- Case: the permit reaches the head h.
  | grant h t hq0 =>
    -- The queue shrinks to what was behind h, so 2 is still not in it; the rest is unchanged.
    refine ⟨he, fun hm => hq (by rw [hq0]; exact List.mem_cons_of_mem _ hm), hw, ho, hown⟩
  -- Case: a woken request goes back to sleep.
  | repark r _ _ =>
    -- Fewer woken requests: 2 is still not among them.
    exact ⟨he, hq, fun hm => hw (mem_of_mem_filter hm), ho, hown⟩
  -- Case: a request r owed a wake is woken.
  | wake r hr =>
    -- The woken request was owed, so it is not 2.
    have : r ≠ 2 := fun e => ho (e ▸ hr)
    -- "2 is not woken" is left; fewer requests are owed a wake; the rest is unchanged.
    refine ⟨he, hq, ?_, fun hm => ho (mem_of_mem_filter hm), hown⟩
    -- The woken list is now the old one followed by r: 2 must be in neither.
    simp only [List.mem_append, List.mem_singleton, not_or]
    -- 2 was not woken, and 2 is not r.
    exact ⟨hw, Ne.symm this⟩
  -- Case: a woken request r is polled and becomes an owner.
  | poll r hwr _ =>
    -- The polled request was woken, so it is not 2.
    have : r ≠ 2 := fun e => hw (e ▸ hwr)
    -- Fewer woken requests; "2 is no owner" is left; the rest is unchanged.
    refine ⟨he, hq, fun hm => hw (mem_of_mem_filter hm), ho, ?_⟩
    -- The owners are now r followed by the old ones: 2 must be neither.
    simp only [List.mem_cons, not_or]
    -- 2 is not r, and 2 was no owner.
    exact ⟨Ne.symm this, hown⟩
  -- Case: a waiting request is dropped.
  | cancelWaiting r _ =>
    -- A smaller queue: 2 is still not in it. The rest is unchanged.
    exact ⟨he, fun hm => hq (List.mem_of_mem_erase hm), hw, ho, hown⟩

/-- **`wake_before_handoff_strands`.** Under the wake-before-handoff
design, request 2 is woken before the permit reaches it, polled in between,
and then handed the permit with no wake: it holds the permit and, whatever
steps follow, is never woken and never becomes an owner. The design's
`no_lost_handoff` fails. -/
theorem wake_before_handoff_strands :
    -- The statement. Step 1: request 1 releases and request 2 is woken. Step 2: request 2 is
    -- polled, finds nothing, and sleeps again.
    StepWake wakeWait wakeEarly ∧ StepWake wakeEarly wakeReparked ∧
    -- Step 3: the permit reaches request 2.
    StepWake wakeReparked wakeLost ∧
    -- Then 2 holds the permit, is owed no wake, and is not woken;
    2 ∈ wakeLost.handed ∧ 2 ∉ wakeLost.owed ∧ 2 ∉ wakeLost.woken ∧
    -- and in every later state 2 is not woken and is no owner.
    ∀ t, StepsWake wakeLost t → 2 ∉ t.woken ∧ 2 ∉ t.owners := by
  -- The three steps and the three facts are checked by computation; the "every later state" part is
  -- left.
  refine ⟨StepWake.release wakeWait 1 2 [] (by decide) (by decide),
    StepWake.repark wakeEarly 2 (by decide) (by decide),
    StepWake.grant wakeReparked 2 [] (by decide),
    by decide, by decide, by decide, ?_⟩
  -- Take a state t reached from wakeLost.
  intro t ht
  -- Stuck at the start, stuck after every step.
  have : Stuck2 t := by
    -- Go through the steps one at a time.
    induction ht with
    -- No step: wakeLost itself is stuck, by computation.
    | refl => exact ⟨by decide, by decide, by decide, by decide, by decide⟩
    -- One more step: stuck before it, so stuck after it.
    | tail _ hstep ih => exact stuck2_step ih hstep
  -- Read off the third and the fifth fact of "stuck": not woken, and no owner.
  exact ⟨this.2.2.1, this.2.2.2.2⟩

/-- The design, plus the defect: a waiter dropped after it was handed a
permit leaves without passing the permit on. -/
inductive StepNoPass : Q → Q → Prop
  /-- Every step of the design. -/
  | step {s t : Q}
      -- A step of the design.
      (h : Step s t) : StepNoPass s t
  /-- The defect: the handed permit leaves with the dropped waiter. -/
  | cancelDrop (s : Q) (r : Nat)
      -- `r` was handed a permit.
      (hr : r ∈ s.handed) :
      -- After the step: r is no longer handed, owed a wake,
      StepNoPass s { s with handed := s.handed.erase r, owed := s.owed.filter (· != r),
                            -- or woken; its permit is passed to nobody.
                            woken := s.woken.filter (· != r) }

/-- Request 1 owns the lock; requests 2 and 3 wait. -/
def passWait : Q := ⟨0, [2, 3], [1], [], [], [], [1, 2, 3], [1]⟩
/-- Request 1 released: request 2 is handed the lock and owed a wake. -/
def passHanded : Q := ⟨0, [3], [], [2], [2], [], [1, 2, 3], [1, 2]⟩
/-- Request 2 was dropped and kept the lock: nobody holds it, nothing is
free, and request 3 waits. -/
def passLost : Q := ⟨0, [3], [], [], [], [], [1, 2, 3], [1, 2]⟩

/-- Nobody holds, nothing is free or owed, and the served list is fixed. -/
abbrev Dead (s : Q) : Prop :=
  -- No free permit, no owner, nobody handed, owed or woken, and exactly 1 and 2 served.
  s.free = 0 ∧ s.owners = [] ∧ s.handed = [] ∧ s.owed = [] ∧ s.woken = [] ∧ s.served = [1, 2]

/-- No step of the design leaves a dead state: there is no permit to hand
to anyone. -/
theorem dead_step {s t : Q}
    -- The state is dead before the step.
    (hd : Dead s)
    -- One step of the design.
    (hstep : Step s t) : Dead t := by
  -- Take the six facts apart.
  obtain ⟨hf, ho, hh, hw, hk, hs⟩ := hd
  -- Split on the kind of step.
  cases hstep with
  -- Each step that would serve anyone needs a free permit or a holder.
  | fast _ _ _ hf' => omega
  -- A request joining the queue changes none of the six.
  | enqueue => exact ⟨hf, ho, hh, hw, hk, hs⟩
  -- A release needs an owner: there is none.
  | release r hr => simp [ho] at hr
  -- A wake needs a request owed one: there is none.
  | wake r hr => simp [hw] at hr
  -- A poll needs a woken request: there is none.
  | poll r hwr _ => simp [hk] at hwr
  -- A waiter leaving the queue changes none of the six.
  | cancelWaiting => exact ⟨hf, ho, hh, hw, hk, hs⟩
  -- Dropping a handed request needs one: there is none.
  | cancelHanded r hr => simp [hh] at hr

/-- **`cancel_without_pass_on_strands`.** When request 2 is dropped after
being handed the lock and does not pass it on, the lock is lost (the count
of `Inv` fails: nothing free, nobody holding), and request 3 is never
served, whatever steps of the design follow. -/
theorem cancel_without_pass_on_strands :
    -- The statement. Step 1: request 1 releases and request 2 is handed the lock. Step 2: request 2
    -- is dropped and keeps it.
    StepNoPass passWait passHanded ∧ StepNoPass passHanded passLost ∧
    -- Then free + owners + handed is 0, where the invariant says 1;
    passLost.free + passLost.owners.length + passLost.handed.length = 0 ∧
    -- and in every later state request 3 is not served.
    ∀ t, Steps passLost t → 3 ∉ t.served := by
  -- The two steps and the count are checked by computation; the "every later state" part is left.
  refine ⟨StepNoPass.step (Step.release passWait 1 (by decide)),
    StepNoPass.cancelDrop passHanded 2 (by decide), by decide, ?_⟩
  -- Take a state t reached from passLost by steps of the design.
  intro t ht
  -- Dead at the start, dead after every step; 3 is not among [1, 2].
  have : Dead t := by
    -- Go through the steps one at a time.
    induction ht with
    -- No step: passLost itself is dead, field by field.
    | refl => exact ⟨rfl, rfl, rfl, rfl, rfl, rfl⟩
    -- One more step: dead before it, so dead after it.
    | tail _ hstep ih => exact dead_step ih hstep
  -- The served list of t is exactly [1, 2].
  rw [this.2.2.2.2.2]
  -- 3 is not in [1, 2], by computation.
  decide

-- End of the namespace opened at the top.
end Regolith.SyncFifo
