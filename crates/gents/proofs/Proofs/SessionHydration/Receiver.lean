import Proofs.SessionHydration.State

/-!
# Receiver-side hydration progress

The server writes `served_doc_count` after delivery is accepted. The client
must not treat that write as completion: it counts unique locally merged
transcript documents and may only complete when that count covers the
server's denominator. Empty served sessions (`servedCount = 0`) complete
immediately.
-/

namespace SessionHydration

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
  mergedCount : Nat := 0
  servedCount : Option Nat := none
  deriving DecidableEq

def canComplete (mergedCount : Nat) (servedCount : Option Nat) : Bool :=
  match servedCount with
  | some n => decide (mergedCount ≥ n)
  | none => false

def mergeServed (prev next : Option Nat) : Option Nat :=
  match next with
  | some n => some n
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

def observeCore (prev : ClientProgress) (merged : Nat) (served : Option Nat)
    (failed : Bool) : ClientProgress :=
  if failed || decide (prev.phase = .failed) then
    { phase := .failed, mergedCount := merged, servedCount := served }
  else if canComplete merged served then
    { phase := .complete, mergedCount := merged, servedCount := served }
  else if served.isSome || decide (prev.phase = .serving) ||
      (decide (prev.phase = .requested) && decide (merged > 0)) then
    { phase := .serving, mergedCount := merged, servedCount := served }
  else if decide (prev.phase = .requested) then
    { phase := .requested, mergedCount := merged, servedCount := served }
  else
    { phase := .idle, mergedCount := merged, servedCount := served }

def observe (prev : ClientProgress) (mergedCount : Nat) (servedCount : Option Nat)
    (failed : Bool) (session agent : String) : ClientProgress :=
  let base := progressFor prev session agent
  { observeCore base
      (max base.mergedCount mergedCount)
      (mergeServed base.servedCount servedCount)
      failed with session, agent }

/-- Durable control-row state for one exact session/agent target. -/
inductive DurableRequest where
  | missing
  | pending
  | served (count : Nat)
  | rejected (count : Option Nat)
  deriving DecidableEq, Repr

/-- Projecting a snapshot is a pure query over one durable control row plus
the locally merged count. It retains no process-wide receiver state. -/
def projectDurable (request : DurableRequest) (mergedCount : Nat)
    (session agent : String) : ClientProgress :=
  match request with
  | .missing => observe { session, agent } mergedCount none false session agent
  | .pending => observe (beginRequest session agent) mergedCount none false session agent
  | .served count =>
      observe (beginRequest session agent) mergedCount (some count) false session agent
  | .rejected count =>
      observe (beginRequest session agent) mergedCount count true session agent

theorem projectDurable_exact_target (request : DurableRequest) (mergedCount : Nat)
    (session agent : String) :
    (projectDurable request mergedCount session agent).session = session ∧
      (projectDurable request mergedCount session agent).agent = agent := by
  cases request <;> simp [projectDurable, observe]

theorem projectDurable_rejected_failed (count : Option Nat) (mergedCount : Nat)
    (session agent : String) :
    (projectDurable (.rejected count) mergedCount session agent).phase = .failed := by
  simp [projectDurable, observe, observeCore]

theorem observeCore_mergedCount (prev : ClientProgress) (merged : Nat)
    (served : Option Nat) (failed : Bool) :
    (observeCore prev merged served failed).mergedCount = merged := by
  unfold observeCore
  split_ifs <;> rfl

theorem observe_mergedCount (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat) (failed : Bool) (session agent : String) :
    (observe prev mergedCount servedCount failed session agent).mergedCount =
      max (progressFor prev session agent).mergedCount mergedCount := by
  unfold observe
  exact observeCore_mergedCount _ _ _ _

theorem observe_merged_monotone (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat) (failed : Bool) (session agent : String)
    (hsession : prev.session = session) (hagent : prev.agent = agent) :
    prev.mergedCount ≤
      (observe prev mergedCount servedCount failed session agent).mergedCount := by
  rw [observe_mergedCount]
  simp [progressFor, hsession, hagent]

theorem observe_complete_iff (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat) (session agent : String)
    (hprev : (progressFor prev session agent).phase ≠ .failed) :
    (observe prev mergedCount servedCount false session agent).phase = .complete ↔
      canComplete (max (progressFor prev session agent).mergedCount mergedCount)
        (mergeServed (progressFor prev session agent).servedCount servedCount) = true := by
  unfold observe observeCore
  have hnf : decide ((progressFor prev session agent).phase = .failed) = false :=
    decide_eq_false_iff_not.mpr hprev
  simp [hnf]
  split_ifs <;> simp_all

theorem observe_cannot_complete_without_server (prev : ClientProgress)
    (mergedCount : Nat) (session agent : String)
    (hprev : (progressFor prev session agent).phase ≠ .failed)
    (hserved : mergeServed (progressFor prev session agent).servedCount none = none) :
    (observe prev mergedCount none false session agent).phase ≠ .complete := by
  intro hcomplete
  have hiff :=
    (observe_complete_iff prev mergedCount none session agent hprev).mp hcomplete
  unfold canComplete at hiff
  simp [hserved] at hiff

/-- Locally present transcript rows are not evidence that a hydration request
was started. Only `beginRequest` may move an idle receiver into an in-flight
phase when the server has not supplied a denominator. -/
theorem observe_idle_without_server_stays_idle (prev : ClientProgress)
    (mergedCount : Nat) (session agent : String)
    (hidle : (progressFor prev session agent).phase = .idle)
    (hserved : (progressFor prev session agent).servedCount = none) :
    (observe prev mergedCount none false session agent).phase = .idle := by
  unfold observe observeCore
  simp [hidle, hserved, mergeServed, canComplete]

/-- A failed receiver is terminal under passive observation. Restarting the
same target requires the explicit `beginRequest` transition. -/
theorem observe_failed_without_begin_stays_failed (prev : ClientProgress)
    (mergedCount : Nat) (servedCount : Option Nat) (session agent : String)
    (hfailed : (progressFor prev session agent).phase = .failed) :
    (observe prev mergedCount servedCount false session agent).phase = .failed := by
  unfold observe observeCore
  simp [hfailed]

/-- Focusing a different session/agent starts from an idle, zero-count receiver state. -/
theorem progressFor_other_target_resets (prev : ClientProgress) (session agent : String)
    (hdifferent : prev.session ≠ session ∨ prev.agent ≠ agent) :
    progressFor prev session agent = { session, agent } := by
  unfold progressFor
  split
  · rename_i hsame
    exact False.elim (hdifferent.elim (fun h => h hsame.1) (fun h => h hsame.2))
  · rfl

/-- Retrying clears a prior terminal receiver state and its old denominator. -/
theorem beginRequest_resets_terminal (session agent : String) :
    (beginRequest session agent).phase = .requested ∧
    (beginRequest session agent).mergedCount = 0 ∧
    (beginRequest session agent).servedCount = none := by
  simp [beginRequest]

end SessionHydration
