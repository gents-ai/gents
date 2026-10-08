import Proofs.SessionHydration.State

/-!
# Receiver-side hydration progress

The server commits the exact identities of the transcript documents it served.
The client may complete only when every signed identity is locally merged AND
recomputing the typed canonical closure reconstructs every message and matches
that manifest. Counts remain transport progress, not output validity.
-/

namespace SessionHydration

/-! Collection-qualified identities prevent an equal raw `_docID` from another
collection from satisfying the signed manifest. -/
abbrev DocumentKey := Document

inductive ClientPhase where
  | idle
  | requested
  | serving
  | complete
  | failed
  deriving DecidableEq, Repr

structure ClientProgress where
  session : String := ""
  node : String := ""
  phase : ClientPhase := .idle
  mergedDocuments : Finset DocumentKey := ∅
  servedDocuments : Option (Finset DocumentKey) := none
  deriving DecidableEq

def ClientProgress.mergedCount (progress : ClientProgress) : Nat :=
  progress.mergedDocuments.card

def ClientProgress.servedCount (progress : ClientProgress) : Option Nat :=
  progress.servedDocuments.map Finset.card

/-- Documents in the signed server manifest that the receiver has actually
observed. Unlike `mergedCount`, this is a valid numerator for `servedCount`. -/
def ClientProgress.coveredCount (progress : ClientProgress) : Nat :=
  match progress.servedDocuments with
  | some served => (progress.mergedDocuments ∩ served).card
  | none => progress.mergedDocuments.card

theorem coveredCount_le_servedCount (progress : ClientProgress)
    (served : Finset DocumentKey) (hserved : progress.servedDocuments = some served) :
    progress.coveredCount ≤ served.card := by
  simp only [ClientProgress.coveredCount, hserved]
  exact Finset.card_le_card (Finset.inter_subset_right)

inductive ValidationResult where
  | loading
  | valid
  | invalid
  deriving DecidableEq, Repr

def hydrationErrorIsLoading : CanonicalOutput.Hydration.Error → Bool
  | .headerUnavailable | .provenanceMissing _ => true
  | .reference (.lookup .unavailable) => true
  | .reference (.extent (.missingOrdinal _)) => true
  | .terminal .missingHeader => true
  | _ => false

def canonicalKey (key : CanonicalOutput.Hydration.DocumentKey) : DocumentKey :=
  ⟨key.collection, key.id⟩

/-- The signed receipt remains the ACP/transport premise. Local closure
recomputation is an independent exact-content check before completion. -/
def validateSnapshot (scope : CanonicalOutput.Hydration.AuthorizationScope)
    (rootSession : SessionId)
    (signed : Finset DocumentKey)
    (bases : List CanonicalOutput.Hydration.AuthorizedBase)
    (roots : List CanonicalOutput.DocId)
    (requirements : List CanonicalOutput.Hydration.TerminalRequirement)
    (messages : List CanonicalOutput.MessageEnvelope)
    (segments : List CanonicalOutput.Segment)
    (provenance : List CanonicalOutput.Hydration.ProvenanceAccess)
    (deniedHeaders deniedSegments : List CanonicalOutput.DocId)
    (dependencyDenials : List CanonicalOutput.DependencyDenial := []) : ValidationResult :=
  match CanonicalOutput.Hydration.buildManifest scope rootSession bases roots requirements messages segments
      provenance deniedHeaders deniedSegments dependencyDenials with
  | .ok manifest =>
      if (manifest.map canonicalKey).toFinset = signed then .valid else .invalid
  | .error error => if hydrationErrorIsLoading error then .loading else .invalid

namespace ValidationExamples

open CanonicalOutput.Hydration

def signedClosure : Finset DocumentKey :=
  [⟨.agentRequest, 10⟩, ⟨.agentMessage, 200⟩, ⟨.agentMessage, 201⟩,
    ⟨.agentOutputSegment, 100⟩].toFinset

example : validateSnapshot Examples.scope 2 signedClosure [] [201] []
    [Examples.originMessage, Examples.childMessage] [Examples.closing]
    Examples.access [] [] = .valid := by native_decide

/-- All IDs may be present while the signed manifest still disagrees with the
typed reference closure. Identity coverage alone cannot complete. -/
example : validateSnapshot Examples.scope 2 (signedClosure.erase ⟨.agentRequest, 10⟩) [] [201] []
    [Examples.originMessage, Examples.childMessage] [Examples.closing]
    Examples.access [] [] = .invalid := by native_decide

example : validateSnapshot Examples.scope 2 signedClosure [] [201] []
    [Examples.childMessage] [Examples.closing] Examples.access [] [] = .loading := by
  native_decide

example : validateSnapshot Examples.scope 2 signedClosure [] [201] []
    [Examples.originMessage, Examples.childMessage] [Examples.closing]
    [⟨Examples.scope, ⟨.agentMessage, 200⟩, .authorized⟩,
     ⟨Examples.scope, ⟨.agentMessage, 201⟩, .authorized⟩,
     ⟨Examples.scope, ⟨.agentOutputSegment, 100⟩, .authorized⟩,
     ⟨Examples.scope, ⟨.agentRequest, 10⟩, .denied⟩] [] [] = .invalid := by native_decide

end ValidationExamples

def canComplete (merged : Finset DocumentKey)
    (served : Option (Finset DocumentKey)) (validation : ValidationResult) : Bool :=
  match served, validation with
  | some expected, .valid => decide (expected ⊆ merged)
  | _, _ => false

def mergeServed (prev next : Option (Finset DocumentKey)) : Option (Finset DocumentKey) :=
  match next with
  | some documents => some documents
  | none => prev

def progressFor (prev : ClientProgress) (session node : String) : ClientProgress :=
  if prev.session = session ∧ prev.node = node then prev
  else { session, node }

/-- An explicit request starts a fresh receiver attempt for this target. -/
def beginRequest (session node : String) : ClientProgress :=
  { session, node, phase := .requested }

/-- An owned session header permits hydration during a live turn. Without that
header, a pending local request alone does not prove the session exists. -/
def canStartInitial (ownedSession hasDocuments nonterminalRequest : Bool) : Bool :=
  ownedSession || (hasDocuments && !nonterminalRequest)

theorem owned_session_can_start_during_turn (hasDocuments nonterminalRequest : Bool) :
    canStartInitial true hasDocuments nonterminalRequest = true := by
  simp [canStartInitial]

theorem pending_request_without_session_waits (hasDocuments : Bool) :
    canStartInitial false hasDocuments true = false := by
  simp [canStartInitial]

/-- A replicated session header whose requester differs from the scope this
reader reads under belongs to that requester. The server's ownership check
refuses such a request, so the receiver never starts one: the header alone is
enough to present the session as unreadable (#2066). -/
def canStart (foreignHeader ownedSession hasDocuments nonterminalRequest : Bool) : Bool :=
  !foreignHeader && canStartInitial ownedSession hasDocuments nonterminalRequest

theorem foreign_header_never_starts (ownedSession hasDocuments nonterminalRequest : Bool) :
    canStart true ownedSession hasDocuments nonterminalRequest = false := by
  simp [canStart]

theorem readable_header_keeps_initial_start
    (ownedSession hasDocuments nonterminalRequest : Bool) :
    canStart false ownedSession hasDocuments nonterminalRequest =
      canStartInitial ownedSession hasDocuments nonterminalRequest := by
  simp [canStart]

/-- An explicit retry is legal only for the same failed target. -/
def canRetry (prev : ClientProgress) (session node : String) : Bool :=
  decide (prev.session = session ∧ prev.node = node ∧ prev.phase = .failed)

theorem canRetry_iff (prev : ClientProgress) (session node : String) :
    canRetry prev session node = true ↔
      prev.session = session ∧ prev.node = node ∧ prev.phase = .failed := by
  simp [canRetry]

def observeCore (prev : ClientProgress) (merged : Finset DocumentKey)
    (served : Option (Finset DocumentKey)) (validation : ValidationResult)
    (failed : Bool) : ClientProgress :=
  if failed || validation == .invalid || decide (prev.phase = .failed) then
    { phase := .failed, mergedDocuments := merged, servedDocuments := served }
  else if canComplete merged served validation then
    { phase := .complete, mergedDocuments := merged, servedDocuments := served }
  else if served.isSome || decide (prev.phase = .serving) ||
      (decide (prev.phase = .requested) && decide (merged.card > 0)) then
    { phase := .serving, mergedDocuments := merged, servedDocuments := served }
  else if decide (prev.phase = .requested) then
    { phase := .requested, mergedDocuments := merged, servedDocuments := served }
  else
    { phase := .idle, mergedDocuments := merged, servedDocuments := served }

def observe (prev : ClientProgress) (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (validation : ValidationResult)
    (failed : Bool)
    (session node : String) : ClientProgress :=
  let base := progressFor prev session node
  { observeCore base
      (base.mergedDocuments ∪ mergedDocuments)
      (mergeServed base.servedDocuments servedDocuments)
      validation failed with session, node }

/-- Durable control-row state for one exact session/node target. -/
inductive DurableRequest where
  | missing
  | pending
  | served (documents : Finset DocumentKey)
  | rejected (documents : Option (Finset DocumentKey))
  deriving DecidableEq

/-- Projecting a snapshot is a pure query over one durable control row plus
the locally merged set. It retains no process-wide receiver state. -/
def projectDurable (request : DurableRequest) (mergedDocuments : Finset DocumentKey)
    (validation : ValidationResult) (session node : String) : ClientProgress :=
  match request with
  | .missing => observe { session, node } mergedDocuments none .loading false session node
  | .pending => observe (beginRequest session node) mergedDocuments none .loading false session node
  | .served documents =>
      observe (beginRequest session node) mergedDocuments (some documents) validation false session node
  | .rejected documents =>
      observe (beginRequest session node) mergedDocuments documents validation true session node

/-- Public receiver boundary: a served receipt cannot supply its own validation
enum. The projection recomputes canonical closure and native reconstruction from
the local snapshot, then applies the lower state projection. -/
def projectCanonicalSnapshot (request : DurableRequest)
    (mergedDocuments : Finset DocumentKey)
    (scope : CanonicalOutput.Hydration.AuthorizationScope)
    (rootSession : SessionId)
    (bases : List CanonicalOutput.Hydration.AuthorizedBase)
    (roots : List CanonicalOutput.DocId)
    (requirements : List CanonicalOutput.Hydration.TerminalRequirement)
    (messages : List CanonicalOutput.MessageEnvelope)
    (segments : List CanonicalOutput.Segment)
    (provenance : List CanonicalOutput.Hydration.ProvenanceAccess)
    (deniedHeaders deniedSegments : List CanonicalOutput.DocId)
    (session node : String)
    (dependencyDenials : List CanonicalOutput.DependencyDenial := []) : ClientProgress :=
  let validation := match request with
    | .served signed => validateSnapshot scope rootSession signed bases roots requirements messages segments
        provenance deniedHeaders deniedSegments dependencyDenials
    | _ => .loading
  projectDurable request mergedDocuments validation session node

theorem canonical_completion_requires_valid_reconstruction
    (signed merged : Finset DocumentKey)
    (scope : CanonicalOutput.Hydration.AuthorizationScope)
    (rootSession : SessionId)
    (bases : List CanonicalOutput.Hydration.AuthorizedBase)
    (roots : List CanonicalOutput.DocId)
    (requirements : List CanonicalOutput.Hydration.TerminalRequirement)
    (messages : List CanonicalOutput.MessageEnvelope)
    (segments : List CanonicalOutput.Segment)
    (provenance : List CanonicalOutput.Hydration.ProvenanceAccess)
    (deniedHeaders deniedSegments : List CanonicalOutput.DocId)
    (session node : String) (dependencyDenials : List CanonicalOutput.DependencyDenial)
    (hcomplete : (projectCanonicalSnapshot (.served signed) merged scope rootSession bases roots requirements
      messages segments provenance deniedHeaders deniedSegments session node
      dependencyDenials).phase = .complete) :
    validateSnapshot scope rootSession signed bases roots requirements messages segments provenance
      deniedHeaders deniedSegments dependencyDenials = .valid := by
  cases hvalidation : validateSnapshot scope rootSession signed bases roots requirements messages segments
      provenance deniedHeaders deniedSegments dependencyDenials with
  | loading =>
      simp [projectCanonicalSnapshot, projectDurable, hvalidation, observe, observeCore,
        canComplete, progressFor, beginRequest, mergeServed] at hcomplete
  | valid => rfl
  | invalid =>
      simp [projectCanonicalSnapshot, projectDurable, hvalidation, observe, observeCore,
        canComplete, progressFor, beginRequest, mergeServed] at hcomplete

theorem projectDurable_exact_target (request : DurableRequest)
    (mergedDocuments : Finset DocumentKey) (validation : ValidationResult)
    (session node : String) :
    (projectDurable request mergedDocuments validation session node).session = session ∧
      (projectDurable request mergedDocuments validation session node).node = node := by
  cases request <;> simp [projectDurable, observe]

theorem projectDurable_rejected_failed (documents : Option (Finset DocumentKey))
    (mergedDocuments : Finset DocumentKey) (validation : ValidationResult)
    (session node : String) :
    (projectDurable (.rejected documents) mergedDocuments validation session node).phase = .failed := by
  simp [projectDurable, observe, observeCore]

theorem observeCore_mergedDocuments (prev : ClientProgress)
    (merged : Finset DocumentKey) (served : Option (Finset DocumentKey))
    (validation : ValidationResult) (failed : Bool) :
    (observeCore prev merged served validation failed).mergedDocuments = merged := by
  unfold observeCore
  split_ifs <;> rfl

theorem observe_mergedDocuments (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (validation : ValidationResult) (failed : Bool)
    (session node : String) :
    (observe prev mergedDocuments servedDocuments validation failed session node).mergedDocuments =
      (progressFor prev session node).mergedDocuments ∪ mergedDocuments := by
  unfold observe
  exact observeCore_mergedDocuments _ _ _ _ _

theorem observe_merged_monotone (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (validation : ValidationResult) (failed : Bool)
    (session node : String) (hsession : prev.session = session)
    (hnode : prev.node = node) :
    prev.mergedDocuments ⊆
      (observe prev mergedDocuments servedDocuments validation failed session node).mergedDocuments := by
  rw [observe_mergedDocuments]
  simp [progressFor, hsession, hnode]

theorem observe_complete_iff_valid (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (session node : String)
    (hprev : (progressFor prev session node).phase ≠ .failed) :
    (observe prev mergedDocuments servedDocuments .valid false session node).phase = .complete ↔
      canComplete ((progressFor prev session node).mergedDocuments ∪ mergedDocuments)
        (mergeServed (progressFor prev session node).servedDocuments servedDocuments)
        .valid = true := by
  unfold observe observeCore
  have hnf : decide ((progressFor prev session node).phase = .failed) = false :=
    decide_eq_false_iff_not.mpr hprev
  simp [hnf]
  split_ifs <;> simp_all

theorem observe_cannot_complete_without_server (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey) (session node : String)
    (hprev : (progressFor prev session node).phase ≠ .failed)
    (hserved : mergeServed (progressFor prev session node).servedDocuments none = none) :
    (observe prev mergedDocuments none .loading false session node).phase ≠ .complete := by
  unfold observe observeCore
  have hnf : decide ((progressFor prev session node).phase = .failed) = false :=
    decide_eq_false_iff_not.mpr hprev
  simp [hnf, hserved, canComplete]
  split_ifs <;> simp

/-- Equal cardinality cannot substitute for exact document identity. -/
theorem equal_count_substitution_fails_closed
    (merged served : Finset DocumentKey) (_ : merged.card = served.card)
    (hmissing : ¬ served ⊆ merged) :
    canComplete merged (some served) .valid = false := by
  simp [canComplete, hmissing]

/-- Locally present transcript rows are not evidence that a hydration request
was started. Only `beginRequest` may move an idle receiver into an in-flight
phase when the server has not supplied a manifest. -/
theorem observe_idle_without_server_stays_idle (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey) (session node : String)
    (hidle : (progressFor prev session node).phase = .idle)
    (hserved : (progressFor prev session node).servedDocuments = none) :
    (observe prev mergedDocuments none .loading false session node).phase = .idle := by
  unfold observe observeCore
  simp [hidle, hserved, mergeServed, canComplete]

/-- A failed receiver is terminal under passive observation. Restarting the
same target requires the explicit `beginRequest` transition. -/
theorem observe_failed_without_begin_stays_failed (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (validation : ValidationResult)
    (session node : String)
    (hfailed : (progressFor prev session node).phase = .failed) :
    (observe prev mergedDocuments servedDocuments validation false session node).phase = .failed := by
  unfold observe observeCore
  simp [hfailed]

/-- Focusing a different session/node starts from an idle empty receiver state. -/
theorem progressFor_other_target_resets (prev : ClientProgress) (session node : String)
    (hdifferent : prev.session ≠ session ∨ prev.node ≠ node) :
    progressFor prev session node = { session, node } := by
  unfold progressFor
  split
  · rename_i hsame
    exact False.elim (hdifferent.elim (fun h => h hsame.1) (fun h => h hsame.2))
  · rfl

/-- Retrying clears a prior terminal receiver state and its old manifest. -/
theorem beginRequest_resets_terminal (session node : String) :
    (beginRequest session node).phase = .requested ∧
    (beginRequest session node).mergedDocuments = ∅ ∧
    (beginRequest session node).servedDocuments = none := by
  simp [beginRequest]

end SessionHydration
