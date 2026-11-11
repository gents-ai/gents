import Proofs.Hooks.Staging

namespace Hooks

/-- The request that supersedes a retired one, as the plugin that asked for
the retirement names it: its request id and its document id. Core records it
on the superseded row with the pending terminal it performs.

Decision 2026-11-11, maintainer-approved: a retirement always names its
successor. A retired request without one would carry a superseded state with
no request that superseded it, so the verdict cannot be formed without it. -/
structure Successor where
  request : String
  doc : String
  deriving DecidableEq, Repr

/-- What a plugin may answer at `request.claim.gate`. The hook catalog
(gents-ai/gents#3642) found four outcomes at the claim boundary today:
claim, leave the request queued, retire it (pending to superseded) and reject
it (pending to failed). A plugin names the outcome; core performs the
transition. A unit-returning notification cannot express this. -/
inductive ClaimVerdict where
  | allow
  | leaveQueued
  | retire (reason : String) (successor : Successor)
  | reject (reason : String)
  deriving DecidableEq, Repr

def ClaimVerdict.severity : ClaimVerdict → Nat
  | .allow => 0
  | .leaveQueued => 1
  | .retire _ _ => 2
  | .reject _ => 3

def ClaimVerdict.narrow (current next : ClaimVerdict) : ClaimVerdict :=
  if current.severity < next.severity then next else current

/-- Decision (maintainer, gents-ai/gents#3656): the most severe verdict wins,
reject over retire over leave queued over allow, independent of plugin order;
the first plugin in the fixed order supplies the reason among equals. A denial
is never masked by another plugin's softer answer. The hand-written sequence
in `request/lifecycle/claim.rs` takes the first non-allow answer instead. -/
def combine (verdicts : List ClaimVerdict) : ClaimVerdict :=
  verdicts.foldl ClaimVerdict.narrow .allow

private theorem narrow_severity (current next : ClaimVerdict) :
    (current.narrow next).severity = max current.severity next.severity := by
  unfold ClaimVerdict.narrow
  split <;> omega

private theorem foldl_narrow_bounds (verdicts : List ClaimVerdict) (start : ClaimVerdict) :
    start.severity ≤ (verdicts.foldl ClaimVerdict.narrow start).severity ∧
      ∀ verdict ∈ verdicts,
        verdict.severity ≤ (verdicts.foldl ClaimVerdict.narrow start).severity := by
  induction verdicts generalizing start with
  | nil => simp
  | cons head rest ih =>
    obtain ⟨fromStart, fromRest⟩ := ih (start.narrow head)
    rw [narrow_severity] at fromStart
    simp only [List.foldl_cons, List.mem_cons, forall_eq_or_imp]
    exact ⟨by omega, by omega, fromRest⟩

private theorem foldl_narrow_selects (verdicts : List ClaimVerdict) (start : ClaimVerdict) :
    verdicts.foldl ClaimVerdict.narrow start = start ∨
      verdicts.foldl ClaimVerdict.narrow start ∈ verdicts := by
  induction verdicts generalizing start with
  | nil => simp
  | cons head rest ih =>
    simp only [List.foldl_cons, List.mem_cons]
    rcases ih (start.narrow head) with kept | later
    · rw [kept]
      unfold ClaimVerdict.narrow
      split
      · exact Or.inr (Or.inl rfl)
      · exact Or.inl rfl
    · exact Or.inr (Or.inr later)

/-- A combined verdict only narrows: no plugin's answer is weakened. -/
theorem combined_is_at_least_each (verdicts : List ClaimVerdict) (verdict : ClaimVerdict)
    (member : verdict ∈ verdicts) : verdict.severity ≤ (combine verdicts).severity :=
  (foldl_narrow_bounds verdicts .allow).2 verdict member

/-- A request is claimable only when every plugin allows it. -/
theorem combined_allow_iff (verdicts : List ClaimVerdict) :
    combine verdicts = .allow ↔ ∀ verdict ∈ verdicts, verdict = .allow := by
  constructor
  · intro allowed verdict member
    have bound := combined_is_at_least_each verdicts verdict member
    rw [allowed] at bound
    cases verdict <;> simp [ClaimVerdict.severity] at bound ⊢
  · intro all
    rcases foldl_narrow_selects verdicts .allow with kept | selected
    · exact kept
    · exact all _ selected

/-- Which core transition is taken does not depend on plugin order. -/
theorem combined_severity_ignores_order (first second : List ClaimVerdict)
    (same : first.Perm second) : (combine first).severity = (combine second).severity := by
  have oneWay : ∀ left right : List ClaimVerdict, left.Perm right →
      (combine left).severity ≤ (combine right).severity := by
    intro left right permuted
    rcases foldl_narrow_selects left .allow with kept | selected
    · unfold combine
      rw [kept]
      simp [ClaimVerdict.severity]
    · exact combined_is_at_least_each right _ (permuted.mem_iff.mp selected)
  exact Nat.le_antisymm (oneWay first second same) (oneWay second first same.symm)

/-- What a plugin may return at `request.create.stage`, before core signs the
request: a value for its own extension slot of the request input, and an
opaque queue partition key. It cannot return a replacement input.

`partition` is what has to change in core for goal state to stop being a queue
partition key that core reads. Today wake coalescing, the claim snapshot, wake
input and wake enqueue each call `goal::load_request_assignment_scopes`
(`queue/coalescing.rs`, `queue/atomic_inputs.rs`, `claim.rs`,
`daemon/wake_input.rs`). For the contract to suffice, the key is stamped on
the request at creation from this value and the queue compares stamped keys;
core never asks a plugin a question at queue time. -/
structure Stamp where
  extension : Option String
  partition : Option String
  deriving DecidableEq, Repr

/-- The unsigned request input after `request.create.stage`: core's fields as
core authored them, and one stamp per plugin in plugin order. -/
structure Stamped (Input : Type) where
  input : Input
  stamps : List (String × Stamp)

variable {Input Core Q Ev Doc Val Op : Type}

def amend (input : Input) (plugins : List String) (stamps : List Stamp) : Stamped Input :=
  ⟨input, plugins.zip stamps⟩

def Stamped.partition (stamped : Stamped Input) : List (String × String) :=
  stamped.stamps.filterMap fun entry => entry.2.partition.map fun key => (entry.1, key)

/-- An amendment cannot replace what core authored. -/
theorem amend_keeps_core_input (input : Input) (plugins : List String) (stamps : List Stamp) :
    (amend input plugins stamps).input = input := rfl

/-- Each slot holds the stamp of the plugin it is named for. -/
theorem amend_pairs_each_plugin_with_its_stamp (input : Input) (plugins : List String)
    (stamps : List Stamp) (complete : plugins.length = stamps.length) :
    (amend input plugins stamps).stamps.map Prod.fst = plugins ∧
      (amend input plugins stamps).stamps.map Prod.snd = stamps := by
  induction plugins generalizing stamps with
  | nil =>
    cases stamps with
    | nil => simp [amend]
    | cons _ _ => simp at complete
  | cons name rest ih =>
    cases stamps with
    | nil => simp at complete
    | cons stamp others =>
      have tail := ih others (by simpa using complete)
      simp only [amend] at tail ⊢
      simp [tail.1, tail.2]

variable [DecidableEq Doc]

/-- `request.create.stage`: plugins stamp, then core creates and signs the
amended input, in one transaction. Core's `create` receives the input it
authored plus the stamps, so the signature covers them. Whether the creation
is fresh or a replay of a durable receipt stays core's answer. -/
def createStage (host : Host Core Q Doc Val Op) (plugins : List (Plugin Q Ev Stamp Doc Val Op))
    (actor : Actor) (event : Ev) (input : Input) (create : Stamped Input → Core → Option Core)
    (state : State Core Doc Val) :
    Except Refusal (State Core Doc Val × List Stamp × List Unit) :=
  around host plugins ([] : List (Plugin Q Unit Unit Doc Val Op)) actor (fun _ => event)
    (fun stamps core => create (amend input (plugins.map (·.manifest.name)) stamps) core)
    (fun _ _ => ()) state

/-- The request core creates carries the input core authored, and the stamps
are the plugins' decisions on the transaction's snapshot. -/
theorem created_request_keeps_core_input (host : Host Core Q Doc Val Op)
    (plugins : List (Plugin Q Ev Stamp Doc Val Op)) (actor : Actor) (event : Ev) (input : Input)
    (create : Stamped Input → Core → Option Core) (state final : State Core Doc Val)
    (stamps : List Stamp) (observed : List Unit)
    (ok : createStage host plugins actor event input create state = .ok (final, stamps, observed)) :
    ∃ prepared next,
      create (amend input (plugins.map (·.manifest.name)) stamps) prepared = some next ∧
      (amend input (plugins.map (·.manifest.name)) stamps).input = input ∧
      Reach host.perform actor next final.core := by
  obtain ⟨prepared, next, _, created, reached⟩ := hooks_cannot_replace_core_transition host
    plugins [] actor _ _ _ state final stamps observed ok
  exact ⟨prepared, next, created, rfl, reached⟩

end Hooks
