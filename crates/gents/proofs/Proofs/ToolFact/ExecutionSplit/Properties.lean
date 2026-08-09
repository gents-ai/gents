import Proofs.ToolFact.ExecutionSplit.Transition

namespace ToolFact.ExecutionSplit

open ToolFact

theorem invocation_twin_rejected
    (state : State) (intent : ToolInvocationIntent) (evidence : SignedRef)
    (h_args : intent.argsHash ≠ 0)
    (h_evidence : evidence.authoritative = true)
    (left right : Nat) :
    (commitInvocation state [left, right] intent evidence).disposition = .rejected := by
  simp [commitInvocation, h_args, h_evidence]
  split <;> rfl

theorem invocation_replay_idempotent
    (state : State) (intent : ToolInvocationIntent) (evidence : SignedRef)
    (h_args : intent.argsHash ≠ 0)
    (h_evidence : evidence.authoritative = true)
    (h_existing : state.invocations evidence.version.docId =
      some (ToolInvocationFact.forIntent intent evidence)) :
    commitInvocation state [evidence.version.docId] intent evidence =
      ⟨.observedIdentical, state⟩ := by
  simp [commitInvocation, h_args, h_evidence, h_existing]

theorem competing_output_proposal_is_admitted
    (state : State) (intent : ToolOutputIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_running : execution.phase = .running)
    (h_full : intent.fullOutput = true)
    (h_output : intent.outputHash ≠ 0)
    (h_contract : intent.truncationContractHash ≠ 0)
    (h_projection : intent.modelProjectionHash ≠ 0)
    (h_derived : intent.modelProjectionHash = canonicalModelProjectionHash
      intent.outputHash intent.truncationContractHash)
    (h_owner : evidence.signerDid = execution.ownerDid)
    (h_evidence : evidence.authoritative = true)
    (h_absent : state.outputs evidence.version.docId = none)
    (left right : Nat) :
    (commitOutput state [left, right] intent evidence).disposition = .applied := by
  have h_canonical := h_projection
  rw [h_derived] at h_canonical
  simp [commitOutput, h_invocation, h_execution, h_parent, h_running, h_full,
    h_output, h_contract, h_projection, h_derived, h_canonical, h_owner, h_evidence, h_absent]

theorem output_replay_idempotent
    (state : State) (intent : ToolOutputIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_running : execution.phase = .running)
    (h_full : intent.fullOutput = true)
    (h_output : intent.outputHash ≠ 0)
    (h_contract : intent.truncationContractHash ≠ 0)
    (h_projection : intent.modelProjectionHash ≠ 0)
    (h_derived : intent.modelProjectionHash = canonicalModelProjectionHash
      intent.outputHash intent.truncationContractHash)
    (h_owner : evidence.signerDid = execution.ownerDid)
    (h_evidence : evidence.authoritative = true)
    (h_existing : state.outputs evidence.version.docId =
      some (ToolOutputFact.forIntent intent evidence)) :
    commitOutput state [evidence.version.docId] intent evidence =
      ⟨.observedIdentical, state⟩ := by
  have h_canonical := h_projection
  rw [h_derived] at h_canonical
  simp [commitOutput, h_invocation, h_execution, h_parent, h_running, h_full,
    h_output, h_contract, h_projection, h_derived, h_canonical, h_owner, h_evidence, h_existing]

theorem exact_model_projection_authoritative
    {output : ToolOutputFact} {observedHash : PayloadHash}
    (h_exact : exactModelProjection? output observedHash = some output) :
    observedHash = output.modelProjectionHash := by
  let derived := canonicalModelProjectionHash output.outputHash output.truncationContractHash
  change (if output.modelProjectionHash = derived ∧ observedHash = derived then
    some output else none) = some output at h_exact
  split at h_exact
  · rename_i h_valid
    exact h_valid.2.trans h_valid.1.symm
  · contradiction

theorem model_projection_is_derived_from_exact_output
    {output : ToolOutputFact} {observedHash : PayloadHash}
    (h_exact : exactModelProjection? output observedHash = some output) :
    observedHash = canonicalModelProjectionHash
      output.outputHash output.truncationContractHash := by
  let derived := canonicalModelProjectionHash output.outputHash output.truncationContractHash
  change (if output.modelProjectionHash = derived ∧ observedHash = derived then
    some output else none) = some output at h_exact
  split at h_exact
  · rename_i h_valid
    exact h_valid.2
  · contradiction

theorem forged_model_projection_rejected
    {output : ToolOutputFact} {observedHash : PayloadHash}
    (h_forged : observedHash ≠ output.modelProjectionHash) :
    exactModelProjection? output observedHash = none := by
  let derived := canonicalModelProjectionHash output.outputHash output.truncationContractHash
  change (if output.modelProjectionHash = derived ∧ observedHash = derived then
    some output else none) = none
  by_cases h_model : output.modelProjectionHash = derived
  · have h_observed : observedHash ≠ derived := by
      intro h_equal
      exact h_forged (h_equal.trans h_model.symm)
    simp [h_model, h_observed]
  · simp [h_model]

theorem approval_twin_rejected
    (state : State) (intent : ToolApprovalIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_held : execution.phase = .awaitingApproval)
    (h_evidence : evidence.authoritative = true)
    (left right : Nat) :
    (commitApproval state [left, right] intent evidence).disposition = .rejected := by
  simp [commitApproval, h_invocation, h_execution, h_parent, h_held, h_evidence]
  split <;> rfl

theorem approval_replay_idempotent
    (state : State) (intent : ToolApprovalIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_held : execution.phase = .awaitingApproval)
    (h_evidence : evidence.authoritative = true)
    (h_existing : state.approvals evidence.version.docId =
      some (ToolApprovalFact.forIntent intent evidence)) :
    commitApproval state [evidence.version.docId] intent evidence =
      ⟨.observedIdentical, state⟩ := by
  simp [commitApproval, h_invocation, h_execution, h_parent, h_held,
    h_evidence, h_existing]

theorem competing_omission_proposal_is_admitted
    (state : State) (intent : ToolOutputOmissionIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_allowed : omissionTransitionAllowed execution.phase
      intent.reason.terminalPhase intent.reason)
    (h_owner : evidence.signerDid = execution.ownerDid)
    (h_evidence : evidence.authoritative = true)
    (h_absent : state.omissions evidence.version.docId = none)
    (left right : Nat) :
    (commitOmission state [left, right] intent evidence).disposition = .applied := by
  simp [commitOmission, h_invocation, h_execution, h_parent, h_allowed,
    h_owner, h_evidence, h_absent]

theorem omission_replay_idempotent
    (state : State) (intent : ToolOutputOmissionIntent) (evidence : SignedRef)
    (invocation : ToolInvocationFact) (execution : ToolExecutionFact)
    (h_invocation : exactInvocation? state.invocations intent.invocation = some invocation)
    (h_execution : exactExecution? state.executions intent.execution = some execution)
    (h_parent : execution.invocation = intent.invocation)
    (h_allowed : omissionTransitionAllowed execution.phase
      intent.reason.terminalPhase intent.reason)
    (h_owner : evidence.signerDid = execution.ownerDid)
    (h_evidence : evidence.authoritative = true)
    (h_existing : state.omissions evidence.version.docId =
      some (ToolOutputOmissionFact.forIntent intent evidence)) :
    commitOmission state [evidence.version.docId] intent evidence =
      ⟨.observedIdentical, state⟩ := by
  simp [commitOmission, h_invocation, h_execution, h_parent, h_allowed,
    h_owner, h_evidence, h_existing]

theorem projectTerminalEvidence_valid
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    (h_project : projectTerminalEvidence state terminalRef = some projection) :
    projection.Valid := by
  unfold projectTerminalEvidence at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      exact h_valid
    · contradiction
  · contradiction

theorem terminal_evidence_closed
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    (h_project : projectTerminalEvidence state terminalRef = some projection) :
    (projection.output.isSome ∧ projection.omission.isNone) ∨
      (projection.output.isNone ∧ projection.omission.isSome) := by
  have h_valid := projectTerminalEvidence_valid h_project
  unfold TerminalEvidenceProjection.Valid at h_valid
  cases h_output : projection.output <;> cases h_omission : projection.omission <;>
    simp_all

theorem terminal_evidence_mutually_exclusive
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    (h_project : projectTerminalEvidence state terminalRef = some projection) :
    ¬ (projection.output.isSome ∧ projection.omission.isSome) := by
  have h_closed := terminal_evidence_closed h_project
  rcases h_closed with h_output | h_omission <;> simp_all

theorem terminal_evidence_owner_preserved
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    (h_project : projectTerminalEvidence state terminalRef = some projection) :
    projection.terminal.ownerDid = projection.accepted.ownerDid ∧
      projection.terminal.epoch = projection.accepted.epoch := by
  have h_valid := projectTerminalEvidence_valid h_project
  exact ⟨h_valid.2.2.2.1.symm, h_valid.2.2.2.2.1.symm⟩

theorem terminal_output_pins_accepted
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    {output : ToolOutputFact}
    (h_project : projectTerminalEvidence state terminalRef = some projection)
    (h_output : projection.output = some output) :
    projection.omission = none ∧ output.execution = projection.accepted.signed := by
  have h_valid := projectTerminalEvidence_valid h_project
  unfold TerminalEvidenceProjection.Valid at h_valid
  rw [h_output] at h_valid
  cases h_omission : projection.omission <;> simp_all

theorem terminal_omission_pins_accepted_and_reason
    {state : State} {terminalRef : SignedRef} {projection : TerminalEvidenceProjection}
    {omission : ToolOutputOmissionFact}
    (h_project : projectTerminalEvidence state terminalRef = some projection)
    (h_omission : projection.omission = some omission) :
    projection.output = none ∧
      omission.execution = projection.accepted.signed ∧
      omissionTransitionAllowed projection.accepted.phase
        projection.terminal.phase omission.reason := by
  have h_valid := projectTerminalEvidence_valid h_project
  unfold TerminalEvidenceProjection.Valid at h_valid
  rw [h_omission] at h_valid
  cases h_output : projection.output <;> simp_all

theorem denied_terminal_pins_exact_approval_and_omission
    {state : State} {terminalRef : SignedRef} {projection : DeniedTerminalProjection}
    (h_project : projectDeniedTerminal state terminalRef = some projection) :
    projection.evidence.output = none ∧
      projection.evidence.omission = some projection.omission ∧
      projection.evidence.approval = some projection.approval ∧
      projection.evidence.terminal.approval = some projection.approval.signed ∧
      projection.approval.decision = .denied ∧
      projection.approval.execution = projection.evidence.accepted.signed := by
  unfold projectDeniedTerminal at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      exact ⟨h_valid.2.1, h_valid.2.2.1, h_valid.2.2.2.1,
        h_valid.2.2.2.2.2.1, h_valid.2.2.2.2.2.2.1,
        h_valid.2.2.2.2.2.2.2.1⟩
    · contradiction
  · contradiction

theorem completed_projection_output_pins_running
    {state : State} {terminalRef : SignedRef} {projection : CompletedProjection}
    (h_project : projectCompleted state terminalRef = some projection) :
    projection.output.execution = projection.running.signed := by
  unfold projectCompleted at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      rcases h_valid with ⟨_, _, _, _, _, _, _, _, _, h_output, _, _⟩
      exact h_output
    · contradiction
  · contradiction

theorem completed_projection_terminal_closure
    {state : State} {terminalRef : SignedRef} {projection : CompletedProjection}
    (h_project : projectCompleted state terminalRef = some projection) :
    projection.terminal.phase = .completed ∧
      projection.terminal.previous = some projection.running.signed ∧
      projection.terminal.output = some projection.output.signed := by
  unfold projectCompleted at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      rcases h_valid with ⟨h_phase, h_previous, h_output, _, _, _, _, _, _, _, _, _⟩
      exact ⟨h_phase, h_previous, h_output⟩
    · contradiction
  · contradiction

theorem completed_projection_owner_preserved
    {state : State} {terminalRef : SignedRef} {projection : CompletedProjection}
    (h_project : projectCompleted state terminalRef = some projection) :
    projection.terminal.ownerDid = projection.running.ownerDid ∧
      projection.terminal.epoch = projection.running.epoch := by
  unfold projectCompleted at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      rcases h_valid with ⟨_, _, _, _, _, _, _, h_owner, h_epoch, _, _, _⟩
      exact ⟨h_owner.symm, h_epoch.symm⟩
    · contradiction
  · contradiction

theorem completed_projection_approval_pins_held
    {state : State} {terminalRef : SignedRef} {projection : CompletedProjection}
    {approval : ToolApprovalFact}
    (h_project : projectCompleted state terminalRef = some projection)
    (h_approval : projection.approval = some approval) :
    ∃ held, projection.running.previous = some held ∧ approval.execution = held := by
  unfold projectCompleted at h_project
  split at h_project
  · rename_i candidate h_candidate
    split at h_project
    · rename_i h_valid
      simp only [Option.some.injEq] at h_project
      subst projection
      unfold CompletedProjection.Valid at h_valid
      rw [h_approval] at h_valid
      rcases h_valid with ⟨_, _, _, _, _, _, _, _, _, _, _, _, _, h_previous, _⟩
      exact ⟨approval.execution, h_previous, rfl⟩
    · contradiction
  · contradiction

end ToolFact.ExecutionSplit
