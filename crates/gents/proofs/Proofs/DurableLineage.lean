import Proofs.Basic
import Proofs.Request.CausalHop

/-! Ingest boundary for durable request lineage: malformed replicated rows are
rejected individually. `requestHop` is provenance only and grants no hierarchy,
cascade or authority over the calling session. -/

namespace DurableLineage

/-- The session-message reader supplies these facts from a canonical session
visible through DefraDB ACP and the caller's current agent-target grants.
Listing and inspection use this same target predicate; neither copied session
labels nor a lineage display grants access outside its requester scope. -/
def sessionTargetAllowed (sameRequester sameNode caused allowedTarget : Bool) : Bool :=
  sameRequester && (sameNode || (caused && allowedTarget))

theorem foreign_requester_target_refused (sameNode caused allowedTarget : Bool) :
    sessionTargetAllowed false sameNode caused allowedTarget = false := by
  simp [sessionTargetAllowed]

theorem same_node_target_needs_no_lineage (caused allowedTarget : Bool) :
    sessionTargetAllowed true true caused allowedTarget = true := by
  simp [sessionTargetAllowed]

theorem remote_target_requires_current_grant (caused : Bool) :
    sessionTargetAllowed true false caused false = false := by
  simp [sessionTargetAllowed]

theorem remote_root_target_refused (allowedTarget : Bool) :
    sessionTargetAllowed true false false allowedTarget = false := by
  simp [sessionTargetAllowed]

structure RawLineage where
  hasParentRequestId : Bool
  hasParentRequestDocId : Bool
  hasParentToolCallId : Bool
  hasParentToolCallDocId : Bool
  requestHop : Nat
  requestOnlyControl : Bool
  controlAllowedAtDepthZero : Bool := false
  deriving DecidableEq, Repr

def pairCoherent (logical physical : Bool) : Bool := logical == physical

def edgePairsCoherent (row : RawLineage) : Bool :=
  pairCoherent row.hasParentRequestId row.hasParentRequestDocId &&
    pairCoherent row.hasParentToolCallId row.hasParentToolCallDocId

def parentShapeCoherent (row : RawLineage) : Bool :=
  let root := !row.hasParentRequestId && !row.hasParentToolCallId
  let bridge := row.hasParentRequestId && row.hasParentToolCallId
  let control :=
    row.requestOnlyControl && row.hasParentRequestId && !row.hasParentToolCallId
  root || bridge || control

def depthCoherent (row : RawLineage) : Bool :=
  if row.hasParentRequestId then
    row.requestHop > 0 ||
      (row.requestOnlyControl && row.controlAllowedAtDepthZero)
  else
    row.requestHop == 0

def admissible (row : RawLineage) : Bool :=
  edgePairsCoherent row && parentShapeCoherent row && depthCoherent row

def admissibleRows (rows : List RawLineage) : List RawLineage :=
  rows.filter admissible

theorem malformed_head_does_not_poison
    (bad : RawLineage)
    (rest : List RawLineage)
    (hBad : admissible bad = false) :
    admissibleRows (bad :: rest) = admissibleRows rest := by
  simp [admissibleRows, hBad]

def controlContinuation (requestHop : Nat) : RawLineage :=
  { hasParentRequestId := true
  , hasParentRequestDocId := true
  , hasParentToolCallId := false
  , hasParentToolCallDocId := false
  , requestHop
  , requestOnlyControl := true
  , controlAllowedAtDepthZero := true
  }

theorem control_continuation_is_admissible (depth : Nat) :
    admissible (controlContinuation depth) = true := by
  simp [admissible, edgePairsCoherent, pairCoherent, parentShapeCoherent,
    depthCoherent, controlContinuation]

inductive Delivery where
  | request
  | steering
  deriving DecidableEq, Repr

/-- Deliveries carry the full calling edge; a steering continuation is queued
after the busy session's active request, which orders the queue and is never
its origin. -/
structure SessionMessageWrite where
  lineage : RawLineage
  queuedAfterActive : Bool
  deriving DecidableEq, Repr

def sessionMessageWrite (delivery : Delivery) (callerHop own : Nat) : SessionMessageWrite :=
  { lineage :=
      { hasParentRequestId := true
      , hasParentRequestDocId := true
      , hasParentToolCallId := true
      , hasParentToolCallDocId := true
      , requestHop := CausalHop.nextHop (.crossSession callerHop) own
      , requestOnlyControl := false }
  , queuedAfterActive := delivery == .steering }

theorem session_message_write_names_its_caller (delivery : Delivery) (callerHop own : Nat) :
    admissible (sessionMessageWrite delivery callerHop own).lineage = true ∧
      (sessionMessageWrite delivery callerHop own).lineage.hasParentToolCallDocId = true := by
  simp [sessionMessageWrite, admissible, edgePairsCoherent, pairCoherent,
    parentShapeCoherent, depthCoherent, CausalHop.nextHop]
  omega

/-- A plugin job's child spawn is a session-message request: the same calling
    edge and hop, written by the same single writer. -/
def jobSpawnWrite : Nat → Nat → SessionMessageWrite :=
  sessionMessageWrite .request

/-- A plugin job's message to a child session is a session message, delivered
    as a new request or as a steering continuation like `agent_message`. -/
def jobMessageWrite : Delivery → Nat → Nat → SessionMessageWrite :=
  sessionMessageWrite

/-- One calling tool call may name many requests: a job's spawns and messages
    all carry the job call's edge, and ingest admits every one of them. -/
theorem many_requests_one_call_admissible (delivery : Delivery) (callerHop own n : Nat) :
    admissibleRows (List.replicate n (sessionMessageWrite delivery callerHop own).lineage) =
      List.replicate n (sessionMessageWrite delivery callerHop own).lineage := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [admissibleRows, List.replicate_succ, List.filter_cons,
      (session_message_write_names_its_caller delivery callerHop own).1] at ih ⊢
    simp [ih]

/-! ## Opening request

The opening request of a caused session is read by document key, never by a
tool name, so it is the same for session-message and job children. -/

/-- A request as the opening-request reader sees it: its creation time, its
    document id, its session, whether it is public, the document ids of its
    calling request and tool call, and the opening request its session
    recorded. -/
structure RequestRow where
  createdAt : Nat
  docId : String
  session : String
  isPublic : Bool
  causeRequestDoc : Option String
  causeToolCallDoc : Option String
  /-- The opening request its session's provenance records
      (`AgentSession.Provenance.openingRequestDoc`, written once when the
      session is created), joined by the reader onto every request row of
      that session; `none` for a session created before it was recorded. -/
  sessionOpening : Option String := none
  deriving DecidableEq, Repr

/-- `a` sorts at or before `b` by `(createdAt, docId)`. -/
def keyLe (a b : RequestRow) : Prop :=
  a.createdAt < b.createdAt ∨ (a.createdAt = b.createdAt ∧ a.docId ≤ b.docId)

instance (a b : RequestRow) : Decidable (keyLe a b) := by
  unfold keyLe; infer_instance

theorem keyLe_refl (a : RequestRow) : keyLe a a :=
  Or.inr ⟨rfl, String.le_refl _⟩

theorem keyLe_trans {a b c : RequestRow} (hab : keyLe a b) (hbc : keyLe b c) :
    keyLe a c := by
  unfold keyLe at *
  rcases hab with hab | ⟨hab, hab'⟩ <;> rcases hbc with hbc | ⟨hbc, hbc'⟩
  · exact Or.inl (by omega)
  · exact Or.inl (by omega)
  · exact Or.inl (by omega)
  · exact Or.inr ⟨by omega, String.le_trans hab' hbc'⟩

theorem keyLe_total (a b : RequestRow) : keyLe a b ∨ keyLe b a := by
  unfold keyLe
  rcases Nat.lt_trichotomy a.createdAt b.createdAt with h | h | h
  · exact Or.inl (Or.inl h)
  · rcases String.le_total a.docId b.docId with h' | h'
    · exact Or.inl (Or.inr ⟨h, h'⟩)
    · exact Or.inr (Or.inr ⟨h.symm, h'⟩)
  · exact Or.inr (Or.inl h)

theorem keyLe_antisymm {a b : RequestRow} (hab : keyLe a b) (hba : keyLe b a) :
    a.createdAt = b.createdAt ∧ a.docId = b.docId := by
  unfold keyLe at *
  rcases hab with hab | ⟨hab, hab'⟩ <;> rcases hba with hba | ⟨hba, hba'⟩
  · omega
  · omega
  · omega
  · exact ⟨hab, String.le_antisymm hab' hba'⟩

/-- The row sorting first by `(createdAt, docId)`. -/
def earliest : List RequestRow → Option RequestRow
  | [] => none
  | r :: rs =>
    match earliest rs with
    | none => some r
    | some e => if keyLe r e then some r else some e

theorem earliest_spec : ∀ {rows : List RequestRow} {e : RequestRow},
    earliest rows = some e → e ∈ rows ∧ ∀ x ∈ rows, keyLe e x
  | [], _, h => by simp [earliest] at h
  | r :: rs, e, h => by
    simp only [earliest] at h
    split at h
    · rename_i h_none
      cases rs with
      | nil =>
        cases h
        exact ⟨List.mem_singleton_self _, fun x hx => by
          rw [List.mem_singleton.1 hx]; exact keyLe_refl _⟩
      | cons y ys =>
        exfalso
        revert h_none
        simp only [earliest]
        split <;> (try split) <;> simp
    · rename_i e' h_some
      have ih := earliest_spec h_some
      split at h
      · rename_i h_le
        cases h
        refine ⟨List.mem_cons_self _ _, fun x hx => ?_⟩
        rcases List.mem_cons.1 hx with rfl | hx
        · exact keyLe_refl _
        · exact keyLe_trans h_le (ih.2 x hx)
      · rename_i h_le
        cases h
        refine ⟨List.mem_cons_of_mem _ ih.1, fun x hx => ?_⟩
        rcases List.mem_cons.1 hx with rfl | hx
        · exact (keyLe_total _ _).resolve_left h_le
        · exact ih.2 x hx

/-- A public request of `session` caused by the request `cause` through a tool
    call. No tool name takes part. -/
def isOpeningCandidate (cause session : String) (r : RequestRow) : Bool :=
  r.session == session && r.isPublic && r.causeRequestDoc == some cause &&
    r.causeToolCallDoc.isSome

/-- The opening request the candidates' session recorded at its creation. -/
def recordedOpening (candidates : List RequestRow) : Option String :=
  candidates.findSome? (·.sessionOpening)

/-- The opening request of a caused session. Among the public requests in
    `session` whose calling request is `cause` and whose tool-call edge is
    set, it is the one the session recorded at its creation, and none while
    that request is not visible. A session without a recorded opening
    request falls back to the earliest candidate by `(createdAt, docId)`.
    The recorded fact decides, not the clock: writers stamp whole seconds. -/
def openingRequest (rows : List RequestRow) (cause session : String) : Option RequestRow :=
  let candidates := rows.filter (isOpeningCandidate cause session)
  match recordedOpening candidates with
  | some doc => candidates.find? (·.docId == doc)
  | none => earliest candidates

private theorem openingRequest_of_unrecorded {rows : List RequestRow} {cause session : String}
    (h : recordedOpening (rows.filter (isOpeningCandidate cause session)) = none) :
    openingRequest rows cause session =
      earliest (rows.filter (isOpeningCandidate cause session)) := by
  simp only [openingRequest, h]

private theorem openingRequest_of_recorded {rows : List RequestRow} {cause session doc : String}
    (h : recordedOpening (rows.filter (isOpeningCandidate cause session)) = some doc) :
    openingRequest rows cause session =
      (rows.filter (isOpeningCandidate cause session)).find? (·.docId == doc) := by
  simp only [openingRequest, h]

private theorem openingRequest_mem {rows : List RequestRow} {cause session : String}
    {r : RequestRow} (h : openingRequest rows cause session = some r) :
    r ∈ rows.filter (isOpeningCandidate cause session) := by
  cases h_rec : recordedOpening (rows.filter (isOpeningCandidate cause session)) with
  | none => rw [openingRequest_of_unrecorded h_rec] at h; exact (earliest_spec h).1
  | some _ => rw [openingRequest_of_recorded h_rec] at h; exact List.mem_of_find?_eq_some h

/-- Without a recorded opening request, document ids identify documents, so
    the opening request is the one candidate nothing sorts before. -/
theorem opening_request_unique (rows : List RequestRow) (cause session : String)
    (r r' : RequestRow)
    (h_ids : ∀ a ∈ rows, ∀ b ∈ rows, a.docId = b.docId → a = b)
    (h_unrecorded : recordedOpening (rows.filter (isOpeningCandidate cause session)) = none)
    (h_open : openingRequest rows cause session = some r)
    (h_mem : r' ∈ rows) (h_cand : isOpeningCandidate cause session r' = true)
    (h_le : keyLe r' r) :
    r' = r := by
  rw [openingRequest_of_unrecorded h_unrecorded] at h_open
  have h := earliest_spec h_open
  have h_r := (List.mem_filter.1 h.1).1
  have h_ge := h.2 r' (List.mem_filter.2 ⟨h_mem, h_cand⟩)
  exact h_ids r' h_mem r h_r (keyLe_antisymm h_le h_ge).2

/-- A session that recorded its opening request opens at it, whatever the
    clocks of the other candidates. -/
theorem recorded_opening_request (rows : List RequestRow) (cause session : String)
    (r : RequestRow)
    (h_ids : ∀ a ∈ rows, ∀ b ∈ rows, a.docId = b.docId → a = b)
    (h_mem : r ∈ rows) (h_cand : isOpeningCandidate cause session r = true)
    (h_rec : recordedOpening (rows.filter (isOpeningCandidate cause session)) = some r.docId) :
    openingRequest rows cause session = some r := by
  rw [openingRequest_of_recorded h_rec]
  cases h_find : (rows.filter (isOpeningCandidate cause session)).find? (·.docId == r.docId) with
  | none =>
    have := List.find?_eq_none.1 h_find r (List.mem_filter.2 ⟨h_mem, h_cand⟩)
    simp at this
  | some x =>
    have h_x := (List.mem_filter.1 (List.mem_of_find?_eq_some h_find)).1
    have h_doc : x.docId = r.docId := by simpa using List.find?_some h_find
    rw [h_ids x h_x r h_mem h_doc]

/-- Without a recorded opening request, a message sorting after the opening
    request never replaces it. `h_later` is the step order: admission
    delivers a message only to an attempt an earlier step spawned
    (`PluginJob.messages_bounded_and_uncharged`). -/
theorem unrecorded_message_after_opening (rows : List RequestRow) (cause session : String)
    (r m : RequestRow)
    (h_unrecorded : recordedOpening (rows.filter (isOpeningCandidate cause session)) = none)
    (h_open : openingRequest rows cause session = some r)
    (h_m : m.sessionOpening = none)
    (h_later : keyLe r m) (h_new : m.docId ≠ r.docId) :
    openingRequest (m :: rows) cause session = some r := by
  rw [openingRequest_of_unrecorded h_unrecorded] at h_open
  have h_unrec' :
      recordedOpening ((m :: rows).filter (isOpeningCandidate cause session)) = none := by
    by_cases h_cand : isOpeningCandidate cause session m = true
    · simpa [recordedOpening, List.filter_cons, h_cand, h_m] using h_unrecorded
    · simpa [recordedOpening, List.filter_cons, h_cand] using h_unrecorded
  rw [openingRequest_of_unrecorded h_unrec']
  by_cases h_cand : isOpeningCandidate cause session m = true
  · have h_not : ¬ keyLe m r := fun h => h_new (keyLe_antisymm h h_later).2
    simp [List.filter_cons, h_cand, earliest, h_open, h_not]
  · simp [List.filter_cons, h_cand, h_open]

/-- A message lands another request in the same child session with the same
    edge. When the session recorded its opening request, the message never
    replaces it, even in the same second or with an earlier clock. -/
theorem message_request_never_opening (rows : List RequestRow) (cause session : String)
    (r m : RequestRow)
    (h_open : openingRequest rows cause session = some r)
    (h_recorded : r.sessionOpening = some r.docId)
    (h_joined : m.sessionOpening = r.sessionOpening)
    (h_new : m.docId ≠ r.docId) :
    openingRequest (m :: rows) cause session = some r := by
  have h_rc := openingRequest_mem h_open
  have h_rec :
      recordedOpening (rows.filter (isOpeningCandidate cause session)) = some r.docId := by
    cases h : recordedOpening (rows.filter (isOpeningCandidate cause session)) with
    | none =>
      have := List.findSome?_eq_none_iff.1 h r h_rc
      simp [h_recorded] at this
    | some d =>
      rw [openingRequest_of_recorded h] at h_open
      have h_doc : r.docId = d := by simpa using List.find?_some h_open
      rw [h_doc]
  have h_find :
      (rows.filter (isOpeningCandidate cause session)).find? (·.docId == r.docId) = some r := by
    rw [← openingRequest_of_recorded h_rec]; exact h_open
  by_cases h_cand : isOpeningCandidate cause session m = true
  · have h_rec' :
        recordedOpening ((m :: rows).filter (isOpeningCandidate cause session)) =
          some r.docId := by
      simp [recordedOpening, List.filter_cons, h_cand, h_joined, h_recorded]
    rw [openingRequest_of_recorded h_rec']
    simp [List.filter_cons, h_cand, h_new, h_find]
  · simpa [openingRequest, List.filter_cons, h_cand] using h_open

/-- A re-attempt spawns into a new session: its request opens that session and
    leaves the opening request of every other session unchanged. -/
theorem reattempt_opens_new_session (rows : List RequestRow) (cause session : String)
    (a : RequestRow)
    (h_fresh : ∀ x ∈ rows, x.session ≠ a.session)
    (h_other : a.session ≠ session)
    (h_cand : isOpeningCandidate cause a.session a = true)
    (h_own : a.sessionOpening = none ∨ a.sessionOpening = some a.docId) :
    openingRequest (a :: rows) cause session = openingRequest rows cause session ∧
      openingRequest (a :: rows) cause a.session = some a := by
  have h_none : rows.filter (isOpeningCandidate cause a.session) = [] := by
    rw [List.filter_eq_nil_iff]
    intro x hx
    simp [isOpeningCandidate, h_fresh x hx]
  constructor
  · simp [openingRequest, List.filter_cons, isOpeningCandidate, h_other]
  · rcases h_own with h_own | h_own
    · simp [openingRequest, recordedOpening, List.filter_cons, h_cand, h_none, earliest, h_own]
    · simp [openingRequest, recordedOpening, List.filter_cons, h_cand, h_none, h_own]

/-- The public requests a calling tool call names. A session-message call
    (`agent_new`, `agent_message`) writes exactly one; a job call may write
    many. -/
def callRequests (rows : List RequestRow) (call : String) : List RequestRow :=
  rows.filter (fun r => r.isPublic && r.causeToolCallDoc == some call)

/-- The single-request projection of a call: its request when it names exactly
    one, otherwise nothing. -/
def singleRequestProjection (rows : List RequestRow) (call : String) : Option RequestRow :=
  match callRequests rows call with
  | [r] => some r
  | _ => none

/-- The request rows a session-message call (`agent_new`, `agent_message`)
    writes: its one `sessionMessageWrite` lands one public request naming the
    call. -/
def sessionMessageRows (call : String) (r : RequestRow) : List RequestRow :=
  [{ r with isPublic := true, causeToolCallDoc := some call }]

/-- The request rows a job call writes: each spawn or message is one more
    public request naming the same call. -/
def jobRows (call : String) (rs : List RequestRow) : List RequestRow :=
  rs.map fun r => { r with isPublic := true, causeToolCallDoc := some call }

/-- Session-message calls keep the single-request projection: one call, one
    request. Whatever other calls wrote, the call projects to the one request
    its write landed. -/
theorem single_request_projection_for_session_message_calls
    (call : String) (r : RequestRow) (others : List RequestRow)
    (h_others : ∀ x ∈ others, x.causeToolCallDoc ≠ some call) :
    singleRequestProjection (sessionMessageRows call r ++ others) call =
      some { r with isPublic := true, causeToolCallDoc := some call } := by
  have h_none : others.filter (fun x => x.isPublic && x.causeToolCallDoc == some call) = [] := by
    rw [List.filter_eq_nil_iff]
    intro x hx
    simp [h_others x hx]
  simp [singleRequestProjection, callRequests, sessionMessageRows, List.filter_append, h_none]

/-- A job call that wrote two requests has no single-request projection. -/
theorem two_job_requests_have_no_single_projection (call : String) (a b : RequestRow) :
    singleRequestProjection (jobRows call [a, b]) call = none := by
  simp [singleRequestProjection, callRequests, jobRows]

inductive TriggerOrigin where
  | event | schedule | manual | goal | unknown
  deriving DecidableEq, Repr

/-- The reader supplies creation only when the session's immutable provenance
names this exact request in the same node, session and requester scope. A later
trigger and a Goal continuation are not evidence that the session was created
by an automated Task trigger. -/
structure TaskTriggerEvidence where
  exactScope : Bool
  isCreationRequest : Bool
  hasTriggerId : Bool
  hasTriggerDoc : Bool
  kind : TriggerOrigin
  deriving DecidableEq, Repr

def verifiedTaskTrigger (e : TaskTriggerEvidence) : Bool :=
  e.exactScope && e.isCreationRequest && e.hasTriggerId && e.hasTriggerDoc &&
    (e.kind == .event || e.kind == .schedule)

theorem later_request_is_not_creation_evidence (e : TaskTriggerEvidence)
    (h : e.isCreationRequest = false) : verifiedTaskTrigger e = false := by
  simp [verifiedTaskTrigger, h]

theorem goal_continuation_is_not_task_trigger (e : TaskTriggerEvidence)
    (h : e.kind = .goal) : verifiedTaskTrigger e = false := by
  simp [verifiedTaskTrigger, h]

theorem foreign_request_scope_is_not_creation_evidence (e : TaskTriggerEvidence)
    (h : e.exactScope = false) : verifiedTaskTrigger e = false := by
  simp [verifiedTaskTrigger, h]

/-- A task-trigger grant requires the session's verified creation request,
not a later request delivered to an existing session. The caller's starter
scope is resolved separately; foreign sessions gain no trigger-based grant. -/
def interruptAllowed (callerSession targetSession : String)
    (targetOriginCause : Option String) (sameNode : Bool := false)
    (taskTriggeredAtCreation : Bool := false) (hasParentDoc : Bool := false) : Bool :=
  CausalHop.sendTargetAllowed callerSession targetSession &&
    (targetOriginCause == some callerSession ||
      (targetOriginCause.isNone && !hasParentDoc && sameNode && taskTriggeredAtCreation))

theorem non_spawner_interrupt_refused (callerSession targetSession : String)
    (targetOriginCause : Option String)
    (h : targetOriginCause ≠ some callerSession) :
    interruptAllowed callerSession targetSession targetOriginCause = false := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, h]

theorem spawner_may_interrupt (callerSession targetSession : String)
    (h : targetSession ≠ callerSession) :
    interruptAllowed callerSession targetSession (some callerSession) = true := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, Ne.symm h]

theorem root_session_not_interruptible (callerSession targetSession : String) :
    interruptAllowed callerSession targetSession none = false := by
  simp [interruptAllowed]

theorem local_task_trigger_is_interruptible (callerSession targetSession : String)
    (h : targetSession ≠ callerSession) :
    interruptAllowed callerSession targetSession none true true false = true := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed, Ne.symm h]

theorem task_trigger_does_not_authorize_another_agents_child
    (callerSession targetSession : String) (cause : Option String)
    (h : cause ≠ some callerSession) :
    interruptAllowed callerSession targetSession cause true true true = false := by
  simp [interruptAllowed, h]

theorem foreign_trigger_does_not_grant_interrupt (callerSession targetSession : String)
    (cause : Option String) (h : cause ≠ some callerSession) :
    interruptAllowed callerSession targetSession cause false true = false := by
  simp [interruptAllowed, h]

theorem unrelated_non_task_is_not_interruptible (callerSession targetSession : String)
    (cause : Option String) (sameNode : Bool) (h : cause ≠ some callerSession) :
    interruptAllowed callerSession targetSession cause sameNode false = false := by
  simp [interruptAllowed, h]

theorem own_session_never_interruptible (session : String) (cause : Option String)
    (sameNode taskTriggeredAtCreation : Bool) :
    interruptAllowed session session cause sameNode taskTriggeredAtCreation = false := by
  simp [interruptAllowed, CausalHop.sendTargetAllowed]

end DurableLineage
