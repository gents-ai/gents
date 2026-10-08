import Proofs.Request.State

/-! Durable session owner; Nat identifiers/times abstract validated canonical
strings. Authorization and atomic query/write remain DB adapter obligations.
Read-only session discovery uses the authenticated caller's DefraDB ACP visibility;
agent/requester fields identify a selected row, not additional read permissions.
Presentation and provenance never select execution configuration. -/
namespace AgentSession
structure Scope where
  agent : Nat
  session : SessionId
  requester : Option Nat
  deriving DecidableEq, Repr
inductive TitleSource where
  | placeholder | generated | task | user
  deriving DecidableEq, Repr
def TitleSource.toWireName : TitleSource → String
  | .placeholder => "placeholder"
  | .generated => "generated"
  | .task => "task"
  | .user => "user"

structure Title where
  text : String
  source : TitleSource
  deriving DecidableEq, Repr
structure ForkOrigin where
  sourceSession : SessionId
  atUserTurn : Nat
  deriving DecidableEq, Repr
structure Provenance where
  task : Option Nat := none
  graphRun : Option Nat := none
  parentRequestDoc : Option Nat := none
  fork : Option ForkOrigin := none
  deriving DecidableEq, Repr
structure RequestObservation where
  docId : Nat
  requestId : RequestId
  state : RequestState
  deriving DecidableEq, Repr
structure Observation where
  activity : Time
  preview : Option String
  latest : Option RequestObservation
  deriving DecidableEq, Repr
structure Document where
  scope : Scope
  agent : AgentId
  createdAt : Time
  closedAt : Option Time := none
  title : Option Title := none
  tags : List String := []
  provenance : Option Provenance := none
  observation : Option Observation := none
  deriving DecidableEq, Repr
/-- Authoritative query projection, not another writable request/index. -/
structure RequestFact where
  purpose : RequestPurpose
  scope : Scope
  agent : AgentId
  createdAt : Time
  observed : RequestObservation
  deriving DecidableEq, Repr
/-- Logical request ID breaks timestamp ties. -/
def newer (a b : RequestFact) : Bool :=
  a.createdAt > b.createdAt ||
    (a.createdAt == b.createdAt && a.observed.requestId > b.observed.requestId)
def newest : List RequestFact → Option RequestFact
  | [] => none
  | row :: rest => match newest rest with
    | none => some row
    | some previous => if newer row previous then some row else some previous

/-- `none` queries all requesters; `some none` queries exact absent scope. -/
def latest (rows : List RequestFact) (agent session : Nat)
    (requester : Option (Option Nat)) : Option RequestFact :=
  newest (rows.filter fun r => r.purpose == .normal && r.scope.agent == agent && r.scope.session == session &&
    requester.all (fun scope => r.scope.requester == scope))

theorem newest_member (rows : List RequestFact) (r : RequestFact)
    (h : newest rows = some r) : r ∈ rows := by
  induction rows with
  | nil => simp [newest] at h
  | cons row rest ih =>
    simp only [newest] at h
    cases hn : newest rest with
    | none => simp [hn] at h; subst r; simp
    | some previous =>
      simp only [hn] at h
      split at h
      · simp only [Option.some.injEq] at h; subst r; simp
      · simp only [Option.some.injEq] at h; subst r
        exact List.mem_cons_of_mem _ (ih hn)

/-- Every selected row dominates all candidates in canonical timestamp/ID order. -/
theorem newest_maximal (rows : List RequestFact) (r : RequestFact)
    (h : newest rows = some r) : ∀ candidate ∈ rows,
      candidate.createdAt < r.createdAt ∨
      (candidate.createdAt = r.createdAt ∧ candidate.observed.requestId ≤ r.observed.requestId) := by
  induction rows generalizing r with
  | nil => simp [newest] at h
  | cons row rest ih =>
    simp only [newest] at h
    cases hn : newest rest with
    | none =>
      have empty : rest = [] := by
        cases rest with
        | nil => rfl
        | cons a tail =>
          simp only [newest] at hn
          cases ht : newest tail with
          | none => simp [ht] at hn
          | some p =>
            simp only [ht] at hn
            split at hn <;> simp_all
      subst rest
      simp only [newest, Option.some.injEq] at h
      subst r
      simp
    | some previous =>
      have hb := ih previous hn
      simp only [hn] at h
      split at h
      · rename_i hc
        simp only [Option.some.injEq] at h
        subst r
        intro candidate hm
        rcases List.mem_cons.mp hm with he | he
        · subst candidate; exact Or.inr ⟨rfl, Nat.le_refl _⟩
        · have bound := hb candidate he
          simp only [newer, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq,
            beq_iff_eq] at hc
          dsimp only [Time, RequestId] at *
          omega
      · rename_i hc
        simp only [Option.some.injEq] at h
        subst r
        intro candidate hm
        rcases List.mem_cons.mp hm with he | he
        · subst candidate
          simp only [newer, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq,
            beq_iff_eq] at hc
          dsimp only [Time, RequestId] at *
          omega
        · exact hb candidate he

/-- Filtering candidates cannot change a retained canonical winner. -/
theorem newest_filter_preserves (rows : List RequestFact) (r : RequestFact)
    (p : RequestFact → Bool) (h : newest rows = some r) (hr : p r = true) :
    newest (rows.filter p) = some r := by
  induction rows generalizing r with
  | nil => simp [newest] at h
  | cons row rest ih =>
    simp only [newest] at h
    cases hn : newest rest with
    | none =>
      have empty : rest = [] := by
        cases rest with
        | nil => rfl
        | cons a tail =>
          simp only [newest] at hn
          cases ht : newest tail with
          | none => simp [ht] at hn
          | some previous => simp only [ht] at hn; split at hn <;> simp_all
      subst rest
      simp only [newest, Option.some.injEq] at h
      subst r
      simp [hr, newest]
    | some previous =>
      simp only [hn] at h
      split at h
      · rename_i hc
        simp only [Option.some.injEq] at h
        subst r
        simp only [List.filter_cons, hr, Bool.true_eq, ↓reduceIte, newest]
        cases hf : newest (rest.filter p) with
        | none => rfl
        | some filtered =>
          have hm := newest_member _ filtered hf
          have hb := newest_maximal rest previous hn filtered (List.mem_filter.mp hm).1
          have hbetter : newer row filtered = true := by
            simp only [newer, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq,
              beq_iff_eq] at hc ⊢
            dsimp only [Time, RequestId] at *
            omega
          simp [hbetter]
      · rename_i hc
        simp only [Option.some.injEq] at h
        subst r
        have hf := ih previous hn hr
        cases hp : p row <;> simp [List.filter_cons, hp, newest, hf, hc]

theorem latest_scope (rows : List RequestFact) (agent session : Nat)
    (requester : Option (Option Nat)) (r : RequestFact)
    (h : latest rows agent session requester = some r) :
    r ∈ rows ∧ r.scope.agent = agent ∧ r.scope.session = session ∧
      requester.all (fun scope => r.scope.requester == scope) = true := by
  have hm := newest_member _ r h
  simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq, and_assoc] at hm
  exact ⟨hm.1, hm.2.2⟩

theorem latest_is_normal (rows : List RequestFact) (agent session : Nat)
    (requester : Option (Option Nat)) (r : RequestFact)
    (h : latest rows agent session requester = some r) : r.purpose = .normal := by
  have hm := newest_member _ r h
  simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq, and_assoc] at hm
  exact hm.2.1

theorem latest_maximal (rows : List RequestFact) (agent session : Nat)
    (requester : Option (Option Nat)) (r candidate : RequestFact)
    (h : latest rows agent session requester = some r)
    (hm : candidate ∈ rows) (ha : candidate.scope.agent = agent)
    (hs : candidate.scope.session = session)
    (hp : candidate.purpose = .normal)
    (hr : requester.all (fun scope => candidate.scope.requester == scope) = true) :
    candidate.createdAt < r.createdAt ∨
      (candidate.createdAt = r.createdAt ∧ candidate.observed.requestId ≤ r.observed.requestId) := by
  apply newest_maximal _ r h candidate
  simp [List.mem_filter, hm, ha, hs, hp, hr]

theorem title_arrival_preserves_latest (rows : List RequestFact) (title : RequestFact)
    (agent session : Nat) (requester : Option (Option Nat))
    (hp : title.purpose = .titleAudit) :
    latest (title :: rows) agent session requester = latest rows agent session requester := by
  simp [latest, hp]

theorem latest_exact_of_session_winner (rows : List RequestFact) (agent session : Nat)
    (r : RequestFact) (h : latest rows agent session none = some r) :
    latest rows agent session (some r.scope.requester) = some r := by
  have hf := newest_filter_preserves
    (rows.filter fun row => row.purpose == .normal && row.scope.agent == agent && row.scope.session == session)
    r (fun row => row.scope.requester == r.scope.requester) (by simpa [latest] using h) (by simp)
  simpa [latest, List.filter_filter, Bool.and_assoc, Bool.and_comm, Bool.and_left_comm] using hf

/-- Run with complete authoritative rows inside the admission/successor transaction,
including the currently indexed request. `preview` is whitespace-normalized by the
existing projection adapter before this character-bound operation. -/
def advance (s : Document) (rows : List RequestFact) (r : RequestFact)
    (preview : String) (now : Time) : Document :=
  if r.scope = s.scope ∧ r.agent = s.agent ∧
      r ∈ rows ∧ latest rows s.scope.agent s.scope.session (some s.scope.requester) = some r then
    { s with observation := some {
      activity := max now (s.observation.map (·.activity) |>.getD s.createdAt)
      preview := some (String.mk (preview.toList.take 240))
      latest := some r.observed } }
  else s
theorem title_does_not_advance (s : Document) (rows : List RequestFact)
    (r : RequestFact) (preview : String) (now : Time)
    (hp : r.purpose = .titleAudit) : advance s rows r preview now = s := by
  unfold advance
  split
  · rename_i h
    have hn := latest_is_normal rows s.scope.agent s.scope.session
      (some s.scope.requester) r h.2.2.2
    simp [hp] at hn
  · rfl

/-- Missing exact rows remain unknown; never return an older local request. -/
def observedRequest (rows : List RequestFact) (o : RequestObservation) : Option RequestFact :=
  rows.find? (fun r => r.purpose == .normal && r.observed.docId == o.docId && r.observed.requestId == o.requestId)

theorem title_arrival_preserves_observed (rows : List RequestFact) (title : RequestFact)
    (o : RequestObservation) (hp : title.purpose = .titleAudit) :
    observedRequest (title :: rows) o = observedRequest rows o := by
  simp [observedRequest, hp]
/-- A notification supplies identity only. Resolve its current authoritative row
inside the existing projection transaction; event payload state cannot overwrite
newer request facts. DB snapshot freshness remains the adapter's obligation. -/
def refresh (s : Document) (rows : List RequestFact) (event : RequestObservation)
    (now : Time) : Document :=
  match s.observation with
  | some o => match o.latest with
    | some old => if old.docId = event.docId ∧ old.requestId = event.requestId then
        match observedRequest rows event with
        | some current => if current.scope = s.scope ∧ current.agent = s.agent then
            { s with observation := some { o with latest := some current.observed, activity := max now o.activity } }
          else s
        | none => s
      else s
    | none => s
  | none => s

/-- Only a newly published tool terminal emits activity, in the same transaction.
Replays and losing publications emit no notification. Request lifecycle state
need not change; identity is resolved through the authoritative request row. -/
def toolActivity (s : Document) (rows : List RequestFact) (event : RequestObservation)
    (now : Time) (published : Bool) : Document :=
  if published then refresh s rows event now else s

theorem unpublished_tool_activity_noop (s : Document) (rows : List RequestFact)
    (event : RequestObservation) (now : Time) :
    toolActivity s rows event now false = s := by simp [toolActivity]

theorem published_tool_activity (s : Document) (o : Observation)
    (r : RequestFact) (now : Time)
    (ho : s.observation = some o) (hl : o.latest = some r.observed)
    (hs : r.scope = s.scope) (hb : r.agent = s.agent)
    (hp : r.purpose = .normal) :
    toolActivity s [r] r.observed now true =
      { s with observation := some { o with latest := some r.observed, activity := max now o.activity } } := by
  simp [toolActivity, refresh, ho, hl, observedRequest, hp, hs, hb]

/-- Stale event lifecycle payloads have no influence on the projection. -/
theorem refresh_ignores_event_state (s : Document) (rows : List RequestFact)
    (event : RequestObservation) (state : RequestState) (now : Time) :
    refresh s rows { event with state } now = refresh s rows event now := rfl

def touch (s : Document) (now : Time) : Document :=
  { s with observation := some (match s.observation with
      | some o => { o with activity := max now o.activity }
      | none => { activity := max now s.createdAt, preview := none, latest := none }) }
def rename (s : Document) (title : Option Title) (now : Time) : Document :=
  { touch s now with title }
def setClosed (s : Document) (closed : Option Time) (now : Time) : Document :=
  { touch s now with closedAt := closed }
theorem advance_preserves_identity (s : Document) (rows : List RequestFact)
    (r : RequestFact) (preview : String) (now : Time) :
    (advance s rows r preview now).scope = s.scope ∧
    (advance s rows r preview now).agent = s.agent ∧
    (advance s rows r preview now).provenance = s.provenance := by
  unfold advance
  split <;> simp
theorem stale_refresh_noop (s : Document) (o : Observation)
    (old r : RequestObservation) (rows : List RequestFact) (now : Time)
    (ho : s.observation = some o) (hl : o.latest = some old)
    (hne : old.docId ≠ r.docId ∨ old.requestId ≠ r.requestId) :
    refresh s rows r now = s := by
  simp only [refresh, ho, hl]
  split <;> simp_all
theorem rename_preserves_request (s : Document) (o : Observation)
    (title : Option Title) (now : Time) (ho : s.observation = some o) :
    (rename s title now).observation = some { o with activity := max now o.activity } ∧
    (rename s title now).closedAt = s.closedAt ∧
    (rename s title now).provenance = s.provenance := by
  simp [rename, touch, ho]
theorem reopen_preserves_creation (s : Document) (now : Time) :
    (setClosed s none now).createdAt = s.createdAt ∧
    (setClosed s none now).title = s.title ∧
    (setClosed s none now).provenance = s.provenance := by
  simp [setClosed, touch]
theorem touch_activity_monotone (s : Document) (o : Observation) (now : Time)
    (h : s.observation = some o) :
    ∃ next, (touch s now).observation = some next ∧ o.activity ≤ next.activity := by
  refine ⟨{ o with activity := max now o.activity }, ?_, Nat.le_max_right _ _⟩
  simp [touch, h]

end AgentSession
