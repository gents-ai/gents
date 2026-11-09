import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.Watcher

def watcherSrc : SourceInstance :=
  { name := "Watcher"
  , dedupePolicy := .ttlCooldown
  , rescanBoundedBy := 1
  }

theorem watcher_pending_eventually_observed
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair watcherSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence watcherSrc w₀ d h_persisted h_unprocessed (by decide)

theorem watcher_cooldown_excludes_handle
    (w : World) (d : DocId) (a : Action) (w' : World)
    (h_processed : d ∈ w.processedSet)
    (h : Transition w a w') :
    a ≠ .handle d :=
  C1_processed_set_excludes_handle w d a w' h_processed h

/-- Delivering the selected head releases the other marks so a later rescan can
offer those requests again. The canonical claim owner still enforces queue order
and the pending-state CAS; releasing a local observation mark never admits a
request. -/
def releasedBy (session : DocId → String) (head d : DocId) : Prop :=
  d ≠ head ∧ session d = session head

/-- This exhibits one such trace; it is not a bound over every fair schedule. -/
theorem watcher_overtaken_request_redelivered_after_unblock
    (session : DocId → String) (w₀ : World) (head d : DocId)
    (h_released : releasedBy session head d)
    (h_pending : d ∈ w₀.persistentSet)
    (h_head : head ∈ w₀.persistentSet)
    (h_marked : d ∈ w₀.processedSet) :
    ∃ (w₂ w' : World),
      TraceOf w₀ [.release d, .depersist head] w₂ ∧
      TraceOf w₂ [.rescanTick, .handle d] w' ∧
      Fair watcherSrc [.rescanTick, .handle d] ∧
      d ∈ w'.handled := by
  let w₁ : World :=
    { w₀ with processedSet := w₀.processedSet.filter (fun x => x ≠ d) }
  let w₂ : World :=
    { w₁ with persistentSet := w₁.persistentSet.erase head }
  have h_unmarked : d ∉ w₂.processedSet := by
    show d ∉ w₀.processedSet.filter (fun x => x ≠ d)
    simp
  have h_still_pending : d ∈ w₂.persistentSet :=
    (List.mem_erase_of_ne h_released.1).mpr h_pending
  let w₃ : World :=
    { w₂ with subscriptionQueue :=
        (w₂.persistentSet.filter (fun x => x ∉ w₂.processedSet)) ++ w₂.subscriptionQueue }
  let w₄ : World :=
    { w₃ with handled := d :: w₃.handled
            , processedSet := d :: w₃.processedSet
            , subscriptionQueue := w₃.subscriptionQueue.erase d }
  have h_queued : d ∈ w₃.subscriptionQueue := by
    show d ∈ (w₂.persistentSet.filter (fun x => x ∉ w₂.processedSet)) ++ w₂.subscriptionQueue
    apply List.mem_append.mpr
    left
    apply List.mem_filter.mpr
    refine ⟨h_still_pending, ?_⟩
    simp [h_unmarked]
  refine ⟨w₂, w₄, ?_, ?_, ?_, ?_⟩
  · exact TraceOf.cons (Transition.release w₀ d h_marked)
      (TraceOf.cons (Transition.depersist w₁ head h_head) TraceOf.nil)
  · exact TraceOf.cons (Transition.rescanTick w₂)
      (TraceOf.cons (Transition.handle w₃ d h_queued h_unmarked) TraceOf.nil)
  · intro i h_window
    have h_len : ([Action.rescanTick, Action.handle d]).length = 2 := rfl
    have h_b : watcherSrc.rescanBoundedBy = 1 := rfl
    rw [h_len, h_b] at h_window
    have h_i : i = 0 := by omega
    subst h_i
    exact ⟨0, Nat.le_refl 0, by rw [h_b]; omega, rfl⟩
  · exact List.mem_cons_self _ _

theorem watcher_release_keeps_unrelated_marks
    (w : World) (d x : DocId) (w' : World)
    (h : Transition w (.release d) w')
    (h_other : x ≠ d)
    (h_marked : x ∈ w.processedSet) :
    x ∈ w'.processedSet := by
  cases h
  show x ∈ w.processedSet.filter (fun y => y ≠ d)
  simp [h_marked, h_other]

/-- Availability governs dispatch, while configured agents with verified local
signing identities remain scan actors so their requests can be rejected. -/
structure ActorCandidate where
  did : String
  configured : Bool
  identityResolved : Bool
  available : Bool
  deriving BEq, DecidableEq, Repr

def scanActors (candidates : List ActorCandidate) : List String :=
  (candidates.filter (fun c => c.configured && c.identityResolved)).map (·.did)

theorem unavailable_resolved_agent_is_scanned :
    scanActors [⟨"agent", true, true, false⟩] = ["agent"] := by decide

theorem unresolved_agent_is_not_scanned :
    scanActors [⟨"agent", true, false, false⟩] = [] := by decide

structure ScopedRow where
  docId : String
  selectedAgent : String
  deriving BEq, DecidableEq, Repr

inductive ScopedView where
  | denied | missingRoot | rows (rows : List ScopedRow)
  deriving Repr

structure ScopedScan where
  actor : String
  view : ScopedView
  deriving Repr

structure ScopedFailure where
  actor : String
  reason : String
  deriving BEq, DecidableEq, Repr

structure ScopedBatch where
  rows : List ScopedRow := []
  failures : List ScopedFailure := []
  deriving BEq, DecidableEq, Repr

/-- Each view is the result of a native ACP query authenticated as the named
agent. A missing root or denied query is not an empty authorized result. Rows
selected for a different agent do not become work for this actor even if it can
read them. The caller supplies configured agents with verified resolved
signing identities, including unavailable agents; an unresolved agent is not scanned. This does not fence a
configuration change racing a previously started native scan. -/
def collectScoped : List String → List ScopedScan → ScopedBatch
  | [], _ => {}
  | actor :: rest, scans =>
      let tail := collectScoped rest scans
      match scans.find? (fun scan => scan.actor == actor) with
      | none => { tail with failures := ⟨actor, "missing_scan"⟩ :: tail.failures }
      | some scan =>
        match scan.view with
        | .denied => { tail with failures := ⟨actor, "denied"⟩ :: tail.failures }
        | .missingRoot => { tail with failures := ⟨actor, "missing_root"⟩ :: tail.failures }
        | .rows rows => { tail with rows := rows.filter (fun row => row.selectedAgent == actor) ++ tail.rows }

def uniqueScopedRows (rows : List ScopedRow) : List ScopedRow :=
  rows.foldl (fun retained row =>
    if retained.any (fun prior => prior.docId == row.docId) then retained else retained ++ [row]) []

/-- `arrivalOrder` is the native global ordering of the successfully observed
physical documents, including comparable cursor positions obtained as each
agent. No order is asserted relative to unobservable requests. The
legacy order is the prior created-at/request-id order, used only for rows with
no native arrival. This is a model of the merge, not an authorization check or
an assertion that detached observations form one transaction. -/
def orderScopedRows (rows : List ScopedRow) (legacyOrder arrivalOrder : List String) : List ScopedRow :=
  let unique := uniqueScopedRows rows
  let legacyIds := (legacyOrder ++ unique.map (·.docId)).eraseDups
  let beforeJournal := legacyIds.filterMap (fun id =>
    if arrivalOrder.contains id then none else unique.find? (fun row => row.docId == id))
  let journaled := arrivalOrder.eraseDups.filterMap (fun id => unique.find? (fun row => row.docId == id))
  beforeJournal ++ journaled

def scopedCandidates (activeAgents : List String) (scans : List ScopedScan)
    (legacyOrder arrivalOrder : List String) : ScopedBatch :=
  let collected := collectScoped activeAgents.eraseDups scans
  { collected with rows := orderScopedRows collected.rows legacyOrder arrivalOrder }

/-- Failures remain available to the existing watcher wake and fallback scan;
their presence does not prevent the successful actors from making progress. -/
def scopedNeedsRescan (batch : ScopedBatch) : Bool := !batch.failures.isEmpty

theorem missing_scoped_root_is_not_empty :
    (scopedCandidates ["agent"] [⟨"agent", .missingRoot⟩] [] []).failures =
      [⟨"agent", "missing_root"⟩] := by decide

theorem denied_scoped_root_is_not_empty :
    (scopedCandidates ["agent"] [⟨"agent", .denied⟩] [] []).failures =
      [⟨"agent", "denied"⟩] := by decide

theorem inactive_agent_scan_is_not_used :
    (scopedCandidates [] [⟨"old", .rows [⟨"old-doc", "old"⟩]⟩] [] ["old-doc"]).rows = [] := by decide

theorem scoped_merge_preserves_native_global_arrival :
    (scopedCandidates ["a", "b"]
      [⟨"a", .rows [⟨"late", "a"⟩]⟩, ⟨"b", .rows [⟨"early", "b"⟩]⟩] [] ["early", "late"]).rows =
      [⟨"early", "b"⟩, ⟨"late", "a"⟩] := by decide

theorem denied_actor_does_not_block_healthy_actor :
    let batch := scopedCandidates ["a", "b"]
      [⟨"a", .denied⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩] [] ["healthy"]
    batch.rows = [⟨"healthy", "b"⟩] ∧ batch.failures = [⟨"a", "denied"⟩] ∧
      scopedNeedsRescan batch = true := by decide

end EventDelivery.Watcher
