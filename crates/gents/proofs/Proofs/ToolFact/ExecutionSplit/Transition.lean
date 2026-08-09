import Proofs.ToolFact.ExecutionSplit.State

namespace ToolFact.ExecutionSplit

open ToolFact

inductive CommitDisposition where
  | applied
  | observedIdentical
  | rejected
  deriving DecidableEq, Repr

namespace CommitDisposition

def toContract : CommitDisposition → String
  | .applied => "applied"
  | .observedIdentical => "observed_identical"
  | .rejected => "rejected"

end CommitDisposition

structure CommitObservation where
  disposition : CommitDisposition
  state : State

def commitInvocation
    (state : State)
    (visibleLogicalDocs : List Nat)
    (intent : ToolInvocationIntent)
    (evidence : SignedRef) : CommitObservation :=
  let target := evidence.version.docId
  if intent.argsHash ≠ 0 ∧ evidence.authoritative = true then
    if visibleLogicalDocs = [] ∧ state.invocations target = none then
      let fact := ToolInvocationFact.forIntent intent evidence
      ⟨.applied, { state with invocations := Store.bind state.invocations target fact }⟩
    else
      match state.invocations target with
      | some existing =>
          if visibleLogicalDocs = [target] ∧
              existing = ToolInvocationFact.forIntent intent evidence then
            ⟨.observedIdentical, state⟩
          else
            ⟨.rejected, state⟩
      | none => ⟨.rejected, state⟩
  else
    ⟨.rejected, state⟩

/-- Publish the first version of the single-writer execution document. -/
def startExecution
    (state : State)
    (visibleLogicalDocs : List Nat)
    (intent : ToolExecutionIntent)
    (evidence : SignedRef) : CommitObservation :=
  let targetDoc := evidence.version.docId
  let targetCid := evidence.version.compositeCommitCid
  match exactInvocation? state.invocations intent.invocation with
  | none => ⟨.rejected, state⟩
  | some _ =>
      if intent.ownerDid ≠ 0 ∧ intent.epoch ≠ 0 ∧
          evidence.signerDid = intent.ownerDid ∧ evidence.authoritative = true ∧
          (intent.phase = .pending ∨ intent.phase = .running ∨
            intent.phase = .awaitingApproval) then
        let fact := ToolExecutionFact.genesis intent evidence
        if visibleLogicalDocs = [] ∧ state.executions targetCid = none then
          ⟨.applied, { state with executions := Store.bind state.executions targetCid fact }⟩
        else
          match state.executions targetCid with
          | some existing =>
              if visibleLogicalDocs = [targetDoc] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩

/-- Full output is immutable and pins the exact running execution version. -/
def commitOutput
    (state : State)
    (_visibleLogicalDocs : List Nat)
    (intent : ToolOutputIntent)
    (evidence : SignedRef) : CommitObservation :=
  let target := evidence.version.docId
  match exactInvocation? state.invocations intent.invocation,
      exactExecution? state.executions intent.execution with
  | some _, some execution =>
      if execution.invocation = intent.invocation ∧ execution.phase = .running ∧
          intent.fullOutput = true ∧ intent.outputHash ≠ 0 ∧
          evidence.signerDid = execution.ownerDid ∧ evidence.authoritative = true then
        let fact := ToolOutputFact.forIntent intent evidence
        if state.outputs target = none then
          ⟨.applied, { state with outputs := Store.bind state.outputs target fact }⟩
        else
          match state.outputs target with
          | some existing =>
              if existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

/-- Approval is immutable and pins the exact held execution version. -/
def commitApproval
    (state : State)
    (visibleLogicalDocs : List Nat)
    (intent : ToolApprovalIntent)
    (evidence : SignedRef) : CommitObservation :=
  let target := evidence.version.docId
  match exactInvocation? state.invocations intent.invocation,
      exactExecution? state.executions intent.execution with
  | some _, some execution =>
      if execution.invocation = intent.invocation ∧
          execution.phase = .awaitingApproval ∧ evidence.authoritative = true then
        let fact := ToolApprovalFact.forIntent intent evidence
        if visibleLogicalDocs = [] ∧ state.approvals target = none then
          ⟨.applied, { state with approvals := Store.bind state.approvals target fact }⟩
        else
          match state.approvals target with
          | some existing =>
              if visibleLogicalDocs = [target] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

def omissionTransitionAllowed
    (acceptedPhase terminalPhase : ExecutionPhase) (reason : OmissionReason) : Prop :=
  match reason with
  | .preDispatchFailure => acceptedPhase = .pending ∧ terminalPhase = .failed
  | .approvalDenied => acceptedPhase = .awaitingApproval ∧ terminalPhase = .failed
  | .executionLost | .recoveryFailure | .childDead | .childSuperseded =>
      acceptedPhase = .running ∧ terminalPhase = .failed
  | .timedOut =>
      (acceptedPhase = .running ∨ acceptedPhase = .awaitingApproval) ∧
        terminalPhase = .timedOut
  | .cancelled =>
      (acceptedPhase = .pending ∨ acceptedPhase = .awaitingApproval ∨
        acceptedPhase = .running) ∧ terminalPhase = .cancelled

instance (acceptedPhase terminalPhase : ExecutionPhase) (reason : OmissionReason) :
    Decidable (omissionTransitionAllowed acceptedPhase terminalPhase reason) := by
  cases reason <;> unfold omissionTransitionAllowed <;> infer_instance

/-- Denial omissions additionally pin the exact immutable denial. Other
omissions must not smuggle in an unrelated approval reference. -/
def omissionApprovalValid
    (state : State) (execution : ToolExecutionFact)
    (intent : ToolOutputOmissionIntent) : Bool :=
  match intent.reason, intent.approval with
  | .approvalDenied, some approvalRef =>
      match exactApproval? state.approvals approvalRef with
      | some approval =>
          approval.decision == .denied && approval.execution == execution.signed &&
          approval.invocation == execution.invocation
      | none => false
  | .approvalDenied, none => false
  | _, none => true
  | _, some _ => false

/-- An omission is immutable typed evidence explaining why no full output can
exist. Its reason must agree with the exact accepted execution phase. -/
def commitOmission
    (state : State)
    (_visibleLogicalDocs : List Nat)
    (intent : ToolOutputOmissionIntent)
    (evidence : SignedRef) : CommitObservation :=
  let target := evidence.version.docId
  match exactInvocation? state.invocations intent.invocation,
      exactExecution? state.executions intent.execution with
  | some _, some execution =>
      if execution.invocation = intent.invocation ∧
          omissionTransitionAllowed execution.phase
            intent.reason.terminalPhase intent.reason ∧
          omissionApprovalValid state execution intent = true ∧
          evidence.signerDid = execution.ownerDid ∧ evidence.authoritative = true then
        let fact := ToolOutputOmissionFact.forIntent intent evidence
        if state.omissions target = none then
          ⟨.applied, { state with omissions := Store.bind state.omissions target fact }⟩
        else
          match state.omissions target with
          | some existing =>
              if existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

/-- Advance a held execution to running only through an exact approved fact. -/
def approveExecution
    (state : State)
    (visibleCurrentHeads : List Nat)
    (held : SignedRef)
    (approvalRef : SignedRef)
    (next : SignedRef) : CommitObservation :=
  match exactExecution? state.executions held, exactApproval? state.approvals approvalRef with
  | some execution, some approval =>
      if execution.phase = .awaitingApproval ∧ approval.decision = .approved ∧
          approval.execution = held ∧ approval.invocation = execution.invocation ∧
          next.version.docId = held.version.docId ∧ next.version.compositeCommitCid ≠ held.version.compositeCommitCid ∧
          next.signerDid = execution.ownerDid ∧ next.authoritative = true then
        let fact : ToolExecutionFact :=
          { signed := next
          , invocation := execution.invocation
          , ownerDid := execution.ownerDid
          , epoch := execution.epoch
          , phase := .running
          , previous := some held
          , approval := some approvalRef }
        let targetCid := next.version.compositeCommitCid
        if visibleCurrentHeads = [held.version.compositeCommitCid] ∧
            state.executions targetCid = none then
          ⟨.applied, { state with executions := Store.bind state.executions targetCid fact }⟩
        else
          match state.executions targetCid with
          | some existing =>
              if visibleCurrentHeads = [targetCid] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

/-- Terminal completion is accepted only after a full immutable output fact
pins the exact running execution version. -/
def completeExecution
    (state : State)
    (visibleCurrentHeads : List Nat)
    (running : SignedRef)
    (outputRef : SignedRef)
    (next : SignedRef) : CommitObservation :=
  match exactExecution? state.executions running, exactOutput? state.outputs outputRef with
  | some execution, some output =>
      if execution.phase = .running ∧ output.execution = running ∧
          output.invocation = execution.invocation ∧
          next.version.docId = running.version.docId ∧
          next.version.compositeCommitCid ≠ running.version.compositeCommitCid ∧
          next.signerDid = execution.ownerDid ∧ next.authoritative = true then
        let fact : ToolExecutionFact :=
          { signed := next
          , invocation := execution.invocation
          , ownerDid := execution.ownerDid
          , epoch := execution.epoch
          , phase := .completed
          , previous := some running
          , approval := execution.approval
          , output := some outputRef }
        let targetCid := next.version.compositeCommitCid
        if visibleCurrentHeads = [running.version.compositeCommitCid] ∧
            state.executions targetCid = none then
          ⟨.applied, { state with executions := Store.bind state.executions targetCid fact }⟩
        else
          match state.executions targetCid with
          | some existing =>
              if visibleCurrentHeads = [targetCid] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

/-- A running execution may fail with a complete output payload. -/
def failExecutionWithOutput
    (state : State)
    (visibleCurrentHeads : List Nat)
    (running : SignedRef)
    (outputRef : SignedRef)
    (next : SignedRef) : CommitObservation :=
  match exactExecution? state.executions running, exactOutput? state.outputs outputRef with
  | some execution, some output =>
      if execution.phase = .running ∧ output.execution = running ∧
          output.invocation = execution.invocation ∧
          next.version.docId = running.version.docId ∧
          next.version.compositeCommitCid ≠ running.version.compositeCommitCid ∧
          next.signerDid = execution.ownerDid ∧ next.authoritative = true then
        let fact : ToolExecutionFact :=
          { signed := next
          , invocation := execution.invocation
          , ownerDid := execution.ownerDid
          , epoch := execution.epoch
          , phase := .failed
          , previous := some running
          , approval := execution.approval
          , output := some outputRef }
        let targetCid := next.version.compositeCommitCid
        if visibleCurrentHeads = [running.version.compositeCommitCid] ∧
            state.executions targetCid = none then
          ⟨.applied, { state with executions := Store.bind state.executions targetCid fact }⟩
        else
          match state.executions targetCid with
          | some existing =>
              if visibleCurrentHeads = [targetCid] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

/-- Timeout, cancellation, and pre-dispatch failure terminate only through an
exact typed omission fact pinned to the accepted execution version. -/
def terminalizeWithOmission
    (state : State)
    (visibleCurrentHeads : List Nat)
    (accepted : SignedRef)
    (omissionRef : SignedRef)
    (terminalPhase : ExecutionPhase)
    (next : SignedRef) : CommitObservation :=
  match exactExecution? state.executions accepted, exactOmission? state.omissions omissionRef with
  | some execution, some omission =>
      if omission.execution = accepted ∧ omission.invocation = execution.invocation ∧
          omissionTransitionAllowed execution.phase terminalPhase omission.reason ∧
          next.version.docId = accepted.version.docId ∧
          next.version.compositeCommitCid ≠ accepted.version.compositeCommitCid ∧
          next.signerDid = execution.ownerDid ∧ next.authoritative = true then
        let fact : ToolExecutionFact :=
          { signed := next
          , invocation := execution.invocation
          , ownerDid := execution.ownerDid
          , epoch := execution.epoch
          , phase := terminalPhase
          , previous := some accepted
          , approval := if omission.reason = .approvalDenied then omission.approval
              else execution.approval
          , omission := some omissionRef }
        let targetCid := next.version.compositeCommitCid
        if visibleCurrentHeads = [accepted.version.compositeCommitCid] ∧
            state.executions targetCid = none then
          ⟨.applied, { state with executions := Store.bind state.executions targetCid fact }⟩
        else
          match state.executions targetCid with
          | some existing =>
              if visibleCurrentHeads = [targetCid] ∧ existing = fact then
                ⟨.observedIdentical, state⟩
              else
                ⟨.rejected, state⟩
          | none => ⟨.rejected, state⟩
      else
        ⟨.rejected, state⟩
  | _, _ => ⟨.rejected, state⟩

structure CompletedProjection where
  invocation : ToolInvocationFact
  running : ToolExecutionFact
  terminal : ToolExecutionFact
  output : ToolOutputFact
  approval : Option ToolApprovalFact
  deriving DecidableEq, Repr

def CompletedProjection.Valid (projection : CompletedProjection) : Prop :=
  projection.terminal.phase = .completed ∧
  projection.terminal.previous = some projection.running.signed ∧
  projection.terminal.output = some projection.output.signed ∧
  projection.terminal.omission = none ∧
  projection.running.phase = .running ∧
  projection.running.invocation = projection.terminal.invocation ∧
  projection.invocation.signed = projection.terminal.invocation ∧
  projection.running.ownerDid = projection.terminal.ownerDid ∧
  projection.running.epoch = projection.terminal.epoch ∧
  projection.output.execution = projection.running.signed ∧
  projection.output.invocation = projection.terminal.invocation ∧
  match projection.approval with
  | none => projection.terminal.approval = none
  | some approval =>
      projection.terminal.approval = some approval.signed ∧
      projection.running.approval = some approval.signed ∧
      projection.running.previous = some approval.execution ∧
      approval.invocation = projection.terminal.invocation

instance (projection : CompletedProjection) : Decidable projection.Valid := by
  unfold CompletedProjection.Valid
  cases h : projection.approval <;> simp only [h] <;> infer_instance

/-- Resolve candidate nodes exclusively through exact refs. Coherence is
checked separately so its proof obligations remain explicit and reusable. -/
def completedCandidate? (state : State) (terminalRef : SignedRef) : Option CompletedProjection :=
  match exactExecution? state.executions terminalRef with
  | some terminal =>
      match terminal.phase, terminal.previous, terminal.output with
      | .completed, some runningRef, some outputRef =>
          match exactInvocation? state.invocations terminal.invocation,
              exactExecution? state.executions runningRef,
              exactOutput? state.outputs outputRef with
          | some invocation, some running, some output =>
              match terminal.approval with
              | none => some ⟨invocation, running, terminal, output, none⟩
              | some approvalRef =>
                  match exactApproval? state.approvals approvalRef with
                  | some approval => some ⟨invocation, running, terminal, output, some approval⟩
                  | none => none
          | _, _, _ => none
      | _, _, _ => none
  | none => none

/-- Admit a completed graph only when every historical edge is exact and the
owner/epoch are preserved across the running-to-terminal transition. -/
def projectCompleted (state : State) (terminalRef : SignedRef) : Option CompletedProjection :=
  match completedCandidate? state terminalRef with
  | some projection => if projection.Valid then some projection else none
  | none => none

structure TerminalEvidenceProjection where
  invocation : ToolInvocationFact
  accepted : ToolExecutionFact
  terminal : ToolExecutionFact
  output : Option ToolOutputFact
  omission : Option ToolOutputOmissionFact
  deriving DecidableEq, Repr

def TerminalEvidenceProjection.Valid (projection : TerminalEvidenceProjection) : Prop :=
  projection.terminal.previous = some projection.accepted.signed ∧
  projection.accepted.invocation = projection.terminal.invocation ∧
  projection.invocation.signed = projection.terminal.invocation ∧
  projection.accepted.ownerDid = projection.terminal.ownerDid ∧
  projection.accepted.epoch = projection.terminal.epoch ∧
  match projection.output, projection.omission with
  | some output, none =>
      projection.terminal.output = some output.signed ∧
      projection.terminal.omission = none ∧
      projection.accepted.phase = .running ∧
      output.execution = projection.accepted.signed ∧
      output.invocation = projection.terminal.invocation ∧
      (projection.terminal.phase = .completed ∨ projection.terminal.phase = .failed)
  | none, some omission =>
      projection.terminal.output = none ∧
      projection.terminal.omission = some omission.signed ∧
      omission.execution = projection.accepted.signed ∧
      omission.invocation = projection.terminal.invocation ∧
      omissionTransitionAllowed projection.accepted.phase projection.terminal.phase omission.reason ∧
      match omission.reason with
      | .approvalDenied =>
          omission.approval.isSome ∧ projection.terminal.approval = omission.approval
      | _ =>
          omission.approval = none ∧
          projection.terminal.approval = projection.accepted.approval
  | _, _ => False

instance (projection : TerminalEvidenceProjection) : Decidable projection.Valid := by
  unfold TerminalEvidenceProjection.Valid
  cases h_output : projection.output with
  | none =>
      cases h_omission : projection.omission with
      | none => simp only [h_output, h_omission]; infer_instance
      | some omission =>
          cases h_reason : omission.reason <;>
            simp only [h_output, h_omission, h_reason] <;> infer_instance
  | some output =>
      cases h_omission : projection.omission <;>
        simp only [h_output, h_omission] <;> infer_instance

def terminalEvidenceCandidate?
    (state : State) (terminalRef : SignedRef) : Option TerminalEvidenceProjection :=
  match exactExecution? state.executions terminalRef with
  | some terminal =>
      match terminal.previous with
      | some acceptedRef =>
          match exactInvocation? state.invocations terminal.invocation,
              exactExecution? state.executions acceptedRef with
          | some invocation, some accepted =>
              match terminal.output, terminal.omission with
              | some outputRef, none =>
                  match exactOutput? state.outputs outputRef with
                  | some output => some ⟨invocation, accepted, terminal, some output, none⟩
                  | none => none
              | none, some omissionRef =>
                  match exactOmission? state.omissions omissionRef with
                  | some omission => some ⟨invocation, accepted, terminal, none, some omission⟩
                  | none => none
              | _, _ => none
          | _, _ => none
      | none => none
  | none => none

def projectTerminalEvidence
    (state : State) (terminalRef : SignedRef) : Option TerminalEvidenceProjection :=
  match terminalEvidenceCandidate? state terminalRef with
  | some projection => if projection.Valid then some projection else none
  | none => none

/-- A denial is terminal omission evidence plus the exact immutable denied
approval that justified it. -/
structure DeniedTerminalProjection where
  evidence : TerminalEvidenceProjection
  omission : ToolOutputOmissionFact
  approval : ToolApprovalFact
  deriving DecidableEq, Repr

def DeniedTerminalProjection.Valid (projection : DeniedTerminalProjection) : Prop :=
  projection.evidence.Valid ∧
  projection.evidence.output = none ∧
  projection.evidence.omission = some projection.omission ∧
  projection.omission.reason = .approvalDenied ∧
  projection.omission.approval = some projection.approval.signed ∧
  projection.evidence.terminal.approval = some projection.approval.signed ∧
  projection.approval.decision = .denied ∧
  projection.approval.execution = projection.evidence.accepted.signed ∧
  projection.approval.invocation = projection.evidence.terminal.invocation

instance (projection : DeniedTerminalProjection) : Decidable projection.Valid := by
  unfold DeniedTerminalProjection.Valid
  infer_instance

def deniedTerminalCandidate?
    (state : State) (terminalRef : SignedRef) : Option DeniedTerminalProjection :=
  match projectTerminalEvidence state terminalRef with
  | some evidence =>
      match evidence.omission with
      | some omission =>
          match omission.approval with
          | some approvalRef =>
              match exactApproval? state.approvals approvalRef with
              | some approval => some ⟨evidence, omission, approval⟩
              | none => none
          | none => none
      | none => none
  | none => none

def projectDeniedTerminal
    (state : State) (terminalRef : SignedRef) : Option DeniedTerminalProjection :=
  match deniedTerminalCandidate? state terminalRef with
  | some projection => if projection.Valid then some projection else none
  | none => none

end ToolFact.ExecutionSplit
