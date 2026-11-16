import Proofs.Store.Identity.EnrollmentGrant

/-!
A desktop that meets a node already set up on its computer: a home made with
`gents init` and served from the terminal, opened by a desktop with no client
state of its own, or with a client that never joined this node. The desktop
cannot read the node's settings until it is an enrolled client of it, so
Connect this desktop runs the whole join as one operation: it creates the
desktop's own client identity when there is none, saves the node's route,
starts the client and enrolls it through the same pairing handler the setup
wizard's local path runs.

Rust owner: `connect_local_node` in
`crates/gents-desktop-bridge/src/tauri_commands/managed_server/connect.rs`, run
by `desktop_managed_server_enroll`; enrollment is
`ensure_managed_runtime_pairing` in
`crates/gents-desktop-bridge/src/local_pairing.rs`.
-/
namespace DesktopConnect
open Identity.EnrollmentGrant

inductive NodeState where
  | notRunning
  /-- The home's store was made by an older Gents: the repair owner's report
  (`refuse_older_home`) is the answer, before anything is asked of the node. -/
  | olderHome
  | outdated
  | serving
  deriving DecidableEq, Repr

structure Observation where
  node : NodeState
  identity : Bool
  routeSaved : Bool
  enrolled : Bool
  answered : Bool
  /-- Decision: a local desktop is approved the way the setup wizard's local path
  approves it, by the User whose key `gents init` left with this computer's CLI
  client, through the one approval handler (`approve_enrollment`, Lean
  `applyDecision .approve`). That key stays with the CLI client and the
  desktop's own key stays with the desktop; for a home `gents init` made that
  User owns the Node. -/
  approver : Standing
  deriving DecidableEq, Repr

inductive Step where
  | createIdentity
  | saveRoute
  | startClient
  | requestEnrollment
  | approve
  deriving DecidableEq, Repr

/-- Each refusal is shown on the screen with the call that moves past it. -/
inductive Refusal where
  | nodeNotRunning
  | olderHome
  | outdatedNode
  | enrollmentRefused
  | approvalPending
  deriving DecidableEq, Repr

structure Outcome where
  steps : List Step
  refusal : Option Refusal
  enrolled : Bool
  deriving DecidableEq, Repr

def blocker : NodeState → Option Refusal
  | .notRunning => some .nodeNotRunning
  | .olderHome => some .olderHome
  | .outdated => some .outdatedNode
  | .serving => none

def opt (c : Bool) (s : Step) : List Step := if c then [s] else []

def clientSteps (o : Observation) : List Step :=
  opt (!o.identity) .createIdentity ++ opt (!o.routeSaved) .saveRoute ++ [.startClient]

/-- A node that cannot be joined is reported before the desktop changes
anything. An enrolled desktop only makes sure its client runs. Otherwise the
desktop requests enrollment; the approver's grant decides it (`approval`), and
a node that does not answer in time leaves the request pending, so connecting
again resends that same request. -/
def connect (o : Observation) : Outcome :=
  match blocker o.node with
  | some refusal => ⟨[], some refusal, o.enrolled⟩
  | none =>
    if o.enrolled then ⟨clientSteps o, none, true⟩
    else if !o.answered then
      ⟨clientSteps o ++ [.requestEnrollment], some .approvalPending, false⟩
    else match approval o.approver with
      | .approve => ⟨clientSteps o ++ [.requestEnrollment, .approve], none, true⟩
      | .refuse => ⟨clientSteps o ++ [.requestEnrollment], some .enrollmentRefused, false⟩

/-- The node's authority after a connect: an approval is the enrollment grant
for the desktop's request, and nothing else is written. -/
def grantsAfter (s : State) (request desktop : String) (o : Observation) : State :=
  if (connect o).steps.contains .approve then applyDecision s request desktop .approve else s

theorem approval_owner : approval .owner = .approve :=
  approval_follows_node_administration.1

theorem blocked_connect_changes_nothing (o : Observation) (r : Refusal)
    (h : blocker o.node = some r) : connect o = ⟨[], some r, o.enrolled⟩ := by
  simp [connect, h]

theorem connect_shape (o : Observation) :
    (connect o).steps = [] ∨ ∃ tail, (connect o).steps = clientSteps o ++ tail ∧
      Step.createIdentity ∉ tail ∧ Step.saveRoute ∉ tail := by
  unfold connect
  split
  · exact Or.inl rfl
  · refine Or.inr ?_
    split
    · exact ⟨[], by simp, by simp, by simp⟩
    · split
      · exact ⟨[.requestEnrollment], rfl, by simp, by simp⟩
      · split
        · exact ⟨[.requestEnrollment, .approve], rfl, by simp, by simp⟩
        · exact ⟨[.requestEnrollment], rfl, by simp, by simp⟩

/-- A desktop that has a client identity keeps it: connecting never replaces
the desktop's User key. -/
theorem existing_identity_is_kept (o : Observation) (h : o.identity = true) :
    Step.createIdentity ∉ (connect o).steps := by
  rcases connect_shape o with e | ⟨tail, e, notIn, _⟩
  · simp [e]
  · simp [e, clientSteps, opt, h, notIn]

theorem saved_route_is_kept (o : Observation) (h : o.routeSaved = true) :
    Step.saveRoute ∉ (connect o).steps := by
  rcases connect_shape o with e | ⟨tail, e, _, notIn⟩
  · simp [e]
  · simp [e, clientSteps, opt, h, notIn]

theorem approve_needs_approval (o : Observation) (h : Step.approve ∈ (connect o).steps) :
    blocker o.node = none ∧ o.enrolled = false ∧ o.answered = true ∧
      approval o.approver = .approve := by
  unfold connect at h
  split at h
  · simp at h
  · rename_i hb
    split at h
    · simp [clientSteps, opt] at h
    · split at h
      · simp [clientSteps, opt] at h
      · split at h
        · simp_all
        · simp [clientSteps, opt] at h

/-- A connect that reports no refusal ends with an enrolled desktop. -/
theorem unrefused_connect_enrolls (o : Observation) (h : (connect o).refusal = none) :
    (connect o).enrolled = true := by
  unfold connect at *
  cases hb : blocker o.node with
  | some r => simp [hb] at h
  | none =>
    simp only [hb] at h ⊢
    by_cases he : o.enrolled
    · simp [he]
    · by_cases ha : o.answered
      · cases hp : approval o.approver <;> simp_all
      · simp_all

/-- A refused enrollment grants the desktop nothing. -/
theorem refused_enrollment_grants_nothing (s : State) (request desktop : String)
    (o : Observation) (h : approval o.approver = .refuse) :
    grantsAfter s request desktop o = s := by
  unfold grantsAfter
  split
  · rename_i c
    have := (approve_needs_approval o (List.contains_iff_mem.mp c)).2.2.2
    rw [h] at this
    cases this
  · rfl

/-- A node that does not answer in time leaves the desktop's request pending
and grants nothing; the identity, route and client the connect made stay, so
connecting again resends the same request. -/
theorem unanswered_enrollment_is_pending (o : Observation) (serving : o.node = .serving)
    (fresh : o.enrolled = false) (silent : o.answered = false) :
    connect o = ⟨clientSteps o ++ [.requestEnrollment], some .approvalPending, false⟩ := by
  simp [connect, blocker, serving, fresh, silent]

/-- An approved connect leaves the desktop holding every tuple the enrollment
grant derives. -/
theorem connected_desktop_holds_the_enrollment_grant (s : State) (request desktop : String)
    (o : Observation) (approved : Step.approve ∈ (connect o).steps)
    (fresh : s.receipts.any (·.request == request) = false) (t : Tuple) (d : t ∈ derived) :
    holds (grantsAfter s request desktop o) desktop t = true := by
  unfold grantsAfter
  rw [if_pos (List.contains_iff_mem.mpr approved)]
  exact approve_derives_all s request desktop fresh t d

/-- Nothing more: every tuple held after a connect was held before, or is one
the enrollment grant derives for the desktop. -/
theorem connect_grants_nothing_more (s : State) (request desktop : String) (o : Observation)
    (a : Held) (h : a ∈ (grantsAfter s request desktop o).held) :
    a ∈ s.held ∨ (a.subject = desktop ∧ a.tuple ∈ derived) := by
  unfold grantsAfter at h
  split at h
  · simp only [applyDecision, grant] at h
    split at h
    · exact Or.inl h
    · rcases List.mem_append.mp h with old | new
      · exact Or.inl old
      · obtain ⟨t, mem, rfl⟩ := List.mem_map.mp new
        exact Or.inr ⟨rfl, (List.mem_filter.mp (List.mem_filter.mp mem).1).1⟩
  · exact Or.inl h

/-- The node's other subjects, its CLI User and its Agents, hold exactly what
they held before. -/
theorem connect_leaves_other_subjects_untouched (s : State) (request desktop : String)
    (o : Observation) (subject : String) (other : subject ≠ desktop) (t : Tuple) :
    holds (grantsAfter s request desktop o) subject t = holds s subject t := by
  unfold grantsAfter
  split
  · simp only [applyDecision, grant]
    split
    · rfl
    · unfold holds
      apply Bool.eq_iff_iff.mpr
      simp only [List.contains_iff_mem, List.mem_append, List.mem_map]
      constructor
      · rintro (old | ⟨u, _, hu⟩)
        · exact old
        · exact absurd (congrArg Held.subject hu).symm other
      · exact Or.inl
  · rfl

/-- Connecting again after a connect only makes sure the client runs. -/
theorem connect_again_only_starts_the_client (o : Observation) (serving : o.node = .serving)
    (identity : o.identity = true) (route : o.routeSaved = true) (enrolled : o.enrolled = true) :
    connect o = ⟨[.startClient], none, true⟩ := by
  simp [connect, blocker, clientSteps, opt, serving, identity, route, enrolled]

/-- The case of #3735: a desktop with no client state meets a node `gents init`
made and the terminal serves. It becomes an enrolled client in one connect. -/
theorem fresh_desktop_joins_a_terminal_node (answered : Bool) (h : answered = true) :
    connect ⟨.serving, false, false, false, answered, .owner⟩ =
      ⟨[.createIdentity, .saveRoute, .startClient, .requestEnrollment, .approve], none, true⟩ := by
  simp [connect, blocker, clientSteps, opt, h, approval_owner]

/-- What the startup screen shows while the settings of a configured local home
are unread. Decision (#3735): a desktop that is not enrolled with the node is
shown the connect screen whatever state the node is in, naming up front what a
connect would refuse (`blocker`), so a stopped node is reported with the call
that starts it rather than left as an unexplained wait, and a node that comes
up is seen by the next read of its status. An enrolled desktop waits for its
settings. -/
inductive Screen where
  | loadingSettings
  | notConnected
  deriving DecidableEq, Repr

def screen (node : NodeState) (enrolled : Bool) : Screen × Option Refusal :=
  if enrolled then (.loadingSettings, none) else (.notConnected, blocker node)

theorem stopped_node_is_named_on_the_connect_screen :
    screen .notRunning false = (.notConnected, some .nodeNotRunning) := rfl

/-- The refusal the screen names is the one Connect this desktop would give a
fresh desktop. -/
theorem screen_names_what_connect_refuses (node : NodeState) :
    (screen node false).2 = (connect ⟨node, false, false, false, true, .owner⟩).refusal := by
  cases node <;> simp [screen, connect, blocker, approval_owner]

end DesktopConnect
