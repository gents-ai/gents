import Proofs.Shared.Basic

/-! What a background `agent_new` or `agent_message` call returns to its
caller. The call caused one request in the delegate's session; when that
request starts its own helpers in the background and ends its turn, the answer
the caller asked for comes later, from the turn their results wake. -/

namespace LineageReturn

/-- One request of the lineage a session-message call caused, as the call's
settler observes it: whether the request is terminal, read first, then whether
a session-message call it made still owes its result (running, or settled with
its completion not yet delivered). `rank` is the request's place in the order
the delegate's session ran the lineage: the caused request first, then each
wake that delivered a lineage call's result. A wake joins the lineage through
the completion it delivers, never through a parent label, so a Goal turn or a
person's message in the same session is not part of it. -/
structure Node where
  terminal : Bool
  owed : Bool
  rank : Nat
  deriving DecidableEq, Repr

/-- A lineage request is quiet once it is terminal and nothing it delegated
still owes its result. External premises: a request's tool calls are created
before its terminal commits, so a settler that reads the terminal before the
calls sees every call; and a call stops owing its result only in a transaction
after the one that commits the wake carrying it, so a quiet call's wake is
already observable. -/
def quiet (terminal owed : Bool) : Bool := terminal && !owed

inductive Decision where
  | wait
  | answer (rank : Nat)
  deriving DecidableEq, Repr

/-- Decision (gents#4191): a session-message call settles once its lineage is
quiet, with the answer of the lineage request that ran last.

The reply route is the caller's own call: a delegate answers only the call
whose lineage its turn belongs to, through that call's settlement, and the
caller's runtime writes the result into the caller's session as its own
completion. A delegate never needs, and is never given, a way to message the
session that started it (`DurableLineage.sessionTargetAllowed`). A delegate
that delegates in the background and ends its turn therefore answers after
its helpers report, not with the turn that started them; with no nested call
the caused request answers, as before. A kill ends the call at once: it
settles only a quiet lineage and otherwise cancels the row. -/
def decide (nodes : List Node) : Decision :=
  if nodes.all (fun node => quiet node.terminal node.owed) then
    .answer (nodes.foldl (fun best node => max best node.rank) 0)
  else .wait

theorem live_request_waits (nodes : List Node) (node : Node) (h : node ∈ nodes)
    (live : node.terminal = false) : decide nodes = .wait := by
  have : nodes.all (fun node => quiet node.terminal node.owed) = false := by
    rw [List.all_eq_false]
    exact ⟨node, h, by simp [quiet, live]⟩
  simp [decide, this]

theorem owed_call_waits (nodes : List Node) (node : Node) (h : node ∈ nodes)
    (owed : node.owed = true) : decide nodes = .wait := by
  have : nodes.all (fun node => quiet node.terminal node.owed) = false := by
    rw [List.all_eq_false]
    exact ⟨node, h, by simp [quiet, owed]⟩
  simp [decide, this]

theorem answer_needs_a_quiet_lineage (nodes : List Node) (rank : Nat)
    (h : decide nodes = .answer rank) :
    ∀ node ∈ nodes, node.terminal = true ∧ node.owed = false := by
  intro node member
  unfold decide at h
  split at h
  · rename_i all
    have := List.all_eq_true.mp all node member
    simp [quiet] at this
    exact this
  · cases h

theorem fold_rank_bounds (nodes : List Node) (start : Nat) :
    start ≤ nodes.foldl (fun best node => max best node.rank) start ∧
      ∀ node ∈ nodes, node.rank ≤ nodes.foldl (fun best node => max best node.rank) start := by
  induction nodes generalizing start with
  | nil => simp
  | cons head rest ih =>
    obtain ⟨hstart, hrest⟩ := ih (max start head.rank)
    refine ⟨Nat.le_trans (Nat.le_max_left _ _) hstart, ?_⟩
    intro node member
    simp only [List.mem_cons] at member
    rcases member with rfl | member
    · exact Nat.le_trans (Nat.le_max_right _ _) hstart
    · exact hrest node member

theorem answer_is_the_last_to_run (nodes : List Node) (rank : Nat)
    (h : decide nodes = .answer rank) : ∀ node ∈ nodes, node.rank ≤ rank := by
  unfold decide at h
  split at h
  · cases h
    exact (fold_rank_bounds nodes 0).2
  · cases h

theorem caused_request_alone_answers_itself :
    decide [⟨true, false, 0⟩] = .answer 0 := rfl

theorem delegate_that_started_a_helper_waits_for_it :
    decide [⟨true, true, 0⟩] = .wait := rfl

theorem nested_result_answers_from_its_wake :
    decide [⟨true, false, 0⟩, ⟨true, false, 1⟩] = .answer 1 := rfl

theorem pending_wake_waits :
    decide [⟨true, false, 0⟩, ⟨false, false, 1⟩] = .wait := rfl

end LineageReturn
