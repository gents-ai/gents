import Proofs.SessionHydration.State

/-!
# Receiver-side hydration progress

The server commits the exact identities of the transcript documents it served.
The client may complete only when every identity in that manifest exists in its
locally merged set. Counts remain a UI projection; equal counts are not proof of
document identity. An empty served manifest completes immediately.
-/

namespace SessionHydration

abbrev DocumentKey := String

inductive ClientPhase where
  | idle
  | requested
  | serving
  | complete
  | failed
  deriving DecidableEq, Repr

structure ClientProgress where
  session : String := ""
  agent : String := ""
  phase : ClientPhase := .idle
  mergedDocuments : Finset DocumentKey := ∅
  servedDocuments : Option (Finset DocumentKey) := none
  deriving DecidableEq

def ClientProgress.mergedCount (progress : ClientProgress) : Nat :=
  progress.mergedDocuments.card

def ClientProgress.servedCount (progress : ClientProgress) : Option Nat :=
  progress.servedDocuments.map Finset.card

def canComplete (merged : Finset DocumentKey)
    (served : Option (Finset DocumentKey)) : Bool :=
  match served with
  | some expected => decide (expected ⊆ merged)
  | none => false

def mergeServed (prev next : Option (Finset DocumentKey)) : Option (Finset DocumentKey) :=
  match next with
  | some documents => some documents
  | none => prev

def progressFor (prev : ClientProgress) (session agent : String) : ClientProgress :=
  if prev.session = session ∧ prev.agent = agent then prev
  else { session, agent }

/-- An explicit request starts a fresh receiver attempt for this target. -/
def beginRequest (session agent : String) : ClientProgress :=
  { session, agent, phase := .requested }

/-- An explicit retry is legal only for the same failed target. -/
def canRetry (prev : ClientProgress) (session agent : String) : Bool :=
  decide (prev.session = session ∧ prev.agent = agent ∧ prev.phase = .failed)

theorem canRetry_iff (prev : ClientProgress) (session agent : String) :
    canRetry prev session agent = true ↔
      prev.session = session ∧ prev.agent = agent ∧ prev.phase = .failed := by
  simp [canRetry]

def observeCore (prev : ClientProgress) (merged : Finset DocumentKey)
    (served : Option (Finset DocumentKey)) (failed : Bool) : ClientProgress :=
  if failed || decide (prev.phase = .failed) then
    { phase := .failed, mergedDocuments := merged, servedDocuments := served }
  else if canComplete merged served then
    { phase := .complete, mergedDocuments := merged, servedDocuments := served }
  else if served.isSome || decide (prev.phase = .serving) ||
      (decide (prev.phase = .requested) && decide (merged.card > 0)) then
    { phase := .serving, mergedDocuments := merged, servedDocuments := served }
  else if decide (prev.phase = .requested) then
    { phase := .requested, mergedDocuments := merged, servedDocuments := served }
  else
    { phase := .idle, mergedDocuments := merged, servedDocuments := served }

def observe (prev : ClientProgress) (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (failed : Bool)
    (session agent : String) : ClientProgress :=
  let base := progressFor prev session agent
  { observeCore base
      (base.mergedDocuments ∪ mergedDocuments)
      (mergeServed base.servedDocuments servedDocuments)
      failed with session, agent }

/-- Durable control-row state for one exact session/agent target. -/
inductive DurableRequest where
  | missing
  | pending
  | served (documents : Finset DocumentKey)
  | rejected (documents : Option (Finset DocumentKey))
  deriving DecidableEq

/-- Projecting a snapshot is a pure query over one durable control row plus
the locally merged set. It retains no process-wide receiver state. -/
def projectDurable (request : DurableRequest) (mergedDocuments : Finset DocumentKey)
    (session agent : String) : ClientProgress :=
  match request with
  | .missing => observe { session, agent } mergedDocuments none false session agent
  | .pending => observe (beginRequest session agent) mergedDocuments none false session agent
  | .served documents =>
      observe (beginRequest session agent) mergedDocuments (some documents) false session agent
  | .rejected documents =>
      observe (beginRequest session agent) mergedDocuments documents true session agent

theorem projectDurable_exact_target (request : DurableRequest)
    (mergedDocuments : Finset DocumentKey) (session agent : String) :
    (projectDurable request mergedDocuments session agent).session = session ∧
      (projectDurable request mergedDocuments session agent).agent = agent := by
  cases request <;> simp [projectDurable, observe]

theorem projectDurable_rejected_failed (documents : Option (Finset DocumentKey))
    (mergedDocuments : Finset DocumentKey) (session agent : String) :
    (projectDurable (.rejected documents) mergedDocuments session agent).phase = .failed := by
  simp [projectDurable, observe, observeCore]

theorem observeCore_mergedDocuments (prev : ClientProgress)
    (merged : Finset DocumentKey) (served : Option (Finset DocumentKey)) (failed : Bool) :
    (observeCore prev merged served failed).mergedDocuments = merged := by
  unfold observeCore
  split_ifs <;> rfl

theorem observe_mergedDocuments (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (failed : Bool)
    (session agent : String) :
    (observe prev mergedDocuments servedDocuments failed session agent).mergedDocuments =
      (progressFor prev session agent).mergedDocuments ∪ mergedDocuments := by
  unfold observe
  exact observeCore_mergedDocuments _ _ _ _

theorem observe_merged_monotone (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (failed : Bool)
    (session agent : String) (hsession : prev.session = session)
    (hagent : prev.agent = agent) :
    prev.mergedDocuments ⊆
      (observe prev mergedDocuments servedDocuments failed session agent).mergedDocuments := by
  rw [observe_mergedDocuments]
  simp [progressFor, hsession, hagent]

theorem observe_complete_iff (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (session agent : String)
    (hprev : (progressFor prev session agent).phase ≠ .failed) :
    (observe prev mergedDocuments servedDocuments false session agent).phase = .complete ↔
      canComplete ((progressFor prev session agent).mergedDocuments ∪ mergedDocuments)
        (mergeServed (progressFor prev session agent).servedDocuments servedDocuments) = true := by
  unfold observe observeCore
  have hnf : decide ((progressFor prev session agent).phase = .failed) = false :=
    decide_eq_false_iff_not.mpr hprev
  simp [hnf]
  split_ifs <;> simp_all

theorem observe_cannot_complete_without_server (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey) (session agent : String)
    (hprev : (progressFor prev session agent).phase ≠ .failed)
    (hserved : mergeServed (progressFor prev session agent).servedDocuments none = none) :
    (observe prev mergedDocuments none false session agent).phase ≠ .complete := by
  intro hcomplete
  have hiff :=
    (observe_complete_iff prev mergedDocuments none session agent hprev).mp hcomplete
  unfold canComplete at hiff
  simp [hserved] at hiff

/-- Equal cardinality cannot substitute for exact document identity. -/
theorem equal_count_substitution_fails_closed
    (merged served : Finset DocumentKey) (_ : merged.card = served.card)
    (hmissing : ¬ served ⊆ merged) :
    canComplete merged (some served) = false := by
  simp [canComplete, hmissing]

/-- Locally present transcript rows are not evidence that a hydration request
was started. Only `beginRequest` may move an idle receiver into an in-flight
phase when the server has not supplied a manifest. -/
theorem observe_idle_without_server_stays_idle (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey) (session agent : String)
    (hidle : (progressFor prev session agent).phase = .idle)
    (hserved : (progressFor prev session agent).servedDocuments = none) :
    (observe prev mergedDocuments none false session agent).phase = .idle := by
  unfold observe observeCore
  simp [hidle, hserved, mergeServed, canComplete]

/-- A failed receiver is terminal under passive observation. Restarting the
same target requires the explicit `beginRequest` transition. -/
theorem observe_failed_without_begin_stays_failed (prev : ClientProgress)
    (mergedDocuments : Finset DocumentKey)
    (servedDocuments : Option (Finset DocumentKey)) (session agent : String)
    (hfailed : (progressFor prev session agent).phase = .failed) :
    (observe prev mergedDocuments servedDocuments false session agent).phase = .failed := by
  unfold observe observeCore
  simp [hfailed]

/-- Focusing a different session/agent starts from an idle empty receiver state. -/
theorem progressFor_other_target_resets (prev : ClientProgress) (session agent : String)
    (hdifferent : prev.session ≠ session ∨ prev.agent ≠ agent) :
    progressFor prev session agent = { session, agent } := by
  unfold progressFor
  split
  · rename_i hsame
    exact False.elim (hdifferent.elim (fun h => h hsame.1) (fun h => h hsame.2))
  · rfl

/-- Retrying clears a prior terminal receiver state and its old manifest. -/
theorem beginRequest_resets_terminal (session agent : String) :
    (beginRequest session agent).phase = .requested ∧
    (beginRequest session agent).mergedDocuments = ∅ ∧
    (beginRequest session agent).servedDocuments = none := by
  simp [beginRequest]

end SessionHydration
