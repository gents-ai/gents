import Proofs.GoalAutomation
import Proofs.GoalAutomation.ClientControl
import Proofs.AgentSession
import Proofs.Session.CommandAuthority

namespace GoalAutomation.ClientStart

/-- A client creates its signed first request as itself, grants the selected
agent read access, and publishes a command bound to the physical request. The
agent stages the Goal and its receipt, but only the request owner can grant the
working relationship after observing the applied receipt. Each completed step
can be retried with the same durable identities after interruption. ACP1 does
not make these steps atomic and node write grants rely on trusted actors. -/
structure OpeningPublication where
  requestOwner : Nat
  requestDoc : Nat
  readerGranted : Bool := false
  commandPublished : Bool := false
  goalApplied : Bool := false
  workerGranted : Bool := false
  deriving DecidableEq, Repr

inductive OpeningAction where
  | grantReader | publishCommand | applyGoal | grantWorker
  deriving DecidableEq, Repr

def openingStep (state : OpeningPublication) : OpeningAction → OpeningPublication
  | .grantReader => { state with readerGranted := true }
  | .publishCommand => if state.readerGranted then { state with commandPublished := true } else state
  | .applyGoal => if state.commandPublished then { state with goalApplied := true } else state
  | .grantWorker => if state.goalApplied then { state with workerGranted := true } else state

theorem no_worker_before_goal (state : OpeningPublication) (absent : state.goalApplied = false) :
    openingStep state .grantWorker = state := by simp [openingStep, absent]

theorem opening_owner_is_preserved (state : OpeningPublication) (action : OpeningAction) :
    (openingStep state action).requestOwner = state.requestOwner := by
  cases action <;> simp [openingStep] <;> (split <;> rfl)

structure State where
  session : Option AgentSession.Document
  submission : SubmissionState
  deriving DecidableEq, Repr

structure Facts where
  authority : AgentSession.CommandAuthority.Facts
  hasPeer : Bool
  now : Nat
  expiresAt : Nat
  /-- The command requester names its signing actor. The first request retains
  canonical existing-session requester provenance, or that actor for a new
  session, while its admission signer remains the command actor. The existing
  request signature and admission owners establish these facts. -/
  firstRequestValid : Bool
  unfinishedRequest : Bool
  currentSessionDocument : Option String
  expectedSessionDocument : Option String
  existingClaim : Option Fingerprint := none
  firstGoalId : Option String := some "initial-assignment"
  firstRoot : Option String := none
  /-- The signed selector identifies the agent actor. Native ACP must authorize
  the signed requester against the session in the same processing transaction;
  a concurrent change to any authority dependency makes that transaction conflict. -/
  selectedAgent : Nat := 3
  requesterAuthorized : Bool := true
  authorityAvailable : Bool := true
  authorityVersion : Nat := 0
  committedAuthorityVersion : Nat := 0
  /-- Session control does not itself grant permission to invoke the agent.
  Native ACP checks the requester's `agents.invoke` on the exact selected
  Agent at admission and again in the processing transaction. -/
  requesterMayInvoke : Bool := true
  invokeVersion : Nat := 0
  committedInvokeVersion : Nat := 0
  deriving DecidableEq, Repr

def creationRequest (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) : CreateRequest :=
  { caller := toString requested.scope.node
    currentSession := toString requested.scope.session
    requestedOwner := toString requested.scope.node
    requestedSession := toString requested.scope.session
    objective, objectiveNonempty := !objective.isEmpty, tokenBudget := budget
    goalTools := true, goalCreate := true }

def creationAllowed (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) (claim : Option Fingerprint) : Bool :=
  match decideCreate (creationRequest requested objective budget) claim with
  | .fresh | .idempotent => true
  | _ => false

def firstBindingValid (facts : Facts) (goalId : String) : Bool :=
  facts.firstGoalId == some goalId && facts.firstRoot.isNone

/-- The command author is the signed acting requester. The first request keeps
the session's immutable requester provenance even when that author is a
different user granted control by native ACP. -/
def openingProvenanceValid (sessionRequester firstRequester commandAuthor requestSigner : Nat) : Bool :=
  sessionRequester == firstRequester && commandAuthor == requestSigner

theorem delegated_author_preserves_requester_provenance :
    openingProvenanceValid 1 1 2 2 = true := by rfl

theorem substituted_requester_is_not_provenance :
    openingProvenanceValid 1 2 2 2 = false := by rfl

inductive ProcessingDecision where
  | allowed | denied | conflict | unavailable | selectorMismatch
  deriving DecidableEq, BEq, Repr

/-- ACP1 trial cannot couple a different requester's permission decision to
the selected agent's write. Such commands remain pending until the native
transaction can provide that check; a same-actor write uses native ACP directly. -/
def requesterCheckAvailable (requester actor : Nat) (atomicCheck : Bool) : Bool :=
  requester == actor || atomicCheck

theorem cross_actor_without_atomic_check_is_unavailable (requester actor : Nat)
    (different : requester ≠ actor) :
    requesterCheckAvailable requester actor false = false := by
  simp [requesterCheckAvailable, beq_iff_eq, different]

theorem same_actor_uses_native_write (actor : Nat) :
    requesterCheckAvailable actor actor false = true := by simp [requesterCheckAvailable]

/-- Denial is a durable rejection; storage unavailability and transaction
conflicts are distinct failures and cannot be converted into authorization or
an applied receipt. Native ACP evaluates the signed requester's permission and
validates its dependencies in the same transaction as the receipt and effects. -/
def processingDecision (facts : Facts) (sessionAgent : Nat) : ProcessingDecision :=
  if facts.selectedAgent != sessionAgent then .selectorMismatch
  else if !facts.authorityAvailable then .unavailable
  else if facts.authorityVersion != facts.committedAuthorityVersion then .conflict
  else if facts.invokeVersion != facts.committedInvokeVersion then .conflict
  else if !facts.requesterAuthorized || !facts.requesterMayInvoke then .denied else .allowed

def processingAuthorized (facts : Facts) (sessionAgent : Nat) : Bool :=
  processingDecision facts sessionAgent == .allowed

/-- The runtime observes goal commands separately as each live agent; the
database supplies only rows that actor can read. Empty authorized reads remain
empty and never trigger a node-authority retry. Each retained pair binds the
command to the actor that observed it. -/
def observedCommands (live : List Nat) (nativeRows : List (Nat × Nat)) : List (Nat × Nat) :=
  nativeRows.filter (fun pair => live.contains pair.1)

theorem empty_command_observation_stays_empty (live : List Nat) :
    observedCommands live [] = [] := by rfl

theorem revoked_authority_cannot_process (facts : Facts) (sessionAgent : Nat)
    (revoked : facts.requesterAuthorized = false) :
    processingAuthorized facts sessionAgent = false := by
  unfold processingAuthorized processingDecision
  repeat' split
  all_goals first | rfl | simp_all

theorem changed_authority_cannot_commit (facts : Facts) (sessionAgent : Nat)
    (changed : facts.authorityVersion ≠ facts.committedAuthorityVersion) :
    processingAuthorized facts sessionAgent = false := by
  unfold processingAuthorized processingDecision
  repeat' split
  all_goals first | rfl | simp_all

theorem revoked_invocation_cannot_process (facts : Facts) (sessionAgent : Nat)
    (revoked : facts.requesterMayInvoke = false) :
    processingAuthorized facts sessionAgent = false := by
  unfold processingAuthorized processingDecision
  repeat' split
  all_goals first | rfl | simp_all

theorem changed_invocation_cannot_commit (facts : Facts) (sessionAgent : Nat)
    (changed : facts.invokeVersion ≠ facts.committedInvokeVersion) :
    processingAuthorized facts sessionAgent = false := by
  unfold processingAuthorized processingDecision
  repeat' split
  all_goals first | rfl | simp_all

/-- A client first reserves a session and its agent relationship in one native
transaction. Goal publication is a separate step: a failed publication leaves
the empty, user-owned session, and retry reuses its physical identity. A pre-existing
session keeps its owner and relationship; native ACP authorizes reuse. -/
structure Reservation where
  session : Option AgentSession.Document := none
  document : Option Nat := none
  owner : Option Nat := none
  worker : Option Nat := none
  deriving DecidableEq, Repr

def reserve? (before : Reservation) (requested : AgentSession.Document)
    (actor physical : Nat) (commit : Bool) : Option Reservation := do
  if !commit || (before.session.isSome && before.document.isNone) ||
      (before.session.isNone && requested.scope.requester != some actor) then none else
    let session ← AgentSession.ensure? before.session requested
    if before.session.isSome then some before
    else some {
      session := some session
      document := some physical
      owner := some actor
      worker := if actor == session.agent then none else some session.agent }

theorem failed_reservation_publishes_nothing (before : Reservation)
    (requested : AgentSession.Document) (actor physical : Nat) :
    reserve? before requested actor physical false = none := by simp [reserve?]

theorem matching_reservation_reuses_physical_id (before after : Reservation)
    (requested : AgentSession.Document) (actor physical : Nat)
    (existsSession : before.session.isSome = true)
    (reserved : reserve? before requested actor physical true = some after) :
    after = before := by
  unfold reserve? at reserved
  split at reserved
  · contradiction
  · cases hs : AgentSession.ensure? before.session requested with
    | none => simp [hs] at reserved
    | some session => simp [hs, existsSession] at reserved; exact reserved.symm

/-- The user-created session and its agent grant must already be committed;
the signed command names that physical session. The runtime transaction creates
the Goal, claim, first request and selected-agent-signed receipt together. -/
def start? (s : State) (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) (facts : Facts) (commit : Bool)
    (goalId : String := "initial-assignment") : Option State := do
  if !commit ||
      !AgentSession.CommandAuthority.authorized facts.authority facts.hasPeer facts.now facts.expiresAt ||
      !facts.firstRequestValid || !firstBindingValid facts goalId || facts.unfinishedRequest ||
      !processingAuthorized facts requested.agent ||
      s.session.isNone || facts.currentSessionDocument.isNone ||
      facts.currentSessionDocument != facts.expectedSessionDocument ||
      s.submission.durableGoal || s.submission.runnableRequest ||
      !creationAllowed requested objective budget facts.existingClaim then none
  else
    let session ← AgentSession.ensure? s.session requested
    let stagedRequest := submissionStep s.submission .stageRequest
    let stagedGoal := submissionStep stagedRequest .stageGoal
    some ⟨some session, submissionStep stagedGoal .commit⟩

theorem discarded_start_is_not_published (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (goalId : String) :
    start? s requested objective budget facts false goalId = none := by simp [start?]

theorem busy_session_cannot_start (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (h : facts.unfinishedRequest = true) :
    start? s requested objective budget facts commit goalId = none := by simp [start?, h]

theorem publication_has_reserved_session_and_goal (s post : State)
    (requested : AgentSession.Document) (objective : String) (budget : Option Int)
    (facts : Facts) (commit : Bool) (goalId : String)
    (h : start? s requested objective budget facts commit goalId = some post) :
    post.session.isSome = true ∧ post.submission.durableGoal = true ∧
      post.submission.runnableRequest = true := by
  unfold start? at h
  split at h
  · contradiction
  · cases hs : AgentSession.ensure? s.session requested with
    | none => simp [hs] at h
    | some session =>
      simp [hs, submissionStep] at h
      cases h
      exact ⟨rfl, rfl, rfl⟩

theorem wrong_goal_reference_cannot_start (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (wrong : facts.firstGoalId ≠ some goalId) :
    start? s requested objective budget facts commit goalId = none := by
  simp [start?, firstBindingValid, wrong]

theorem first_request_must_open_itself (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (bound : facts.firstRoot.isNone = false) :
    start? s requested objective budget facts commit goalId = none := by
  simp [start?, firstBindingValid, bound]

/-- Each logical assignment keeps its own immutable creation claim and opening
request. Retiring a terminal Goal uses DefraDB's retained tombstone/history;
its rows remain readable through the existing historical Goal owner. It does
not rewrite that assignment, its accounting, or its request ancestry. -/
structure Assignment where
  id : String
  creationKey : String
  snapshot : ClientControl.Snapshot
  rootRequest : String
  usageBaseline : Nat
  activeSeconds : Nat
  deriving DecidableEq, Repr

/-- `retained` is the read projection of historical Goal documents, not another
writable assignment registry. The transaction replaces only the current head. -/
structure Assignments where
  current : Assignment
  retained : List Assignment
  deriving DecidableEq, Repr

structure Replacement where
  expected : ClientControl.Revision
  nextId : String
  creationKey : String
  nextRevision : ClientControl.Revision
  firstRequest : String
  sessionUsage : Nat
  deriving DecidableEq, Repr

def replaceable (status : Goals.Status) : Bool :=
  status == .complete || status == .stopped

/-- Replacement is an explicit, revision-fenced first request in an idle
session. A stopped assignment stays terminal; a fresh identity and claim own
new counters and ancestry. The existing start owner authorizes and atomically
publishes the session, fresh Goal, request and receipt. -/
def replace? (s : State) (assignments : Assignments) (replacement : Replacement)
    (requested : AgentSession.Document) (objective : String) (budget : Option Int)
    (facts : Facts) (commit : Bool) : Option (State × Assignments) := do
  let current := assignments.current
  if !s.submission.durableGoal ||
      !replaceable current.snapshot.execution.goal.status ||
      current.snapshot.revision != replacement.expected ||
      replacement.nextId.isEmpty || replacement.creationKey.isEmpty ||
      replacement.firstRequest.isEmpty || replacement.nextRevision.document.isEmpty ||
      replacement.nextRevision.composite.isEmpty ||
      replacement.nextRevision.document = current.snapshot.revision.document ||
      (current :: assignments.retained).any (fun prior =>
        prior.id == replacement.nextId || prior.creationKey == replacement.creationKey ||
          prior.rootRequest == replacement.firstRequest ||
          prior.snapshot.revision.document == replacement.nextRevision.document) then none
  else
    let post ← start? { s with submission := ⟨false, false, false, false⟩ }
      requested objective budget facts commit replacement.nextId
    let fresh : Assignment :=
      { id := replacement.nextId, creationKey := replacement.creationKey
        snapshot :=
          { revision := replacement.nextRevision, objective := canonicalObjective objective
            execution :=
              { goal := ⟨.active, 0, false, false⟩, sequence := 0
                lastContinuedFrom := none, latestRequest := 0, children := []
                tokensUsed := 0, tokenBudget := budget.map Int.toNat } }
        rootRequest := replacement.firstRequest, usageBaseline := replacement.sessionUsage
        activeSeconds := 0 }
    some (post, ⟨fresh, current :: assignments.retained⟩)

theorem replacement_keeps_prior_assignments (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (post : State × Assignments)
    (h : replace? s assignments replacement requested objective budget facts commit = some post) :
    post.2.retained = assignments.current :: assignments.retained := by
  dsimp only [replace?] at h
  split at h
  · contradiction
  · cases hs : start? { s with submission := ⟨false, false, false, false⟩ }
        requested objective budget facts commit replacement.nextId with
    | none => simp [hs] at h
    | some started =>
      simp only [hs, Option.bind_some, Option.some.injEq] at h
      cases h
      simp

theorem replacement_resets_only_new_accounting (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (post : State × Assignments)
    (h : replace? s assignments replacement requested objective budget facts commit = some post) :
    post.2.current.snapshot.execution.tokensUsed = 0 ∧
      post.2.current.activeSeconds = 0 ∧
      post.2.current.usageBaseline = replacement.sessionUsage ∧
      post.2.current.rootRequest = replacement.firstRequest := by
  dsimp only [replace?] at h
  split at h
  · contradiction
  · cases hs : start? { s with submission := ⟨false, false, false, false⟩ }
        requested objective budget facts commit replacement.nextId with
    | none => simp [hs] at h
    | some started =>
      simp only [hs, Option.bind_some, Option.some.injEq] at h
      cases h
      simp

theorem stale_replacement_cannot_publish (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (h : assignments.current.snapshot.revision ≠ replacement.expected) :
    replace? s assignments replacement requested objective budget facts commit = none := by
  simp [replace?, h]

theorem running_goal_cannot_be_replaced (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (h : replaceable assignments.current.snapshot.execution.goal.status = false) :
    replace? s assignments replacement requested objective budget facts commit = none := by
  simp [replace?, h]

theorem discarded_replacement_cannot_publish (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) :
    replace? s assignments replacement requested objective budget facts false = none := by
  simp [replace?, discarded_start_is_not_published]

end GoalAutomation.ClientStart
