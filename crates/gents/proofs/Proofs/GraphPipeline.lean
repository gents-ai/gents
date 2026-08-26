import Proofs.Basic

/-!
# Graph pipeline publication and run pinning

This model covers the control-plane boundary for compiling a model-proposed
document graph. A proposal is not executable configuration. It becomes visible
to the trigger runtime only after whole-graph validation and complete artifact
materialization, through one active-revision pointer.

The model deliberately abstracts over compiler algorithms, DefraDB transaction
implementation, and stage execution. Those are Rust conformance boundaries.
-/

namespace GraphPipeline

abbrev GraphId := Nat
abbrev RevisionId := Nat
abbrev RevisionDigest := Nat
abbrev RunId := Nat

inductive RevisionStatus where
  | draft
  | validated
  | active
  | retired
  deriving DecidableEq, Repr

inductive RunStatus where
  | running
  | succeeded
  | failed
  | cancelled
  deriving DecidableEq, Repr

def RunStatus.terminal : RunStatus → Bool
  | .succeeded | .failed | .cancelled => true
  | .running => false

structure Revision where
  graphId : GraphId
  revisionId : RevisionId
  digest : RevisionDigest
  status : RevisionStatus
  typesValid : Bool
  topologyValid : Bool
  capabilitiesAuthorized : Bool
  withinBounds : Bool
  artifactsComplete : Bool
  deriving DecidableEq, Repr

def Revision.wholeGraphValid (revision : Revision) : Prop :=
  revision.typesValid = true ∧
    revision.topologyValid = true ∧
    revision.capabilitiesAuthorized = true ∧
    revision.withinBounds = true

instance (revision : Revision) : Decidable revision.wholeGraphValid := by
  unfold Revision.wholeGraphValid
  infer_instance

def Revision.ready (revision : Revision) : Prop :=
  revision.wholeGraphValid ∧ revision.artifactsComplete = true

instance (revision : Revision) : Decidable revision.ready := by
  unfold Revision.ready
  infer_instance

structure Run where
  runId : RunId
  graphId : GraphId
  revisionId : RevisionId
  revisionDigest : RevisionDigest
  status : RunStatus
  seedCommitted : Bool
  cancellationRequested : Bool
  resultsCommitted : Bool
  deriving DecidableEq, Repr

def Run.mayMaterializeCorrelated (run : Run) : Prop :=
  run.status = .running ∧ run.cancellationRequested = false

instance (run : Run) : Decidable run.mayMaterializeCorrelated := by
  unfold Run.mayMaterializeCorrelated
  infer_instance

structure State where
  revision : Revision
  activeRevision : Option RevisionId
  run : Option Run
  deriving DecidableEq, Repr

def State.pointerAligned (state : State) : Prop :=
  match state.revision.status with
  | .active => state.activeRevision = some state.revision.revisionId
  | .draft | .validated | .retired => state.activeRevision = none

def State.activeReady (state : State) : Prop :=
  state.revision.status = .active → state.revision.ready

def State.runPinned (state : State) : Prop :=
  ∀ run, state.run = some run →
    run.graphId = state.revision.graphId ∧
      run.revisionId = state.revision.revisionId ∧
      run.revisionDigest = state.revision.digest

def State.safe (state : State) : Prop :=
  state.pointerAligned ∧ state.activeReady ∧ state.runPinned

def initial (revision : Revision) : State :=
  { revision := { revision with status := .draft, artifactsComplete := false }
  , activeRevision := none
  , run := none
  }

inductive Action where
  | validate
  | materialize
  | activate
  | retire
  | startRun (runId : RunId)
  | requestCancel
  | succeedRun (resultContractSatisfied activeWorkTerminal : Bool)
  | failRun (failureProven : Bool)
  | cancelRun (activeWorkTerminal : Bool)
  deriving DecidableEq, Repr

def updateRunStatus (state : State) (current next : RunStatus) : Option State :=
  match state.run with
  | some run =>
      if run.status = current then
        some { state with run := some { run with status := next } }
      else
        none
  | none => none

def step? (state : State) : Action → Option State
  | .validate =>
      if state.revision.status = .draft ∧ state.revision.wholeGraphValid then
        some { state with revision := { state.revision with status := .validated } }
      else
        none
  | .materialize =>
      if state.revision.status = .validated then
        some { state with
          revision := { state.revision with artifactsComplete := true }
        }
      else
        none
  | .activate =>
      if state.revision.status = .validated ∧ state.revision.ready ∧
          state.activeRevision = none then
        some { state with
          revision := { state.revision with status := .active }
          activeRevision := some state.revision.revisionId
        }
      else
        none
  | .retire =>
      if state.revision.status = .active ∧
          state.activeRevision = some state.revision.revisionId then
        some { state with
          revision := { state.revision with status := .retired }
          activeRevision := none
        }
      else
        none
  | .startRun runId =>
      if state.revision.status = .active ∧
          state.activeRevision = some state.revision.revisionId ∧
          state.revision.ready ∧
          state.run = none then
        some { state with
          run := some
            { runId := runId
            , graphId := state.revision.graphId
            , revisionId := state.revision.revisionId
            , revisionDigest := state.revision.digest
            , status := .running
            , seedCommitted := true
            , cancellationRequested := false
            , resultsCommitted := false
            }
        }
      else
        none
  | .requestCancel =>
      match state.run with
      | some run =>
          if run.status = .running then
            some { state with
              run := some { run with cancellationRequested := true }
            }
          else
            none
      | none => none
  | .succeedRun resultContractSatisfied activeWorkTerminal =>
      match state.run with
      | some run =>
          if run.status = .running ∧ resultContractSatisfied = true ∧
              activeWorkTerminal = true then
            some { state with
              run := some
                { run with status := .succeeded, resultsCommitted := true }
            }
          else
            none
      | none => none
  | .failRun failureProven =>
      if failureProven = true then
        updateRunStatus state .running .failed
      else
        none
  | .cancelRun activeWorkTerminal =>
      match state.run with
      | some run =>
          if run.status = .running ∧
              run.cancellationRequested = true ∧
              activeWorkTerminal = true then
            some { state with run := some { run with status := .cancelled } }
          else
            none
      | none => none

theorem updateRunStatus_preserves_revision
    {pre post : State} {current next : RunStatus}
    (h : updateRunStatus pre current next = some post) :
    post.revision = pre.revision := by
  unfold updateRunStatus at h
  split at h
  · split at h
    · cases h
      rfl
    · simp at h
  · simp at h

theorem updateRunStatus_preserves_safe
    {pre post : State} {current next : RunStatus}
    (h_safe : pre.safe)
    (h : updateRunStatus pre current next = some post) :
    post.safe := by
  unfold updateRunStatus at h
  split at h
  · split at h
    · cases h
      simp_all [State.safe, State.pointerAligned, State.activeReady,
        State.runPinned]
    · simp at h
  · simp at h

theorem initial_safe (revision : Revision) : (initial revision).safe := by
  simp [initial, State.safe, State.pointerAligned, State.activeReady,
    State.runPinned]

theorem step_preserves_revision_identity
    {pre post : State} {action : Action}
    (h : step? pre action = some post) :
    post.revision.graphId = pre.revision.graphId ∧
      post.revision.revisionId = pre.revision.revisionId ∧
      post.revision.digest = pre.revision.digest := by
  cases action with
  | validate =>
      simp only [step?] at h
      split at h
      · cases h
        simp
      · simp at h
  | materialize =>
      simp only [step?] at h
      split at h
      · cases h
        simp
      · simp at h
  | activate =>
      simp only [step?] at h
      split at h
      · cases h
        simp
      · simp at h
  | retire =>
      simp only [step?] at h
      split at h
      · cases h
        simp
      · simp at h
  | startRun runId =>
      simp only [step?] at h
      split at h
      · cases h
        simp
      · simp at h
  | failRun failureProven =>
      simp only [step?] at h
      split at h
      · have h_revision := updateRunStatus_preserves_revision h
        simp [h_revision]
      · simp at h
  | requestCancel | succeedRun _ _ | cancelRun _ =>
      simp only [step?] at h
      split at h
      · split at h
        · cases h
          simp
        · simp at h
      · simp at h

theorem start_requires_active_revision
    {pre post : State} {runId : RunId}
    (h : step? pre (.startRun runId) = some post) :
    pre.revision.status = .active ∧
      pre.activeRevision = some pre.revision.revisionId := by
  simp only [step?] at h
  split at h <;> simp_all

theorem inactive_revision_cannot_start
    (state : State) (runId : RunId)
    (h : state.revision.status ≠ .active) :
    step? state (.startRun runId) = none := by
  simp [step?, h]

theorem activation_requires_complete_validated_artifacts
    {pre post : State}
    (h : step? pre .activate = some post) :
    pre.revision.status = .validated ∧ pre.revision.ready := by
  simp only [step?] at h
  split at h <;> simp_all

theorem start_pins_revision
    {pre post : State} {runId : RunId}
    (h : step? pre (.startRun runId) = some post) :
    ∃ run, post.run = some run ∧
      run.graphId = pre.revision.graphId ∧
      run.revisionId = pre.revision.revisionId ∧
      run.revisionDigest = pre.revision.digest := by
  simp only [step?] at h
  split at h
  · cases h
    simp
  · simp at h

theorem start_commits_seed_and_enters_running
    {pre post : State} {runId : RunId}
    (h : step? pre (.startRun runId) = some post) :
    ∃ run, post.run = some run ∧
      run.status = .running ∧
      run.seedCommitted = true ∧
      run.cancellationRequested = false ∧
      run.resultsCommitted = false := by
  simp only [step?] at h
  split at h
  · cases h
    simp
  · simp at h

theorem success_requires_result_contract_terminal_work_and_commits_results
    {pre post : State} {resultContractSatisfied activeWorkTerminal : Bool}
    (h : step? pre (.succeedRun resultContractSatisfied activeWorkTerminal) = some post) :
    resultContractSatisfied = true ∧ activeWorkTerminal = true ∧
      ∃ run, post.run = some run ∧
        run.status = .succeeded ∧ run.resultsCommitted = true := by
  simp only [step?] at h
  split at h
  · split at h
    · cases h
      simp_all
    · simp at h
  · simp at h

theorem failure_requires_durable_evidence
    {pre post : State} {failureProven : Bool}
    (h : step? pre (.failRun failureProven) = some post) :
    failureProven = true := by
  simp only [step?] at h
  split at h <;> simp_all

theorem cancellation_intent_suppresses_correlated_materialization
    {pre post : State}
    (h : step? pre .requestCancel = some post) :
    ∃ run, post.run = some run ∧
      run.cancellationRequested = true ∧
      ¬ run.mayMaterializeCorrelated := by
  simp only [step?] at h
  split at h
  · split at h
    · cases h
      simp [Run.mayMaterializeCorrelated]
    · simp at h
  · simp at h

theorem cancel_requires_intent_and_terminal_work
    {pre post : State} {activeWorkTerminal : Bool}
    (h : step? pre (.cancelRun activeWorkTerminal) = some post) :
    activeWorkTerminal = true ∧
      ∃ run, pre.run = some run ∧
        run.status = .running ∧ run.cancellationRequested = true := by
  simp only [step?] at h
  split at h
  · split at h
    · cases h
      simp_all
    · simp at h
  · simp at h

theorem terminal_run_rejects_further_terminal_writes
    (state : State) (run : Run)
    (h_run : state.run = some run)
    (h_terminal : run.status.terminal = true) :
    step? state (.succeedRun true true) = none ∧
      step? state (.failRun true) = none ∧
      step? state (.cancelRun true) = none := by
  cases h_status : run.status <;>
    simp_all [RunStatus.terminal, step?, updateRunStatus]

theorem safe_preserved
    {pre post : State} {action : Action}
    (h_safe : pre.safe)
    (h_step : step? pre action = some post) :
    post.safe := by
  cases action with
  | validate =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned, Revision.ready, Revision.wholeGraphValid]
      · simp at h_step
  | materialize =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned, Revision.ready, Revision.wholeGraphValid]
      · simp at h_step
  | activate =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned, Revision.ready, Revision.wholeGraphValid]
      · simp at h_step
  | retire =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned]
      · simp at h_step
  | startRun runId =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned]
      · simp at h_step
  | failRun failureProven =>
      simp only [step?] at h_step
      split at h_step
      · exact updateRunStatus_preserves_safe h_safe h_step
      · simp at h_step
  | requestCancel | succeedRun _ _ | cancelRun _ =>
      simp only [step?] at h_step
      split at h_step
      · split at h_step <;>
          try { simp at h_step }
        cases h_step
        simp_all [State.safe, State.pointerAligned, State.activeReady,
          State.runPinned]
      · simp at h_step

theorem active_revision_is_ready
    {state : State}
    (h_safe : state.safe)
    (h_active : state.revision.status = .active) :
    state.revision.ready :=
  h_safe.2.1 h_active

theorem run_binding_survives_step
    {pre post : State} {action : Action} {run : Run}
    (h_safe : pre.safe)
    (h_step : step? pre action = some post)
    (h_run : post.run = some run) :
    run.graphId = post.revision.graphId ∧
      run.revisionId = post.revision.revisionId ∧
      run.revisionDigest = post.revision.digest :=
  (safe_preserved h_safe h_step).2.2 run h_run

end GraphPipeline
