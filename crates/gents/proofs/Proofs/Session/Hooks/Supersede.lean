import Proofs.Session.Hooks.Claim

namespace Hooks

/-- One pending request core supersedes, with the request that supersedes it
and the reason core records. -/
structure StaleRequest where
  doc : String
  request : String
  successor : String
  successorDoc : String
  reason : String
  deriving DecidableEq, Repr

/-- A session request row as core reads it for a supersede selector: its
lifecycle, whether it is a Goal continuation (trigger kind `goal`), its
trigger id and the signed Goal assignment reference on its input
(`goal_id`, `root_request_doc_id`). -/
structure PendingRow where
  doc : String
  request : String
  pending : Bool
  goalContinuation : Bool
  trigger : Option String
  reference : Option (String × Option String)
  deriving DecidableEq, Repr

/-- Which pending continuations a new Goal assignment keeps: the assignment's
Goal and root, and the successor and reason core records on the others. -/
structure Selector where
  goal : String
  root : String
  successor : String
  successorDoc : String
  reason : String
  deriving DecidableEq, Repr

/-- A member of the assignment, decided from the row's own signed fields: the
root itself, or a continuation of the Goal whose signed reference names the
Goal and that root. -/
def Selector.member (selector : Selector) (row : PendingRow) : Bool :=
  row.doc == selector.root ||
    (row.trigger == some selector.goal &&
      match row.reference with
      | some (goal, root) => goal == selector.goal && root.getD row.doc == selector.root
      | none => false)

def Selector.matches (selector : Selector) (row : PendingRow) : Bool :=
  row.pending && row.goalContinuation && !selector.member row

def Selector.entry (selector : Selector) (row : PendingRow) : StaleRequest :=
  ⟨row.doc, row.request, selector.successor, selector.successorDoc, selector.reason⟩

/-- The retirements a selector names on the session's rows at the snapshot. -/
def Selector.stale (selector : Selector) (rows : List PendingRow) : List StaleRequest :=
  (rows.filter selector.matches).map selector.entry

/-- `request.pending.supersede`: core reads the session's request rows at the
snapshot and supersedes, in order, every pending Goal continuation the
selector does not keep, through `retire`, the pending-supersede path
`request.claim.gate`'s `retire` verdict uses (with its
`request.terminal.stage`).

Decision 2026-11-11, maintainer-approved: AgentRequest rows are core state, so
retiring the pending continuations a new Task Goal assignment makes stale is a
core operation the Goals plugin requests with a selector as data, not a write
it performs. The plugin names the assignment, not the rows, so the number of
rows retired is not bounded by the plugin's effect bound. -/
def supersedeMatching {Core : Type} (retire : StaleRequest → Core → Option Core)
    (selector : Selector) (rows : List PendingRow) (core : Core) : Option Core :=
  (selector.stale rows).foldlM (fun current entry => retire entry current) core

/-- The chain of pending retirements performed. -/
inductive Retired {Core : Type} (retire : StaleRequest → Core → Option Core) :
    List StaleRequest → Core → Core → Prop where
  | nil (core : Core) : Retired retire [] core core
  | cons {first mid last : Core} (entry : StaleRequest) (rest : List StaleRequest)
      (step : retire entry first = some mid) (tail : Retired retire rest mid last) :
      Retired retire (entry :: rest) first last

variable {Core : Type}

private theorem fold_retired (retire : StaleRequest → Core → Option Core)
    (stale : List StaleRequest) (core next : Core)
    (performed : stale.foldlM (fun current entry => retire entry current) core = some next) :
    Retired retire stale core next := by
  induction stale generalizing core with
  | nil =>
    simp only [List.foldlM_nil, pure, Option.some.injEq] at performed
    subst performed
    exact .nil core
  | cons entry rest ih =>
    simp only [List.foldlM_cons, bind, Option.bind] at performed
    cases step : retire entry core with
    | none => simp [step] at performed
    | some mid =>
      simp only [step] at performed
      exact .cons entry rest step (ih mid performed)

/-- Core state moves only through `retire`, once per selected row, in row
order. -/
theorem superseded_requests_are_pending_retires (retire : StaleRequest → Core → Option Core)
    (selector : Selector) (rows : List PendingRow) (core next : Core)
    (performed : supersedeMatching retire selector rows core = some next) :
    Retired retire (selector.stale rows) core next :=
  fold_retired retire (selector.stale rows) core next performed

/-- Every pending Goal continuation at the snapshot that is not a member of
the assignment is retired. -/
theorem every_stale_continuation_is_retired (selector : Selector) (rows : List PendingRow)
    (row : PendingRow) (present : row ∈ rows) (pending : row.pending = true)
    (continuation : row.goalContinuation = true) (outside : selector.member row = false) :
    selector.entry row ∈ selector.stale rows := by
  apply List.mem_map_of_mem
  simp [List.mem_filter, present, Selector.matches, pending, continuation, outside]

/-- A member of the assignment, a row that is not pending, and a row that is
not a Goal continuation are never selected. -/
theorem kept_rows_are_not_selected (selector : Selector) (rows : List PendingRow)
    (row : PendingRow)
    (kept : selector.member row = true ∨ row.pending = false ∨ row.goalContinuation = false) :
    row ∉ rows.filter selector.matches := by
  intro selected
  have chosen := (List.mem_filter.mp selected).2
  simp only [Selector.matches, Bool.and_eq_true, Bool.not_eq_true'] at chosen
  rcases kept with member | idle | other
  · simp [member] at chosen
  · simp [idle] at chosen
  · simp [other] at chosen

end Hooks
