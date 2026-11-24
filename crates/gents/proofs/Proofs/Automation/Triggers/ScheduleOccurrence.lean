import Proofs.ToolCalls.Triggers.Identity
import Proofs.Documents.ScheduleCadence
import Proofs.Store.TransactionFence

/-! One request per schedule occurrence.

A schedule tick fires the occurrence its Trigger's cursor (`next_run_at`) names
when that occurrence is due. The cursor is advanced by a write that runs after
the fire's dispatch, in its own task: it can lag past the next tick, fail, or be
lost with the process, and the next tick then offers the same occurrence again.
The cursor is therefore an optimization that keeps a tick from offering an
occurrence it already fired (`ScheduleCadence.cursor_fires_each_occurrence_once`
holds only when each fire's cursor write lands before the next tick reads); it is
not what makes an occurrence fire once.

What does is the occurrence's identity. A fire's request takes its request id,
session id and `retry_key` from (owner, trigger, occurrence) alone
(`identity`), the same derivation as a targeted schedule's receipt
(`Triggers.Durable.Identity` over the collection `Schedule` and the occurrence's
fire key). Materialization reads the request by its `retry_key`, an equality on a
unique key the store validates like a read by id (`TransactionFence.Read.byUniqueKey`),
and returns the request it finds; otherwise it inserts the request, and
`AgentRequest.retry_key` is a unique index, so of two concurrent first
dispatches of one occurrence the second commit aborts and its rebuilt read finds
the first (`concurrent_dispatches_of_one_occurrence_are_fenced`). Whatever the
cursor does, a re-dispatched occurrence answers with its one request
(`no_occurrence_fires_twice`), and two occurrences never share a request
(`different_occurrences_never_collide`).

External premise: the occurrence is the cursor value the tick found due, as
persisted; the source writes every advanced value in one RFC 3339 rendering
(seconds, `Z`), so distinct instants are distinct strings. -/

namespace Triggers.ScheduleOccurrence

open Triggers.Durable

/-- `durable_fire_key("schedule", [trigger, occurrence])`. -/
def occurrenceKey (trigger occurrence : String) : String :=
  "8:schedule:" ++ (component trigger ++ (":" ++ component occurrence))

def identity (owner trigger occurrence : String) : Identity :=
  ⟨owner, trigger, "Schedule", occurrenceKey trigger occurrence⟩

def retryKey (owner trigger occurrence : String) : String :=
  (identity owner trigger occurrence).key

def requestId (owner trigger occurrence : String) : String :=
  (identity owner trigger occurrence).requestId

def sessionId (owner trigger occurrence : String) : String :=
  (identity owner trigger occurrence).sessionId

theorem occurrenceKey_injective (t t' o o' : String)
    (h : occurrenceKey t o = occurrenceKey t' o') : t = t' ∧ o = o' := by
  have hd := congrArg String.data h
  simp only [occurrenceKey, String.data_append] at hd
  have h1 : component t ++ (":" ++ component o) = component t' ++ (":" ++ component o') := by
    apply String.ext
    simp only [String.data_append]
    exact List.append_cancel_left hd
  obtain ⟨ht, h2⟩ := component_append_injective _ _ _ _ h1
  have h3 : component o = component o' := by
    apply String.ext
    have h4 := congrArg String.data h2
    simp only [String.data_append] at h4
    exact List.append_cancel_left h4
  exact ⟨ht, (component_append_injective o o' "" "" (by simpa using h3)).1⟩

theorem identity_injective {o t a o' t' a' : String}
    (h : identity o t a = identity o' t' a') : o = o' ∧ t = t' ∧ a = a' := by
  simp only [identity, Identity.mk.injEq] at h
  obtain ⟨ho, ht, -, hk⟩ := h
  exact ⟨ho, ht, (occurrenceKey_injective _ _ _ _ hk).2⟩

theorem retryKey_injective {o t a o' t' a' : String}
    (h : retryKey o t a = retryKey o' t' a') : o = o' ∧ t = t' ∧ a = a' :=
  identity_injective (Identity.key_injective h)

theorem requestId_injective {o t a o' t' a' : String}
    (h : requestId o t a = requestId o' t' a') : o = o' ∧ t = t' ∧ a = a' :=
  identity_injective (Identity.requestId_injective h)

/-- Two occurrences of one Trigger never share a request, a session or a
retry key. -/
theorem different_occurrences_never_collide (owner trigger a b : String) (h : a ≠ b) :
    retryKey owner trigger a ≠ retryKey owner trigger b ∧
      requestId owner trigger a ≠ requestId owner trigger b ∧
      sessionId owner trigger a ≠ sessionId owner trigger b :=
  ⟨fun e => h (retryKey_injective e).2.2, fun e => h (requestId_injective e).2.2,
    fun e => h (identity_injective (Identity.sessionId_injective e)).2.2⟩

/-- A persisted request, by the unique key it was found by and its id. -/
structure Row where
  retryKey : String
  requestId : String
  deriving DecidableEq, Repr

abbrev Store := List Row

inductive Outcome where
  | published (requestId : String)
  | replayed (requestId : String)
  deriving DecidableEq, Repr

def Outcome.requestId : Outcome → String
  | .published id => id
  | .replayed id => id

def Outcome.isReplay : Outcome → Bool
  | .published _ => false
  | .replayed _ => true

/-- One dispatch of an occurrence: the request its retry key finds, or a new
one under the occurrence's identity. -/
def materialize (store : Store) (owner trigger occurrence : String) : Store × Outcome :=
  match store.find? (fun row => row.retryKey == retryKey owner trigger occurrence) with
  | some row => (store, .replayed row.requestId)
  | none =>
      (⟨retryKey owner trigger occurrence, requestId owner trigger occurrence⟩ :: store,
        .published (requestId owner trigger occurrence))

/-- The dispatches a run makes, in order: any sequence of occurrences, each as
often as ticks, lagging or failed cursor writes and restarts offer it. -/
def dispatchOutcomes (owner trigger : String) : Store → List String → Store × List Outcome
  | store, [] => (store, [])
  | store, occurrence :: rest =>
      let step := materialize store owner trigger occurrence
      let after := dispatchOutcomes owner trigger step.1 rest
      (after.1, step.2 :: after.2)

theorem materialize_keeps_keys_unique (store : Store) (owner trigger occurrence : String)
    (h : (store.map Row.retryKey).Nodup) :
    ((materialize store owner trigger occurrence).1.map Row.retryKey).Nodup := by
  unfold materialize
  split
  · exact h
  · rename_i hfind
    rw [List.map_cons, List.nodup_cons]
    refine ⟨?_, h⟩
    intro hmem
    obtain ⟨r, hr, heq⟩ := List.mem_map.mp hmem
    have hnone := List.find?_eq_none.mp hfind r hr
    simp only [beq_iff_eq] at hnone
    exact hnone heq

theorem materialize_keeps_rows (store : Store) (owner trigger occurrence : String)
    (row : Row) (h : row ∈ store) : row ∈ (materialize store owner trigger occurrence).1 := by
  unfold materialize
  split
  · exact h
  · exact List.mem_cons_of_mem _ h

theorem materialize_holds_the_occurrence (store : Store) (owner trigger occurrence : String) :
    ∃ row ∈ (materialize store owner trigger occurrence).1,
      row.retryKey = retryKey owner trigger occurrence := by
  unfold materialize
  split
  · rename_i row hfind
    refine ⟨row, List.mem_of_find?_eq_some hfind, ?_⟩
    have hp := List.find?_some hfind
    simpa using hp
  · exact ⟨_, List.mem_cons_self .., rfl⟩

theorem dispatchOutcomes_keeps_keys_unique (owner trigger : String) :
    ∀ (dispatches : List String) (store : Store), (store.map Row.retryKey).Nodup →
      ((dispatchOutcomes owner trigger store dispatches).1.map Row.retryKey).Nodup := by
  intro dispatches
  induction dispatches with
  | nil => intro store h; exact h
  | cons occurrence rest ih =>
    intro store h
    exact ih _ (materialize_keeps_keys_unique store owner trigger occurrence h)

theorem dispatchOutcomes_keeps_rows (owner trigger : String) :
    ∀ (dispatches : List String) (store : Store) (row : Row), row ∈ store →
      row ∈ (dispatchOutcomes owner trigger store dispatches).1 := by
  intro dispatches
  induction dispatches with
  | nil => intro store row h; exact h
  | cons occurrence rest ih =>
    intro store row h
    exact ih _ row (materialize_keeps_rows store owner trigger occurrence row h)

theorem dispatchOutcomes_holds_every_dispatch (owner trigger : String) :
    ∀ (dispatches : List String) (store : Store), ∀ occurrence ∈ dispatches,
      ∃ row ∈ (dispatchOutcomes owner trigger store dispatches).1,
        row.retryKey = retryKey owner trigger occurrence := by
  intro dispatches
  induction dispatches with
  | nil => intro store occurrence h; simp at h
  | cons first rest ih =>
    intro store occurrence h
    rcases List.mem_cons.mp h with h | h
    · subst h
      obtain ⟨row, hrow, hkey⟩ := materialize_holds_the_occurrence store owner trigger occurrence
      exact ⟨row, dispatchOutcomes_keeps_rows owner trigger rest _ row hrow, hkey⟩
    · exact ih _ occurrence h

/-- No occurrence fires twice, whatever the cursor does: after any sequence of
dispatches, repeated occurrences included, the requests' retry keys are
distinct (so each occurrence holds at most one request, by
`retryKey_injective`) and every dispatched occurrence holds its request. -/
theorem no_occurrence_fires_twice (owner trigger : String) (dispatches : List String) :
    ((dispatchOutcomes owner trigger [] dispatches).1.map Row.retryKey).Nodup ∧
      ∀ occurrence ∈ dispatches, ∃ row ∈ (dispatchOutcomes owner trigger [] dispatches).1,
        row.retryKey = retryKey owner trigger occurrence :=
  ⟨dispatchOutcomes_keeps_keys_unique owner trigger dispatches [] List.nodup_nil,
    dispatchOutcomes_holds_every_dispatch owner trigger dispatches []⟩

/-- A first dispatch that finds no request inserts the occurrence's retry key
under the unique index; two of them running at once are fenced, so the second
commit aborts and its rebuilt read finds the first's request (`materialize`). -/
def publishFootprint (doc : TransactionFence.Doc) (key : TransactionFence.UniqueValue) :
    TransactionFence.Footprint :=
  TransactionFence.arrival doc [key]

theorem concurrent_dispatches_of_one_occurrence_are_fenced (first second : TransactionFence.Doc)
    (key : TransactionFence.UniqueValue) :
    TransactionFence.fencedAgainst (publishFootprint second key) (publishFootprint first key) =
      true :=
  TransactionFence.a_unique_index_fences_twin_arrivals second first key

/-- Catch-up after an outage cannot collide: the occurrences a cadence offers
from one cursor are distinct (`ScheduleCadence.cursor_fires_each_occurrence_once`), and
distinct occurrences take distinct retry keys. An interval cadence offers every
missed occurrence (`late_interval_ticks_catch_up_every_occurrence`); a cron
cadence offers the due one and moves past the tick
(`late_latest_only_tick_skips_missed_occurrences`). -/
theorem offered_occurrences_never_collide (owner trigger : String) (render : Nat → String)
    (render_injective : Function.Injective render) (next : Nat → Nat)
    (policy : ScheduleCadence.MissedRunPolicy) (increasing : ∀ t, t < next t)
    (ticks : List Nat) (due : Nat) :
    ((ScheduleCadence.fires next policy due ticks).map
      (fun t => retryKey owner trigger (render t))).Nodup :=
  List.Nodup.map (f := fun t => retryKey owner trigger (render t))
    (by intro a b h; exact render_injective (retryKey_injective h).2.2)
    (ScheduleCadence.cursor_fires_each_occurrence_once next policy increasing ticks due)

end Triggers.ScheduleOccurrence
