import Proofs.CrossMachineComposed.ToolTermination

namespace ComposedState
namespace ReachabilityWitness

/-!
## Non-vacuity witnesses for reachable composed tool states

These small witnesses pin the abstract composed dynamics that are external to
`RequestContext.Transition` itself:

* `slot_acquire` supplies the fleet/scheduler admission grant needed before
  `begin_inference`.
* `clock_advance` moves the request clock and tool clocks in lockstep, making
  `deadlineExceeded` reachable without breaking `Coherent`.
-/

def startupCtx : ProcessState.StartupContext :=
  { hasStuckRequests := false, activeRequestCount := 0 }

def ready : ComposedState :=
  { initial with process := .ready }

def claimed : ComposedState :=
  { ready with
    request :=
      { ready.request with
        state := .claimed
        admission := .waiting
        claimTime := ready.request.currentTime
        deadline := ready.request.claimDeadline } }

def acquired : ComposedState :=
  { claimed with request := { claimed.request with admission := .acquired } }

def processing : ComposedState :=
  { acquired with
    request := { acquired.request with state := .processing, admission := .executing } }

def pendingTool : ToolExecution.ToolCallContext :=
  { callId := 1
  , requestId := processing.requestId
  , state := .pending
  , operation := .mcpCall
  , deadline := processing.request.deadline
  , currentTime := processing.request.currentTime
  , persistence := .uncommitted
  }

def withPendingTool : ComposedState :=
  { processing with tools := [pendingTool] }

def detachedTool : ToolExecution.ToolCallContext :=
  { pendingTool with
    callId := 2
    cancelPolicy := .detach
    childRequestId := some 42 }

def withDetachedTool : ComposedState :=
  { processing with tools := [detachedTool] }

def runningTool : ToolExecution.ToolCallContext :=
  { pendingTool with state := .running, startedAt := some pendingTool.currentTime }

def withRunningTool : ComposedState :=
  { withPendingTool with tools := [runningTool] }

def expiredPendingTime : Time :=
  withPendingTool.request.deadline + 1

def expiredPendingTool : ToolExecution.ToolCallContext :=
  { pendingTool with currentTime := expiredPendingTime }

def withExpiredPendingTool : ComposedState :=
  { withPendingTool with
    request := { withPendingTool.request with currentTime := expiredPendingTime }
    tools := [expiredPendingTool] }

def expiredRunningTime : Time :=
  withRunningTool.request.deadline + 1

def expiredRunningTool : ToolExecution.ToolCallContext :=
  { runningTool with currentTime := expiredRunningTime }

def withExpiredRunningTool : ComposedState :=
  { withRunningTool with
    request := { withRunningTool.request with currentTime := expiredRunningTime }
    tools := [expiredRunningTool] }

theorem step_ready : Transition initial ready := by
  exact Transition.process_step
    (ProcessState.Transition.startup_clean startupCtx rfl) rfl rfl rfl rfl

theorem step_claimed : Transition ready claimed := by
  refine Transition.request_step ?_ rfl rfl rfl rfl ?_ ?_
  · exact RequestContext.Transition.claim rfl rfl trivial rfl
  · intro _; trivial
  · intro h_advance
    cases h_advance with
    | inl h_progress =>
      simp [claimed, ready] at h_progress
    | inr h_begin =>
      obtain ⟨_, h_processing⟩ := h_begin
      simp [claimed, ready] at h_processing

theorem step_acquired : Transition claimed acquired := by
  exact Transition.slot_acquire rfl rfl rfl rfl rfl rfl rfl

theorem step_processing : Transition acquired processing := by
  refine Transition.request_step ?_ rfl rfl rfl rfl ?_ ?_
  · exact RequestContext.Transition.begin_inference rfl rfl rfl
  · intro h_pending
    simp [acquired, claimed, ready] at h_pending
  · intro h_advance
    intro h_fg
    simp [acquired, claimed, ready, initial] at h_fg

theorem step_withPendingTool : Transition processing withPendingTool := by
  refine Transition.tool_spawn
    (newTool := pendingTool)
    rfl rfl rfl rfl rfl rfl rfl ?_ ?_ ?_ ?_
  · simp [Coherent, pendingTool, withPendingTool]
  · intro h_det
    simp [IsDetached, pendingTool] at h_det
  · intro t h_in
    simp [processing, acquired, claimed, ready, initial] at h_in
  · intro _h_fg h_existing
    simp [processing, acquired, claimed, ready, initial] at h_existing

theorem step_withDetachedTool : Transition processing withDetachedTool := by
  refine Transition.tool_spawn
    (newTool := detachedTool)
    rfl rfl rfl rfl rfl rfl rfl ?_ ?_ ?_ ?_
  · simp [Coherent, detachedTool, pendingTool, withDetachedTool]
  · intro _h_det
    simp [Persistent, detachedTool, pendingTool, withDetachedTool, processing]
  · intro t h_in
    simp [processing, acquired, claimed, ready, initial] at h_in
  · intro _h_fg h_existing
    simp [processing, acquired, claimed, ready, initial] at h_existing

theorem step_withRunningTool : Transition withPendingTool withRunningTool := by
  refine Transition.tool_step
    (idx := 0)
    (toolPre := pendingTool)
    (toolPost := runningTool)
    ?_ ?_ rfl rfl rfl rfl rfl ?_ ?_ ?_ ?_
  · simp [withPendingTool]
  · exact ToolExecution.ToolCallContext.Transition.dispatch rfl rfl
  · simp [Coherent, pendingTool, withPendingTool]
  · simp [Coherent, pendingTool, runningTool, withRunningTool, withPendingTool,
      processing, acquired, claimed, ready, initial]
  · intro h_det
    simp [IsDetached, runningTool, pendingTool] at h_det
  · intro h_bg _h_fg
    simp [pendingTool] at h_bg

theorem step_withExpiredPendingTool : Transition withPendingTool withExpiredPendingTool := by
  refine Transition.clock_advance expiredPendingTime ?_ rfl rfl rfl ?_ rfl
  · simp [expiredPendingTime, withPendingTool, processing, acquired, claimed, ready, initial]
  · simp [withExpiredPendingTool, withPendingTool, expiredPendingTool, expiredPendingTime,
      processing, acquired, claimed, ready, initial]

theorem step_withExpiredRunningTool : Transition withRunningTool withExpiredRunningTool := by
  refine Transition.clock_advance expiredRunningTime ?_ rfl rfl rfl ?_ rfl
  · simp [expiredRunningTime, withRunningTool, withPendingTool, processing, acquired,
      claimed, ready, initial]
  · simp [withExpiredRunningTool, withRunningTool, withPendingTool, expiredRunningTool,
      expiredRunningTime, runningTool, pendingTool, processing, acquired, claimed, ready,
      initial]

theorem trace_ready : Trace initial ready :=
  Trace.step step_ready Trace.refl

theorem trace_claimed : Trace initial claimed :=
  Trace.step step_ready (Trace.step step_claimed Trace.refl)

theorem trace_acquired : Trace initial acquired :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired Trace.refl))

theorem trace_processing : Trace initial processing :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing Trace.refl)))

theorem trace_withPendingTool : Trace initial withPendingTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool Trace.refl))))

theorem trace_withDetachedTool : Trace initial withDetachedTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withDetachedTool Trace.refl))))

theorem trace_withRunningTool : Trace initial withRunningTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool
        (Trace.step step_withRunningTool Trace.refl)))))

theorem trace_withExpiredPendingTool : Trace initial withExpiredPendingTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool
        (Trace.step step_withExpiredPendingTool Trace.refl)))))

theorem trace_withExpiredRunningTool : Trace initial withExpiredRunningTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool
        (Trace.step step_withRunningTool
          (Trace.step step_withExpiredRunningTool Trace.refl))))))

/-- C1' has a concrete reachable domain: a pending linked tool can coexist with
    an exceeded parent deadline. -/
theorem c1_prime_reachable_domain_nonempty :
    ∃ pre toolPre,
      Trace initial pre ∧
      toolPre ∈ pre.tools ∧
      toolPre.state = .pending ∧
      ¬ IsDetached toolPre ∧
      pre.request.deadlineExceeded := by
  refine ⟨withExpiredPendingTool, expiredPendingTool, ?_, ?_, ?_, ?_, ?_⟩
  · exact trace_withExpiredPendingTool
  · simp [withExpiredPendingTool]
  · rfl
  · simp [IsDetached, expiredPendingTool, pendingTool]
  · simp [RequestContext.deadlineExceeded, withExpiredPendingTool, expiredPendingTime,
      withPendingTool, processing, acquired, claimed, ready, initial]

/-- C1 has a concrete reachable domain: a running linked tool can coexist with
    an exceeded parent deadline. -/
theorem c1_reachable_domain_nonempty :
    ∃ pre toolPre,
      Trace initial pre ∧
      toolPre ∈ pre.tools ∧
      toolPre.state = .running ∧
      ¬ IsDetached toolPre ∧
      pre.request.deadlineExceeded := by
  refine ⟨withExpiredRunningTool, expiredRunningTool, ?_, ?_, ?_, ?_, ?_⟩
  · exact trace_withExpiredRunningTool
  · simp [withExpiredRunningTool]
  · rfl
  · simp [IsDetached, expiredRunningTool, runningTool, pendingTool]
  · simp [RequestContext.deadlineExceeded, withExpiredRunningTool, expiredRunningTime,
      withRunningTool, withPendingTool, processing, acquired, claimed, ready, initial]

/-- B4 has a concrete reachable domain: a detached bridged tool can be spawned
    from a processing request, and the resulting reachable state is globally
    well-formed while the detached tool satisfies `Persistent`. -/
theorem detached_reachable_domain_nonempty :
    ∃ pre toolPre,
      Trace initial pre ∧
      pre.WellFormed ∧
      toolPre ∈ pre.tools ∧
      IsDetached toolPre ∧
      Persistent pre toolPre := by
  refine ⟨withDetachedTool, detachedTool, ?_, ?_, ?_, ?_, ?_⟩
  · exact trace_withDetachedTool
  · exact wellFormed_from_initial trace_withDetachedTool
  · simp [withDetachedTool]
  · simp [IsDetached, detachedTool, pendingTool]
  · simp [Persistent, detachedTool, pendingTool, withDetachedTool, processing]

/-- C2 reachable domain: latch a direct interrupt on a processing request that
    still carries a live pending tool, then drive `interrupt_processing`. -/
def interruptLatched : ComposedState :=
  { withPendingTool with
    request :=
      { withPendingTool.request with
        interruptRequestedAt := some withPendingTool.request.currentTime } }

def interruptedWithTool : ComposedState :=
  { interruptLatched with
    request :=
      { interruptLatched.request with state := .interrupted, admission := .released } }

theorem step_interruptLatched : Transition withPendingTool interruptLatched := by
  exact Transition.request_interrupt withPendingTool.request.currentTime rfl rfl rfl rfl rfl

theorem step_interrupted : Transition interruptLatched interruptedWithTool := by
  refine Transition.request_step ?_ rfl rfl rfl rfl ?_ ?_
  · exact RequestContext.Transition.interrupt_processing rfl rfl (by simp [interruptLatched]) rfl
  · intro h_pending
    simp [interruptLatched, withPendingTool, processing, acquired, claimed, ready] at h_pending
  · intro h_advance
    rcases h_advance with h_progress | ⟨h_claimed, _⟩
    · simp [interruptedWithTool, interruptLatched, withPendingTool, processing, acquired,
        claimed, ready] at h_progress
    · simp [interruptLatched, withPendingTool, processing, acquired, claimed, ready] at h_claimed

theorem trace_interruptedWithTool : Trace initial interruptedWithTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool
        (Trace.step step_interruptLatched
          (Trace.step step_interrupted Trace.refl))))))

/-- C2 has a concrete reachable domain: an interrupted parent request can still
    carry a live (cancellable) pending tool. -/
theorem c2_reachable_domain_nonempty :
    ∃ pre toolPre,
      Trace initial pre ∧
      toolPre ∈ pre.tools ∧
      pre.request.state = .interrupted ∧
      ¬ IsDetached toolPre ∧
      toolPre.cancellable := by
  refine ⟨interruptedWithTool, pendingTool, ?_, ?_, ?_, ?_, ?_⟩
  · exact trace_interruptedWithTool
  · simp [interruptedWithTool, interruptLatched, withPendingTool]
  · rfl
  · simp [IsDetached, pendingTool]
  · exact Or.inl rfl

/-- C2 running arm: latch a direct interrupt on a processing request that still
    carries a live *running* tool, then drive `interrupt_processing`. Mirrors the
    pending chain but reaches `.interrupted` while the tool is running. -/
def interruptLatchedRunning : ComposedState :=
  { withRunningTool with
    request :=
      { withRunningTool.request with
        interruptRequestedAt := some withRunningTool.request.currentTime } }

def interruptedWithRunningTool : ComposedState :=
  { interruptLatchedRunning with
    request :=
      { interruptLatchedRunning.request with state := .interrupted, admission := .released } }

theorem step_interruptLatchedRunning : Transition withRunningTool interruptLatchedRunning := by
  exact Transition.request_interrupt withRunningTool.request.currentTime rfl rfl rfl rfl rfl

theorem step_interruptedRunning :
    Transition interruptLatchedRunning interruptedWithRunningTool := by
  refine Transition.request_step ?_ rfl rfl rfl rfl ?_ ?_
  · exact RequestContext.Transition.interrupt_processing rfl rfl
      (by simp [interruptLatchedRunning]) rfl
  · intro h_pending
    simp [interruptLatchedRunning, withRunningTool, withPendingTool, processing, acquired,
      claimed, ready] at h_pending
  · intro h_advance
    rcases h_advance with h_progress | ⟨h_claimed, _⟩
    · simp [interruptedWithRunningTool, interruptLatchedRunning, withRunningTool,
        withPendingTool, processing, acquired, claimed, ready] at h_progress
    · simp [interruptLatchedRunning, withRunningTool, withPendingTool, processing, acquired,
        claimed, ready] at h_claimed

theorem trace_interruptedWithRunningTool : Trace initial interruptedWithRunningTool :=
  Trace.step step_ready (Trace.step step_claimed
    (Trace.step step_acquired (Trace.step step_processing
      (Trace.step step_withPendingTool
        (Trace.step step_withRunningTool
          (Trace.step step_interruptLatchedRunning
            (Trace.step step_interruptedRunning Trace.refl)))))))

/-- C2 running arm has a concrete reachable domain: an interrupted parent request
    can still carry a live *running* tool. -/
theorem c2_running_reachable_domain_nonempty :
    ∃ pre toolPre,
      Trace initial pre ∧
      toolPre ∈ pre.tools ∧
      pre.request.state = .interrupted ∧
      ¬ IsDetached toolPre ∧
      toolPre.cancellable := by
  refine ⟨interruptedWithRunningTool, runningTool, ?_, ?_, ?_, ?_, ?_⟩
  · exact trace_interruptedWithRunningTool
  · simp [interruptedWithRunningTool, interruptLatchedRunning, withRunningTool]
  · rfl
  · simp [IsDetached, runningTool, pendingTool]
  · exact Or.inr (Or.inr rfl)

end ReachabilityWitness
end ComposedState
