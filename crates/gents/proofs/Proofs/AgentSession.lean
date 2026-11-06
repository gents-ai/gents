import Proofs.Request.State

/-! Durable session owner; Nat identifiers/times abstract validated canonical
strings. Authorization and atomic query/write remain DB adapter obligations.
Read-only session discovery uses the authenticated caller's DefraDB ACP visibility;
node/requester fields identify a selected row, not additional read permissions.
Presentation and provenance never select execution configuration. -/
namespace AgentSession
structure Scope where
  node : Nat
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
structure StarterEvidence where
  source : Scope
  parentRequestDoc : Nat
  deriving DecidableEq, Repr

/-- A receipt is signed by the creating Agent in the transaction that creates
the session. For agent_new that is the requester; the selected Agent is the
worker and may differ. The node identifier is routing data, not the signer. Its
request and causal kind bind the verified opening evidence; a
pre-existing session never receives one from a later request. -/
structure CreationReceipt where
  scope : Scope
  sessionDoc : Nat
  agent : AgentDid
  openingRequestDoc : Nat
  causalKind : Nat
  causeBinding : Nat
  starter : Option StarterEvidence := none
  deriving DecidableEq, Repr

def creationReceiptSigner (agentNew : Bool) (requester : Option AgentDid)
    (selected : AgentDid) : Option AgentDid :=
  if agentNew then requester else some selected

theorem agent_new_receipt_uses_creator (creator worker : AgentDid) :
    creationReceiptSigner true (some creator) worker = some creator := rfl

theorem agent_new_receipt_requires_creator (worker : AgentDid) :
    creationReceiptSigner true none worker = none := rfl

def creationReceiptAtEnsure (existing : Bool) (receipt : CreationReceipt) : Option CreationReceipt :=
  if existing then none else some receipt

theorem existing_session_never_receives_creation_receipt (receipt : CreationReceipt) :
    creationReceiptAtEnsure true receipt = none := by
  simp [creationReceiptAtEnsure]

theorem new_session_receives_creation_receipt (receipt : CreationReceipt) :
    creationReceiptAtEnsure false receipt = some receipt := by
  simp [creationReceiptAtEnsure]

structure Provenance where
  task : Option Nat := none
  parentRequestDoc : Option Nat := none
  /-- The signed calling request carries its source session because the receiver
      need not hold that request. Creation records the complete scope once;
      neither this reference nor its indexed node/session projection grants
      access or creates runtime hierarchy. The absent requester remains exact
      absence, never a wildcard. Older sessions may have no recorded scope. -/
  startedBy : Option Scope := none
  /-- The request whose admission created this session through a
      calling tool call or a Task delivery. It is written once and
      never rewritten (`advance`, `rename` and `setClosed` keep
      `provenance`). `DurableLineage.openingRequest` opens the session at
      it; a session created before it was recorded has none. -/
  openingRequestDoc : Option Nat := none
  fork : Option ForkOrigin := none
  deriving DecidableEq, Repr

/-- The fire is immutable task-delivery evidence. Its canonical identity and
trigger kind are checked by the adapter before it can name a task; a mismatched
scope or ambiguous receipt is never projected as provenance. -/
structure TaskFireEvidence where
  node : Nat
  session : SessionId
  request : Nat
  task : Nat
  valid : Bool
  deriving DecidableEq, Repr

def taskForOpening (node : Nat) (session : SessionId) (request : Nat)
    (fires : List TaskFireEvidence) : Option Nat :=
  match fires with
  | [fire] =>
      if fire.valid && fire.node == node && fire.session == session && fire.request == request
      then some fire.task else none
  | _ => none

theorem ambiguous_task_fire_is_not_provenance (node : Nat) (session : SessionId)
    (request : Nat) (a b : TaskFireEvidence) (rest : List TaskFireEvidence) :
    taskForOpening node session request (a :: b :: rest) = none := by
  simp [taskForOpening]

/-- A starter in this projection has already been verified against the signed
creation receipt. A Task is scoped to its owning node; an unreadable, ambiguous,
or cyclic parent chain does not establish an originating Task. -/
structure TaskLineageRow where
  scope : Scope
  task : Option Nat
  starter : Option Scope
  deriving DecidableEq, Repr

def originatingTask (owner : Nat) (scope : Scope) (rows : List TaskLineageRow)
    (visited : List Scope) : Nat → Option Nat
  | 0 => none
  | fuel + 1 =>
      if scope.node != owner || visited.contains scope then none
      else
        match rows.filter (fun row => row.scope == scope) with
        | [row] =>
            match row.task with
            | some task => some task
            | none => match row.starter with
              | some parent => originatingTask owner parent rows (scope :: visited) fuel
              | none => none
        | _ => none

theorem foreign_task_lineage_is_not_attributed (owner : Nat) (scope : Scope)
    (rows : List TaskLineageRow) (visited : List Scope) (fuel : Nat)
    (different : scope.node ≠ owner) :
    originatingTask owner scope rows visited (fuel + 1) = none := by
  simp [originatingTask, different]

theorem repeated_task_lineage_is_not_attributed (owner : Nat) (scope : Scope)
    (rows : List TaskLineageRow) (visited : List Scope) (fuel : Nat)
    (seen : scope ∈ visited) :
    originatingTask owner scope rows visited (fuel + 1) = none := by
  simp [originatingTask, seen]

def openingRequestAtCreation (request : Nat) (toolCall taskTrigger : Bool) : Option Nat :=
  if toolCall || taskTrigger then some request else none

theorem task_opening_recorded (request : Nat) :
    openingRequestAtCreation request false true = some request := by
  simp [openingRequestAtCreation]
structure RequestObservation where
  docId : Nat
  requestId : RequestId
  state : RequestState
  deriving DecidableEq, Repr
structure Observation where
  activity : Time
  preview : Option String
  latest : Option RequestObservation
  replyPreview : Option String := none
  deriving DecidableEq, Repr
structure Document where
  scope : Scope
  agent : AgentDid
  createdAt : Time
  closedAt : Option Time := none
  stoppedAt : Option Time := none
  archivedAt : Option Time := none
  archiveRevision : Nat := 0
  title : Option Title := none
  tags : List String := []
  provenance : Option Provenance := none
  observation : Option Observation := none
  deriving DecidableEq, Repr

/-- Creation reserves immutable scope and agent selection. A Client submission
ensures a new session and inserts its signed request in one transaction under the
client actor: the session's native document owner is that user. An unattended
agent submission creates its session under that agent. Reuse preserves the complete
document and its native owner, including closed state and presentation metadata.
Runtime admission requires an existing Client session and must never repair a
missing one under agent authority. Atomic commit/rollback and native owner assignment
are obligations of the database transaction adapter, not application ACP checks. -/
def ensure? (existing : Option Document) (requested : Document) : Option Document :=
  match existing with
  | none => some requested
  | some current =>
      if current.scope = requested.scope ∧ current.agent = requested.agent then some current
      else none

theorem ensure_existing_preserves_document (current requested : Document)
    (scope : current.scope = requested.scope) (agent : current.agent = requested.agent) :
    ensure? (some current) requested = some current := by simp [ensure?, scope, agent]

/-- The completed logical admission retains its existing session and identifies
the request/session creators and intended relationships. ACP1 installs the
relationships after document commit; `OwnershipPublication` models that window.
Existing sessions retain owner, title, provenance and creation receipt. -/
structure AdmissionState where
  session : Option Document := none
  sessionOwner : Option Nat := none
  sessionDoc : Option Nat := none
  sessionParent : Option Nat := none
  sessionWorker : Option Nat := none
  receipt : Option CreationReceipt := none
  requestDoc : Option Nat := none
  requestOwner : Option Nat := none
  requestParent : Option Nat := none
  requestWorker : Option Nat := none
  deriving DecidableEq, Repr

structure AdmissionInput where
  requested : Document
  actor : Nat
  sessionDoc : Nat
  agentDoc : Nat
  requestDoc : Nat
  receipt : Option CreationReceipt
  authorized : Bool
  commit : Bool
  deriving DecidableEq, Repr

def admitRequest (before : AdmissionState) (input : AdmissionInput) : AdmissionState :=
  if !input.authorized || !input.commit || (before.session.isSome && before.sessionDoc.isNone)
  then before else
  match ensure? before.session input.requested with
  | none => before
  | some session =>
    let fresh := before.session.isNone
    let sessionDoc := if fresh then input.sessionDoc else before.sessionDoc.getD input.sessionDoc
    { session := some session
      sessionOwner := if fresh then some input.actor else before.sessionOwner
      sessionDoc := some sessionDoc
      sessionParent := if fresh then some input.agentDoc else before.sessionParent
      sessionWorker := if fresh && input.actor != session.agent then some session.agent else before.sessionWorker
      receipt := if fresh then input.receipt else before.receipt
      requestDoc := some input.requestDoc
      requestOwner := some input.actor
      requestParent := some sessionDoc
      requestWorker := if input.actor != session.agent then some session.agent else none }

theorem failed_admission_is_unchanged (before : AdmissionState) (input : AdmissionInput)
    (aborted : input.commit = false) : admitRequest before input = before := by
  simp [admitRequest, aborted]

theorem unauthorized_admission_is_unchanged (before : AdmissionState) (input : AdmissionInput)
    (denied : input.authorized = false) : admitRequest before input = before := by
  simp [admitRequest, denied]

/-- ACP1 creation commits the authenticated owner's document before relation
updates. Missing relationships deny the corresponding inherited access, while
the owner retains native access during recovery. Node-level document-write
grants are a trusted-actors trial premise; they do not enforce per-agent invoke
or target isolation. Recovery retries each owner-authorized grant idempotently
without substituting node authority. -/
structure OwnershipPublication where
  committed : Bool := false
  owner : Option Nat := none
  parent : Bool := false
  worker : Bool := false
  deriving DecidableEq, Repr

def publishOwned (actor : Nat) : OwnershipPublication := ⟨true, some actor, false, false⟩

def grantParent (publication : OwnershipPublication) (actor : Nat) : OwnershipPublication :=
  if publication.committed && publication.owner == some actor then
    { publication with parent := true }
  else publication

def grantWorker (publication : OwnershipPublication) (actor : Nat) : OwnershipPublication :=
  if publication.committed && publication.owner == some actor then
    { publication with worker := true }
  else publication

theorem published_owner_retains_access_before_grants (actor : Nat) :
    (publishOwned actor).owner = some actor ∧
      (publishOwned actor).parent = false ∧ (publishOwned actor).worker = false := by
  exact ⟨rfl, rfl, rfl⟩

theorem owner_can_recover_publication (actor : Nat) :
    let completed := grantWorker (grantParent (publishOwned actor) actor) actor
    completed.parent = true ∧ completed.worker = true := by simp [publishOwned, grantParent, grantWorker]

theorem repeated_grants_are_idempotent (publication : OwnershipPublication) (actor : Nat) :
    grantWorker (grantWorker publication actor) actor = grantWorker publication actor := by
  simp [grantWorker]; split <;> simp_all

/-- The physical opening request is staged first so the creator can bind its
session provenance to that document. Session creation precedes Goal creation;
all three become visible together on commit, or none do on failure. -/
structure GoalAdmission where
  request : Bool := false
  session : Bool := false
  goal : Bool := false
  owner : Option Nat := none
  deriving DecidableEq, Repr

def admitGoal (before : GoalAdmission) (actor : Nat)
    (requestStaged sessionStaged goalStaged commit : Bool) : GoalAdmission :=
  if requestStaged && sessionStaged && goalStaged && commit then
    ⟨true, true, true, some actor⟩
  else before

theorem incomplete_goal_admission_is_unchanged (before : GoalAdmission) (actor : Nat)
    (requestStaged goalStaged commit : Bool) :
    admitGoal before actor requestStaged false goalStaged commit = before := by
  simp [admitGoal]

theorem committed_goal_admission_binds_creator (before : GoalAdmission) (actor : Nat) :
    admitGoal before actor true true true true = ⟨true, true, true, some actor⟩ := by rfl

/-- Indexed display is evidence only when the target receipt is authenticated
    against its selected Agent DID,
    belongs to the physical session and binds the same source, parent and index.
    These observations do not authorize session control. -/
def verifiedStarter (receipt : CreationReceipt) (physical : Nat)
    (source : Scope) (parent : Nat) (indexed : Nat × SessionId)
    (signatureValid : Bool) : Option Scope :=
  if signatureValid && receipt.sessionDoc == physical &&
      receipt.starter == some ⟨source, parent⟩ && indexed == (source.node, source.session)
  then some source else none

theorem verified_starter_rejects_other_physical (receipt : CreationReceipt)
    (physical : Nat) (source : Scope) (parent : Nat) (indexed : Nat × SessionId)
    (valid : Bool) (mismatch : receipt.sessionDoc ≠ physical) :
    verifiedStarter receipt physical source parent indexed valid = none := by
  simp [verifiedStarter, mismatch]

def startedBy (session : Document) : Option Scope :=
  session.provenance.bind (·.startedBy)

/-- Scalar storage indexes are derived from the canonical provenance, not
    independently writable origins. Requester scope remains in provenance. -/
def starterIndex (session : Document) : Option (Nat × SessionId) :=
  (startedBy session).map fun scope => (scope.node, scope.session)

theorem ensure_preserves_starter (current requested : Document)
    (scope : current.scope = requested.scope) (agent : current.agent = requested.agent) :
    (ensure? (some current) requested).bind startedBy = startedBy current := by
  rw [ensure_existing_preserves_document current requested scope agent]
  rfl

theorem starter_index_retains_node (session : Document) (source : Scope)
    (h : startedBy session = some source) :
    starterIndex session = some (source.node, source.session) := by
  simp [starterIndex, h]

/-- The one starter reading of a stored row. The immutable index columns select
    verification; the mutable provenance never does, so removing `startedBy` or
    rewriting `parentRequestDoc` cannot route an indexed row to a weaker lookup.
    An unindexed row has no starter: no signed or immutable field separates a
    row created before the index from a new row whose starter failed
    verification, so a parent pointer alone is never lineage. -/
def observedStarter (indexed : Option (Nat × SessionId)) (receipt : Option CreationReceipt)
    (physical : Nat) (recorded : Option Scope) (parent : Option Nat)
    (signatureValid : Bool) : Option Scope :=
  match indexed, receipt, recorded, parent with
  | some index, some receipt, some source, some parent =>
      verifiedStarter receipt physical source parent index signatureValid
  | _, _, _, _ => none

theorem unindexed_row_has_no_starter (receipt : Option CreationReceipt) (physical : Nat)
    (recorded : Option Scope) (parent : Option Nat) (valid : Bool) :
    observedStarter none receipt physical recorded parent valid = none := rfl

theorem removed_starter_has_no_fallback (indexed : Option (Nat × SessionId))
    (receipt : Option CreationReceipt) (physical : Nat) (parent : Option Nat) (valid : Bool) :
    observedStarter indexed receipt physical none parent valid = none := by
  cases indexed <;> cases receipt <;> cases parent <;> rfl

theorem observed_starter_is_signed (indexed : Option (Nat × SessionId))
    (receipt : Option CreationReceipt) (physical : Nat) (recorded : Option Scope)
    (parent : Option Nat) (valid : Bool) (source : Scope)
    (h : observedStarter indexed receipt physical recorded parent valid = some source) :
    ∃ r p, receipt = some r ∧ r.starter = some ⟨source, p⟩ := by
  cases indexed <;> cases receipt <;> cases recorded <;> cases parent <;>
    simp [observedStarter, verifiedStarter] at h
  case some.some.some.some index r s p =>
    obtain ⟨⟨⟨⟨_, _⟩, hs⟩, _⟩, rfl⟩ := h
    exact ⟨r, p, rfl, hs⟩

theorem ensure_cannot_rebind_agent (current requested : Document)
    (agent : current.agent ≠ requested.agent) : ensure? (some current) requested = none := by
  simp [ensure?, agent]
/-- Authoritative query projection, not another writable request/index. -/
structure RequestFact where
  purpose : RequestPurpose
  scope : Scope
  agent : AgentDid
  createdAt : Time
  observed : RequestObservation
  deriving DecidableEq, Repr
def newer (a b : RequestFact) : Bool :=
  a.createdAt > b.createdAt ||
    (a.createdAt == b.createdAt && a.observed.requestId > b.observed.requestId)
def newest : List RequestFact → Option RequestFact
  | [] => none
  | row :: rest => match newest rest with
    | none => some row
    | some previous => if newer row previous then some row else some previous

/-- `none` queries all requesters; `some none` queries exact absent scope. -/
def latest (rows : List RequestFact) (node session : Nat)
    (requester : Option (Option Nat)) : Option RequestFact :=
  newest (rows.filter fun r => r.purpose == .normal && r.scope.node == node && r.scope.session == session &&
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

theorem latest_scope (rows : List RequestFact) (node session : Nat)
    (requester : Option (Option Nat)) (r : RequestFact)
    (h : latest rows node session requester = some r) :
    r ∈ rows ∧ r.scope.node = node ∧ r.scope.session = session ∧
      requester.all (fun scope => r.scope.requester == scope) = true := by
  have hm := newest_member _ r h
  simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq, and_assoc] at hm
  exact ⟨hm.1, hm.2.2⟩

theorem latest_is_normal (rows : List RequestFact) (node session : Nat)
    (requester : Option (Option Nat)) (r : RequestFact)
    (h : latest rows node session requester = some r) : r.purpose = .normal := by
  have hm := newest_member _ r h
  simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq, and_assoc] at hm
  exact hm.2.1

theorem latest_maximal (rows : List RequestFact) (node session : Nat)
    (requester : Option (Option Nat)) (r candidate : RequestFact)
    (h : latest rows node session requester = some r)
    (hm : candidate ∈ rows) (ha : candidate.scope.node = node)
    (hs : candidate.scope.session = session)
    (hp : candidate.purpose = .normal)
    (hr : requester.all (fun scope => candidate.scope.requester == scope) = true) :
    candidate.createdAt < r.createdAt ∨
      (candidate.createdAt = r.createdAt ∧ candidate.observed.requestId ≤ r.observed.requestId) := by
  apply newest_maximal _ r h candidate
  simp [List.mem_filter, hm, ha, hs, hp, hr]

theorem title_arrival_preserves_latest (rows : List RequestFact) (title : RequestFact)
    (node session : Nat) (requester : Option (Option Nat))
    (hp : title.purpose = .titleAudit) :
    latest (title :: rows) node session requester = latest rows node session requester := by
  simp [latest, hp]

theorem latest_exact_of_session_winner (rows : List RequestFact) (node session : Nat)
    (r : RequestFact) (h : latest rows node session none = some r) :
    latest rows node session (some r.scope.requester) = some r := by
  have hf := newest_filter_preserves
    (rows.filter fun row => row.purpose == .normal && row.scope.node == node && row.scope.session == session)
    r (fun row => row.scope.requester == r.scope.requester) (by simpa [latest] using h) (by simp)
  simpa [latest, List.filter_filter, Bool.and_assoc, Bool.and_comm, Bool.and_left_comm] using hf

/-- Run with complete authoritative rows inside the admission/successor transaction,
including the currently indexed request. `preview` is whitespace-normalized by the
existing projection adapter before this character-bound operation. -/
def advance (s : Document) (rows : List RequestFact) (r : RequestFact)
    (preview : String) (now : Time) : Document :=
  if r.scope = s.scope ∧ r.agent = s.agent ∧
      r ∈ rows ∧ latest rows s.scope.node s.scope.session (some s.scope.requester) = some r then
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
    have hn := latest_is_normal rows s.scope.node s.scope.session
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

/-- `reply` is reconstructed from the exact terminal output selected by the
authoritative request. The adapter supplies only visible assistant text; missing
or unreadable output remains absent. It is presentation, never work authority. -/
def observeReply (o : Observation) (current : RequestObservation)
    (reply : Option String) : Observation :=
  if o.latest.any (fun latest => latest.docId == current.docId && latest.requestId == current.requestId)
      && decide (HasTerminal.isTerminal current.state) then
    { o with replyPreview := reply.map (fun text => String.mk (text.toList.take 240)) }
  else o

theorem nonterminal_reply_preserves_observation (o : Observation)
    (current : RequestObservation) (reply : Option String)
    (h : ¬ HasTerminal.isTerminal current.state) : observeReply o current reply = o := by
  simp [observeReply, h]

theorem reply_preserves_request_identity (o : Observation)
    (current : RequestObservation) (reply : Option String) :
    (observeReply o current reply).latest = o.latest := by
  unfold observeReply
  split <;> rfl

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
