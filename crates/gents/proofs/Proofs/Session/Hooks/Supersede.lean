import Proofs.Session.Hooks.Claim

namespace Hooks

/-- The link of a pending Goal continuation's ancestry that core found
broken, judged from the row's own signed fields and the parent row it names.

Decision 2026-11-11, maintainer-approved: a continuation a new Task Goal
assignment makes stale is superseded whether or not its ancestry is intact
(before the lifecycle hooks, a broken or unsigned ancestry failed the claim
instead), but a broken one is never retired as an ordinary supersession: its
retirement names the link that failed.

What core decides here is what it can judge from request rows alone, without
the Goal: the row's receipt signature (the store's receipt verifier), whether
its signed `goal_assignment` names the Goal it continues, whether it names a
parent document, and that parent: visible at the snapshot, of the same node
and session, with a verifying receipt signature, continuing the same Goal,
and with a signed reference the row's reference continues
(`GoalAssignmentInput::continuation`). What core cannot judge, because only
the Goal-only ancestry walk (`request_head::authenticated_assignment_entry`)
knows it: the continuation's deterministic identity (request id and retry
key from Goal, parent and sequence), the runtime-local control receipt that
binds it to its parent's requester, every link above its immediate parent
(cycles, background-wake hops, the walk up to an assignment opening), and
whether that opening is an opening of one of the Goal's retained assignments.
A row broken only there is retired as ordinary. -/
inductive BrokenLink where
  | unsigned
  | otherGoal
  | noParent
  | parentAbsent
  | parentOutsideSession
  | parentUnsigned
  | diverges
  deriving DecidableEq, Repr

def BrokenLink.describe : BrokenLink → String
  | .unsigned => "its receipt signature is missing or invalid"
  | .otherGoal => "its signed Goal reference names another Goal"
  | .noParent => "it names no parent request"
  | .parentAbsent => "its parent request is not visible"
  | .parentOutsideSession => "its parent request is in another node or session"
  | .parentUnsigned => "its parent's receipt signature is missing or invalid"
  | .diverges => "its signed Goal reference does not continue its parent's"

/-- How core classifies a continuation it retires: a well-formed continuation
of another assignment, or one whose ancestry is broken at `link`. -/
inductive Ancestry where
  | ordinary
  | broken (link : BrokenLink)
  deriving DecidableEq, Repr

/-- One pending request core supersedes, with the request that supersedes it,
how core classified its ancestry and the reason core records. -/
structure StaleRequest where
  doc : String
  request : String
  successor : String
  successorDoc : String
  reason : String
  ancestry : Ancestry
  deriving DecidableEq, Repr

/-- A session request row as core reads it for a supersede selector: its
lifecycle, whether it is a Goal continuation (trigger kind `goal`), its
trigger id, the signed Goal assignment reference on its input (`goal_id`,
`root_request_doc_id`), whether its receipt signature verifies, and the parent
request document it names. -/
structure PendingRow where
  doc : String
  request : String
  pending : Bool
  goalContinuation : Bool
  trigger : Option String
  reference : Option (String × Option String)
  signed : Bool
  parent : Option String
  deriving DecidableEq, Repr

/-- The parent a retired continuation names, as core reads it by document:
whether it is of the continuation's node and session, whether its receipt
signature verifies, the Goal it continues when it is itself a Goal
continuation, and its signed Goal assignment reference. -/
structure ParentRow where
  doc : String
  inSession : Bool
  signed : Bool
  goal : Option String
  reference : Option (String × Option String)
  deriving DecidableEq, Repr

/-- The row continues its parent's Goal assignment: the parent continues the
same Goal if it continues one, and the row's signed reference is the
parent's carried on (`first_request_binding::continuation_reference_matches`
with the row's own Goal). -/
def continues (row : PendingRow) (parent : ParentRow) : Bool :=
  parent.goal.all (fun goal => some goal == row.trigger) &&
    match parent.reference with
    | some (goal, root) =>
        some goal == row.trigger && row.reference == some (goal, some (root.getD parent.doc))
    | none =>
        row.reference.all fun (goal, root) =>
          some goal == row.trigger && root.all (· == parent.doc)

/-- Core's classification of a continuation it retires, checking the links in
order and naming the first that fails. -/
def classify (row : PendingRow) (parents : List ParentRow) : Ancestry :=
  if !row.signed then .broken .unsigned
  else if row.reference.any (fun reference => some reference.1 != row.trigger) then
    .broken .otherGoal
  else
    match row.parent with
    | none => .broken .noParent
    | some doc =>
        match parents.find? (·.doc == doc) with
        | none => .broken .parentAbsent
        | some parent =>
            if !parent.inSession then .broken .parentOutsideSession
            else if !parent.signed then .broken .parentUnsigned
            else if !continues row parent then .broken .diverges
            else .ordinary

/-- Which pending continuations a new Goal assignment keeps: the assignment's
Goal and root, the successor and the ordinary reason core records on the
others, and the call the plugin names as the next step for a broken one. -/
structure Selector where
  goal : String
  root : String
  successor : String
  successorDoc : String
  reason : String
  next : Option String
  deriving DecidableEq, Repr

/-- What core appends to the ordinary reason for a broken ancestry. -/
def brokenText (link : BrokenLink) (next : Option String) : String :=
  "; Goal ancestry broken: " ++ link.describe ++ ((next.map ("; next: " ++ ·)).getD "")

/-- The reason core records on a retirement, a function of its
classification alone. -/
def Selector.reasonFor (selector : Selector) : Ancestry → String
  | .ordinary => selector.reason
  | .broken link => selector.reason ++ brokenText link selector.next

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

def Selector.entry (selector : Selector) (parents : List ParentRow) (row : PendingRow) :
    StaleRequest :=
  ⟨row.doc, row.request, selector.successor, selector.successorDoc,
    selector.reasonFor (classify row parents), classify row parents⟩

/-- The retirements a selector names on the session's rows at the snapshot,
with the parents core read for them. -/
def Selector.stale (selector : Selector) (rows : List PendingRow) (parents : List ParentRow) :
    List StaleRequest :=
  (rows.filter selector.matches).map (selector.entry parents)

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
    (selector : Selector) (rows : List PendingRow) (parents : List ParentRow) (core : Core) :
    Option Core :=
  (selector.stale rows parents).foldlM (fun current entry => retire entry current) core

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
    (selector : Selector) (rows : List PendingRow) (parents : List ParentRow) (core next : Core)
    (performed : supersedeMatching retire selector rows parents core = some next) :
    Retired retire (selector.stale rows parents) core next :=
  fold_retired retire (selector.stale rows parents) core next performed

/-- Every pending Goal continuation at the snapshot that is not a member of
the assignment is retired. -/
theorem every_stale_continuation_is_retired (selector : Selector) (rows : List PendingRow)
    (parents : List ParentRow) (row : PendingRow) (present : row ∈ rows)
    (pending : row.pending = true) (continuation : row.goalContinuation = true)
    (outside : selector.member row = false) :
    selector.entry parents row ∈ selector.stale rows parents := by
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

/-- A retirement's reason and classification are core's classification of
that row: the plugin names the assignment, never a row's reason. -/
theorem retire_reason_is_its_classification (selector : Selector) (parents : List ParentRow)
    (row : PendingRow) :
    (selector.entry parents row).ancestry = classify row parents ∧
      (selector.entry parents row).reason = selector.reasonFor (classify row parents) :=
  ⟨rfl, rfl⟩

private theorem extended_ne (text : String) (link : BrokenLink) (next : Option String) :
    text ++ brokenText link next ≠ text := by
  intro same
  have lengths := congrArg String.length same
  simp only [String.length_append, brokenText] at lengths
  have prefixed : ("; Goal ancestry broken: ").length = 24 := by decide
  omega

/-- A continuation whose ancestry is broken is never retired with the
ordinary reason. -/
theorem broken_ancestry_is_never_retired_as_ordinary (selector : Selector)
    (parents : List ParentRow) (row : PendingRow) (link : BrokenLink)
    (broken : classify row parents = .broken link) :
    (selector.entry parents row).reason ≠ selector.reason ∧
      (selector.entry parents row).reason = selector.reason ++ brokenText link selector.next := by
  simp only [Selector.entry, broken, Selector.reasonFor]
  exact ⟨extended_ne _ _ _, trivial⟩

/-- A well-formed continuation of another assignment is retired with the
ordinary reason and never with a broken one. -/
theorem ordinary_ancestry_is_never_retired_as_broken (selector : Selector)
    (parents : List ParentRow) (row : PendingRow) (ordinary : classify row parents = .ordinary) :
    (selector.entry parents row).reason = selector.reason ∧
      ∀ link, (selector.entry parents row).reason ≠ selector.reasonFor (.broken link) := by
  simp only [Selector.entry, ordinary, Selector.reasonFor]
  exact ⟨trivial, fun link same => extended_ne selector.reason link selector.next same.symm⟩

/-- What `ordinary` means: the row is signed and names a visible parent of its
node and session, with a verifying signature, whose Goal assignment it
continues. -/
theorem ordinary_ancestry_is_well_formed (row : PendingRow) (parents : List ParentRow)
    (ordinary : classify row parents = .ordinary) :
    row.signed = true ∧ ∃ doc parent, row.parent = some doc ∧
      parents.find? (·.doc == doc) = some parent ∧ parent.inSession = true ∧
        parent.signed = true ∧ continues row parent = true := by
  unfold classify at ordinary
  by_cases signed : row.signed = true
  · simp only [signed, Bool.not_true, Bool.false_eq_true, if_false] at ordinary
    split at ordinary
    · cases ordinary
    · cases named : row.parent with
      | none => simp [named] at ordinary
      | some doc =>
        simp only [named] at ordinary
        cases found : parents.find? (·.doc == doc) with
        | none => simp [found] at ordinary
        | some parent =>
          simp only [found] at ordinary
          by_cases inSession : parent.inSession = true
          · by_cases parentSigned : parent.signed = true
            · by_cases continued : continues row parent = true
              · exact ⟨signed, doc, parent, rfl, found, inSession, parentSigned, continued⟩
              · simp [inSession, parentSigned, continued] at ordinary
            · simp [inSession, parentSigned] at ordinary
          · simp [inSession] at ordinary
  · simp [signed] at ordinary

end Hooks
