import Proofs.Session.SessionQueue.FoldCases
import Proofs.Session.SessionQueue.ManagementCases
import Proofs.Session.SessionQueue.InputEditCases
import Proofs.Runtime.CanonicalOutput.Execution.FoldPublicationCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.FoldQueueContracts

open SessionQueue SessionQueue.FoldCases Conformance.Contracts

def entryJson (entry : QueueEntry) : String :=
  "{\"request_id\":" ++ toString entry.requestId ++
  ",\"execution_origin\":" ++ jsonString entry.origin.toDefraDB ++
  ",\"delivery\":" ++ jsonString entry.delivery.toDefraDB ++
  ",\"order_key\":" ++ toString entry.orderKey ++
  ",\"source\":" ++ jsonString entry.source.toDefraDB ++
  ",\"policy\":" ++ jsonString entry.policy.toDefraDB ++
  ",\"queued_after\":" ++ jsonOptionalNat entry.queuedAfter ++
  ",\"requester_id\":" ++ jsonOptionalNat entry.requester ++
  ",\"turn_context\":" ++ toString entry.turnContext ++
  ",\"fresh\":" ++ jsonOptionalBool (some entry.fresh) ++ "}"

def eventJson : Event → String
  | .enqueue entry => "{\"kind\":\"enqueue\",\"entry\":" ++ entryJson entry ++ "}"
  | .claim admitted =>
      "{\"kind\":\"claim\",\"admitted\":" ++ jsonArray (admitted.map toString) ++ "}"
  | .consume => "{\"kind\":\"consume\"}"
  | .finish => "{\"kind\":\"finish\"}"

inductive AuthoredKeyKind where
  | context
  | prompt
  | folded
  deriving Lean.ToJson

structure AuthoredKeyWire where
  kind : AuthoredKeyKind
  requestId : Option Nat
  deriving Lean.ToJson

def AuthoredKeyWire.ofModel : AuthoredKey → AuthoredKeyWire
  | .context => { kind := .context, requestId := none }
  | .prompt => { kind := .prompt, requestId := none }
  | .folded id => { kind := .folded, requestId := some id }

structure FoldedWire where
  requestId : Nat
  content : String
  deriving Lean.ToJson

structure AuthoredWire where
  key : AuthoredKeyWire
  content : String
  deriving Lean.ToJson

structure TurnInputCase where
  name : String
  context : Option String
  head : String
  folded : List FoldedWire
  deriving Lean.ToJson

structure TurnInputObservation where
  authored : List AuthoredWire
  providerInput : List String
  deriving Lean.ToJson

@[conformance_family]
def turnInput : Family TurnInputCase TurnInputObservation where
  inputs := turnInputCases.map fun (name, input) =>
    { name, context := input.context, head := input.head
      folded := input.folded.map fun (requestId, content) => { requestId, content } }
  model := fun i =>
    let input : TurnInput String :=
      ⟨i.context, i.head, i.folded.map fun folded => (folded.requestId, folded.content)⟩
    { authored := input.authored.map fun (key, content) =>
        { key := AuthoredKeyWire.ofModel key, content }
      providerInput := input.providerInput }

structure RetrySelectionInput where
  name : String
  parentPublished : Bool
  selected : List Nat
  deriving Lean.ToJson

structure RetrySelectionOutput where
  resume : Bool
  answered : List Nat
  deriving Lean.ToJson

@[conformance_family]
def retrySelection : Family RetrySelectionInput RetrySelectionOutput where
  inputs := retrySelectionCases.map fun c =>
    { name := c.name, parentPublished := c.parentPublished, selected := c.selected }
  model := fun i =>
    let value : RetrySelectionCase := ⟨i.name, i.parentPublished, i.selected⟩
    { resume := value.resume, answered := value.answered }

section Families

open Lean

deriving instance ToJson for QueueDelivery, QueueSource, QueuePolicy, ExecutionOrigin

structure GoalAssignmentWire where
  goal : String
  opening : String
  deriving ToJson

/-- Every modeled queue entry is issued at its request ID; the round-trip
examples below check that for each case list. -/
structure EntryWire where
  requestId : Nat
  orderKey : Nat
  executionOrigin : ExecutionOrigin
  delivery : QueueDelivery
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option Nat
  queuedAfter : Option Nat
  requesterId : Option Nat
  turnContext : Nat
  executionDirectory : Option Nat
  goalAssignment : Option GoalAssignmentWire
  fresh : Bool
  deriving ToJson

def EntryWire.ofModel (entry : QueueEntry) : EntryWire :=
  { requestId := entry.requestId, orderKey := entry.orderKey
    executionOrigin := entry.origin, delivery := entry.delivery, source := entry.source
    policy := entry.policy, queueKey := entry.queueKey, queuedAfter := entry.queuedAfter
    requesterId := entry.requester, turnContext := entry.turnContext
    executionDirectory := entry.executionDirectory
    goalAssignment := entry.goalAssignment.map fun (goal, opening) => { goal, opening }
    fresh := entry.fresh }

def EntryWire.toModel (entry : EntryWire) : QueueEntry :=
  { requestId := entry.requestId, createdAt := entry.requestId, orderKey := entry.orderKey
    origin := entry.executionOrigin, delivery := entry.delivery, source := entry.source
    policy := entry.policy, queueKey := entry.queueKey, queuedAfter := entry.queuedAfter
    requester := entry.requesterId, turnContext := entry.turnContext
    executionDirectory := entry.executionDirectory
    goalAssignment := entry.goalAssignment.map fun g => (g.goal, g.opening)
    fresh := entry.fresh }

structure IssuedEntryWire where
  requestId : Nat
  createdAt : Nat
  orderKey : Nat
  executionOrigin : ExecutionOrigin
  delivery : QueueDelivery
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option Nat
  queuedAfter : Option Nat
  requesterId : Option Nat
  turnContext : Nat
  executionDirectory : Option Nat
  goalAssignment : Option GoalAssignmentWire
  fresh : Bool
  deriving ToJson

def IssuedEntryWire.ofModel (entry : QueueEntry) : IssuedEntryWire :=
  let wire := EntryWire.ofModel entry
  { requestId := wire.requestId, createdAt := entry.createdAt, orderKey := wire.orderKey
    executionOrigin := wire.executionOrigin, delivery := wire.delivery, source := wire.source
    policy := wire.policy, queueKey := wire.queueKey, queuedAfter := wire.queuedAfter
    requesterId := wire.requesterId, turnContext := wire.turnContext
    executionDirectory := wire.executionDirectory, goalAssignment := wire.goalAssignment
    fresh := wire.fresh }

def IssuedEntryWire.toModel (entry : IssuedEntryWire) : QueueEntry :=
  { (EntryWire.toModel
      { requestId := entry.requestId, orderKey := entry.orderKey
        executionOrigin := entry.executionOrigin, delivery := entry.delivery
        source := entry.source, policy := entry.policy, queueKey := entry.queueKey
        queuedAfter := entry.queuedAfter, requesterId := entry.requesterId
        turnContext := entry.turnContext, executionDirectory := entry.executionDirectory
        goalAssignment := entry.goalAssignment, fresh := entry.fresh })
    with createdAt := entry.createdAt }

inductive EventKind where
  | enqueue | claim | consume | finish
  deriving ToJson

structure EventWire where
  kind : EventKind
  entry : Option EntryWire
  admitted : List Nat
  deriving ToJson

def EventWire.ofModel : Event → EventWire
  | .enqueue entry => { kind := .enqueue, entry := some (EntryWire.ofModel entry), admitted := [] }
  | .claim admitted => { kind := .claim, entry := none, admitted }
  | .consume => { kind := .consume, entry := none, admitted := [] }
  | .finish => { kind := .finish, entry := none, admitted := [] }

def EventWire.toModel (event : EventWire) : Option Event :=
  match event.kind with
  | .enqueue => event.entry.map fun entry => .enqueue entry.toModel
  | .claim => some (.claim event.admitted)
  | .consume => some .consume
  | .finish => some .finish

structure FoldInput where
  name : String
  nodeId : Nat
  sessionId : Nat
  inputs : List EventWire
  deriving ToJson

structure ClaimWire where
  head : Nat
  folded : List Nat
  deriving ToJson

structure FoldObservation where
  active : Option Nat
  pending : List Nat
  folding : List Nat
  terminal : List Nat
  claims : List ClaimWire
  deriving ToJson

@[conformance_family]
def foldScript : Family FoldInput (Option FoldObservation) where
  inputs := cases.map fun (name, events) =>
    { name, nodeId := queue.scope.node, sessionId := queue.scope.session
      inputs := events.map EventWire.ofModel }
  model := fun i => do
    let events ← i.inputs.mapM EventWire.toModel
    let observed ← observation events
    pure { active := observed.active, pending := observed.pending, folding := observed.folding
           terminal := observed.terminal
           claims := observed.claims.map fun (head, folded) => { head, folded } }

structure HandoverInput where
  name : String
  pending : List IssuedEntryWire
  admitted : List Nat
  deriving ToJson

structure HandoverQueue where
  active : Option Nat
  folding : List Nat
  pending : List Nat
  deriving ToJson

open CanonicalOutput.Execution.Handover.Cases in
@[conformance_family]
def handoverClaim : Family HandoverInput (Option HandoverQueue) where
  inputs := foldClaimCases.map fun value =>
    { name := value.name, pending := value.pending.map IssuedEntryWire.ofModel, admitted := value.admitted }
  model := fun i => do
    let claimed ← foldClaim
      { name := i.name, pending := i.pending.map IssuedEntryWire.toModel, admitted := i.admitted }
    pure { active := claimed.active, folding := claimed.folding.map (·.requestId)
           pending := claimed.pending.map (·.requestId) }

section Publication

open CanonicalOutput.Execution.FoldPublication

inductive StepKind where
  | observeDeadline | cancelFirstPending | enqueueSteering | intake | finishOrIntake
  | publishPrompt | publishChangedPrompt | publishFolded | acceptTurn | recover | terminalize
  | finish
  deriving ToJson

structure StepWire where
  kind : StepKind
  now : Option Nat
  deadline : Option Nat
  requestId : Option Nat
  generation : Option Nat
  safeBoundary : Option Bool
  expected : Option Nat
  fresh : Option Nat
  deriving ToJson

def StepWire.empty (kind : StepKind) : StepWire :=
  { kind, now := none, deadline := none, requestId := none, generation := none
    safeBoundary := none, expected := none, fresh := none }

def StepWire.ofModel : Step → StepWire
  | .observeDeadline now deadline =>
      { StepWire.empty .observeDeadline with now := some now, deadline := some deadline }
  | .cancelFirstPending => StepWire.empty .cancelFirstPending
  | .enqueueSteering requestId =>
      { StepWire.empty .enqueueSteering with requestId := some requestId }
  | .intake generation safe =>
      { StepWire.empty .intake with generation := some generation, safeBoundary := some safe }
  | .finishOrIntake generation safe =>
      { StepWire.empty .finishOrIntake with
        generation := some generation, safeBoundary := some safe }
  | .publishPrompt generation =>
      { StepWire.empty .publishPrompt with generation := some generation }
  | .publishChangedPrompt generation =>
      { StepWire.empty .publishChangedPrompt with generation := some generation }
  | .publishFolded generation requestId =>
      { StepWire.empty .publishFolded with
        generation := some generation, requestId := some requestId }
  | .acceptTurn generation => { StepWire.empty .acceptTurn with generation := some generation }
  | .recover expected fresh =>
      { StepWire.empty .recover with expected := some expected, fresh := some fresh }
  | .terminalize generation => { StepWire.empty .terminalize with generation := some generation }
  | .finish => StepWire.empty .finish

def StepWire.toModel (step : StepWire) : Option Step :=
  match step.kind with
  | .observeDeadline => do pure (.observeDeadline (← step.now) (← step.deadline))
  | .cancelFirstPending => some .cancelFirstPending
  | .enqueueSteering => step.requestId.map .enqueueSteering
  | .intake => do pure (.intake (← step.generation) (← step.safeBoundary))
  | .finishOrIntake => do pure (.finishOrIntake (← step.generation) (← step.safeBoundary))
  | .publishPrompt => step.generation.map .publishPrompt
  | .publishChangedPrompt => step.generation.map .publishChangedPrompt
  | .publishFolded => do pure (.publishFolded (← step.generation) (← step.requestId))
  | .acceptTurn => step.generation.map .acceptTurn
  | .recover => do pure (.recover (← step.expected) (← step.fresh))
  | .terminalize => step.generation.map .terminalize
  | .finish => some .finish

structure PublicationInput where
  name : String
  head : Nat
  selected : Nat
  steps : List StepWire
  deriving ToJson

structure PublicationObservation where
  accepted : Bool
  active : Option Nat
  pending : List Nat
  folding : List Nat
  terminal : List Nat
  authoredKeys : List String
  deriving ToJson

def PublicationInput.ofCase (value : String × List Step) : PublicationInput :=
  { name := value.1, head := activeEntry.requestId, selected := selectedEntry.requestId
    steps := value.2.map StepWire.ofModel }

def publicationRun (i : PublicationInput) : Option (List PublicationObservation) := do
  let steps ← i.steps.mapM StepWire.toModel
  let observations ← run steps
  pure (observations.map fun o =>
    { accepted := o.accepted, active := o.active, pending := o.pending, folding := o.folding
      terminal := o.terminal, authoredKeys := o.authoredKeys })

def unsafeBoundary (steps : List Step) : Bool :=
  steps.any fun step => match step with
    | .intake _ false | .finishOrIntake _ false => true
    | _ => false

/-- The served writer fixture intakes at the owned loop's safe boundary; an
unsafe provider-stream boundary is the loop's, which has no durable queue. -/
def servedPublicationCases : List (String × List Step) :=
  CanonicalOutput.Execution.FoldPublication.cases ++
    steeringCases.filter fun (_, steps) => !unsafeBoundary steps

def unsafePublicationCases : List (String × List Step) :=
  steeringCases.filter fun (_, steps) => unsafeBoundary steps

@[conformance_family]
def publicationScript : Family PublicationInput (Option (List PublicationObservation)) where
  inputs := servedPublicationCases.map PublicationInput.ofCase
  model := publicationRun

structure UnsafeBoundaryObservation where
  accepted : Bool
  deriving ToJson

@[conformance_family]
def publicationUnsafeBoundary : Family PublicationInput UnsafeBoundaryObservation where
  inputs := unsafePublicationCases.map PublicationInput.ofCase
  model := fun i =>
    { accepted := ((publicationRun i).bind List.getLast?).map (·.accepted) |>.getD true }

/-- model-production-gap: the served writer fixture has no unsafe provider-stream
boundary and the owned loop has no durable queue, so the per-step queue and
authored-key observations of the unsafe boundary scripts are not replayed;
publication_unsafe_boundary replays their refused intake through the loop. -/
@[conformance_family]
def publicationUnsafeBoundaryUnreached :
    Family PublicationInput (Option (List PublicationObservation)) where
  inputs := unsafePublicationCases.map PublicationInput.ofCase
  model := publicationRun

example : unsafePublicationCases.map (·.1) =
    ["streaming_boundary_does_not_take_input", "natural_completion_during_stream_refuses"] := by
  native_decide

example : unsafePublicationCases.all fun (_, steps) =>
    match (run steps).bind List.getLast? with
    | some last => !last.accepted && last.active == some activeEntry.requestId &&
        last.pending.length == 1
    | none => false := by
  native_decide

example : (steeringCases.find? (·.1 == "natural_completion_intakes_late_input_before_terminal")).all
    fun (_, steps) =>
      match run steps with
      | none => false
      | some observations =>
        let finishes := (steps.zip observations).filterMap fun (step, observed) =>
          match step with
          | .finishOrIntake _ _ => some observed
          | _ => none
        finishes.length == 2 &&
          (finishes[0]?.map (·.active)) == some (some activeEntry.requestId) &&
          (finishes[1]?.map (·.active)) == some none &&
          (finishes[1]?.map (·.terminal.contains activeEntry.requestId)) == some true &&
          (observations.getLast?.map (·.active)) == some none := by
  native_decide

end Publication

section Management

open SessionQueue.ManagementCases (Operation)

structure StateWire where
  active : Option Nat
  pending : List EntryWire
  folding : List EntryWire
  terminal : List Nat
  deriving ToJson

def StateWire.ofModel (state : SessionQueueState) : StateWire :=
  { active := state.active, pending := state.pending.map EntryWire.ofModel
    folding := state.folding.map EntryWire.ofModel, terminal := state.terminal.sort (· ≤ ·) }

def StateWire.toModel (state : StateWire) : SessionQueueState :=
  { scope := queue.scope, active := state.active, pending := state.pending.map EntryWire.toModel
    terminal := state.terminal.toFinset, folding := state.folding.map EntryWire.toModel }

inductive OperationKind where
  | intake | replace
  deriving ToJson

structure OperationWire where
  kind : OperationKind
  active : Option EntryWire
  admitted : List Nat
  safeBoundary : Option Bool
  caller : Option Nat
  expected : List Nat
  offset : Option Nat
  count : Option Nat
  replacements : List EntryWire
  deriving ToJson

def OperationWire.ofModel : Operation → OperationWire
  | .intake active admitted safe =>
      { kind := .intake, active := some (EntryWire.ofModel active), admitted
        safeBoundary := some safe, caller := none, expected := [], offset := none
        count := none, replacements := [] }
  | .replace caller expected offset count replacements =>
      { kind := .replace, active := none, admitted := [], safeBoundary := none, caller
        expected, offset := some offset, count := some count
        replacements := replacements.map EntryWire.ofModel }

def OperationWire.toModel (operation : OperationWire) : Option Operation :=
  match operation.kind with
  | .intake => do
      pure (.intake (← operation.active).toModel operation.admitted (← operation.safeBoundary))
  | .replace => do
      pure (.replace operation.caller operation.expected (← operation.offset) (← operation.count)
        (operation.replacements.map EntryWire.toModel))

structure ManagementInput where
  name : String
  before : StateWire
  operation : OperationWire
  deriving ToJson

structure QueueImageWire where
  active : Option Nat
  pending : List Nat
  folding : List Nat
  terminal : List Nat
  deriving ToJson

def managementAfter (i : ManagementInput) : Option QueueImageWire := do
  let operation ← i.operation.toModel
  let after ← SessionQueue.ManagementCases.execute i.before.toModel operation
  pure { active := after.active, pending := after.pending.map (·.requestId)
         folding := after.folding.map (·.requestId), terminal := after.terminal.sort (· ≤ ·) }

def ManagementInput.ofCase (c : SessionQueue.ManagementCases.Case) : ManagementInput :=
  { name := c.name, before := StateWire.ofModel c.before
    operation := OperationWire.ofModel c.operation }

/-- The signed replacement owner issues each replacement as a fresh request in
the slot it replaces, so a replacement that reuses an original request ID or
names another slot is not something it can be asked to write. -/
def signedReplacement (c : SessionQueue.ManagementCases.Case) : Bool :=
  match c.operation with
  | .replace _ _ offset count replacements =>
      let originals := c.before.folding ++ c.before.pending
      let selected := (originals.drop offset).take count
      !(replacements.any fun r => originals.any (·.requestId == r.requestId)) &&
        (replacements.zip selected).all fun (r, o) => r.orderKey == o.orderKey
  | .intake .. => false

/-- The served expiry fixture claims the head, then enqueues one expired or
malformed steering row ahead of a fresh one; the other intake decisions need
a provider-stream boundary, a stale head observation or an unadmitted row,
which that fixture does not build. -/
def servedIntakeNames : List String := ["expired_steering_is_barrier"]

@[conformance_family]
def managementReplace : Family ManagementInput (Option QueueImageWire) where
  inputs := (SessionQueue.ManagementCases.cases.filter signedReplacement).map ManagementInput.ofCase
  model := managementAfter

@[conformance_family]
def managementExpiredIntake : Family ManagementInput (Option QueueImageWire) where
  inputs := (SessionQueue.ManagementCases.cases.filter (servedIntakeNames.contains ·.name)).map
    ManagementInput.ofCase
  model := managementAfter

/-- model-production-gap: the signed replacement owner generates replacement
request IDs and slots itself, and intake at an unsafe stream boundary, from a
stale head observation, behind an unadmitted, queued, foreign or agent-authored
row has no served fixture; steering publication and the owned loop replay the
composed intake. -/
@[conformance_family]
def managementUnreached : Family ManagementInput (Option QueueImageWire) where
  inputs := (SessionQueue.ManagementCases.cases.filter fun c =>
    !signedReplacement c && !servedIntakeNames.contains c.name).map ManagementInput.ofCase
  model := managementAfter

def roundTrips (entries : List QueueEntry) : Bool :=
  entries.all fun entry => (EntryWire.ofModel entry).toModel == entry

example : roundTrips (cases.flatMap fun (_, events) => events.filterMap fun event =>
    match event with
    | .enqueue entry => some entry
    | _ => none) := by native_decide

example : roundTrips (SessionQueue.ManagementCases.cases.flatMap fun c =>
    c.before.pending ++ c.before.folding ++ match c.operation with
      | .intake active _ _ => [active]
      | .replace _ _ _ _ replacements => replacements) := by
  native_decide

end Management

section InputEditClientObservation

open SessionQueue.InputEdit

deriving instance ToJson for SessionQueue.InputEdit.Outcome

structure InputEditClientObservationInput where
  receiptOutcome : SessionQueue.InputEdit.Outcome
  projectionRefreshSucceeded : Bool
  deriving ToJson

structure InputEditClientObservationOutput where
  commandOutcome : SessionQueue.InputEdit.Outcome
  projectionFresh : Bool
  deriving ToJson

@[conformance_family]
def inputEditClientObservation :
    Family InputEditClientObservationInput InputEditClientObservationOutput where
  inputs := SessionQueue.InputEditCases.clientObservationCases.map fun c =>
    { receiptOutcome := c.receiptOutcome, projectionRefreshSucceeded := c.projectionRefreshSucceeded }
  model := fun i =>
    let observation := SessionQueue.InputEditCases.runClientObservation
      ⟨"", i.receiptOutcome, i.projectionRefreshSucceeded⟩
    { commandOutcome := observation.commandOutcome, projectionFresh := observation.projectionFresh }

end InputEditClientObservation

section InputEdit

open SessionQueue.InputEdit

deriving instance ToJson for SessionQueue.InputEdit.Editor, SessionQueue.InputEdit.Authority,
  SessionQueue.InputEdit.Receipt

structure EditStateWire where
  queue : StateWire
  receipts : List SessionQueue.InputEdit.Receipt
  deriving ToJson

def EditStateWire.ofModel (s : SessionQueue.InputEdit.State) : EditStateWire :=
  { queue := StateWire.ofModel s.queue, receipts := s.receipts }

def EditStateWire.toModel (s : EditStateWire) : SessionQueue.InputEdit.State :=
  ⟨s.queue.toModel, s.receipts⟩

structure CommandWire where
  id : Nat
  digest : Nat
  caller : Option Nat
  issuedAt : Nat
  expiresAt : Nat
  expected : List Nat
  offset : Nat
  count : Nat
  replacements : List EntryWire
  deriving ToJson

def CommandWire.ofModel (c : SessionQueue.InputEdit.Command) : CommandWire :=
  { id := c.id, digest := c.digest, caller := c.caller, issuedAt := c.issuedAt
    expiresAt := c.expiresAt, expected := c.expected, offset := c.offset, count := c.count
    replacements := c.replacements.map EntryWire.ofModel }

def CommandWire.toModel (c : CommandWire) : SessionQueue.InputEdit.Command :=
  ⟨c.id, c.digest, c.caller, c.issuedAt, c.expiresAt, c.expected, c.offset, c.count,
    c.replacements.map EntryWire.toModel⟩

inductive InputEditStepKind where
  | apply | loseReceipts | otherSession
  deriving ToJson

structure InputEditStepWire where
  kind : InputEditStepKind
  now : Option Nat
  authority : Option Authority
  command : Option CommandWire
  session : Option Nat
  deriving ToJson

def InputEditStepWire.ofModel : SessionQueue.InputEditCases.Step → InputEditStepWire
  | .apply now a c =>
      { kind := .apply, now := some now, authority := some a
        command := some (CommandWire.ofModel c), session := none }
  | .loseReceipts =>
      { kind := .loseReceipts, now := none, authority := none, command := none, session := none }
  | .otherSession session =>
      { kind := .otherSession, now := none, authority := none, command := none
        session := some session }

def InputEditStepWire.toModel (step : InputEditStepWire) :
    Option SessionQueue.InputEditCases.Step :=
  match step.kind with
  | .apply => do pure (.apply (← step.now) (← step.authority) (← step.command).toModel)
  | .loseReceipts => some .loseReceipts
  | .otherSession => step.session.map .otherSession

structure InputEditCaseWire where
  name : String
  initial : EditStateWire
  history : List InputEditStepWire
  now : Nat
  command : CommandWire
  authority : Authority
  deriving ToJson

def InputEditCaseWire.ofCase (c : SessionQueue.InputEditCases.Case) : InputEditCaseWire :=
  { name := c.name, initial := EditStateWire.ofModel SessionQueue.InputEditCases.state
    history := c.history.map InputEditStepWire.ofModel, now := c.now
    command := CommandWire.ofModel c.command, authority := c.authority }

def InputEditCaseWire.run (i : InputEditCaseWire) :
    Option (SessionQueue.InputEdit.State × SessionQueue.InputEdit.State ×
      Option SessionQueue.InputEdit.Receipt) := do
  let history ← i.history.mapM InputEditStepWire.toModel
  let before := history.foldl SessionQueue.InputEditCases.Step.run i.initial.toModel
  let result := apply before.queue.scope i.now i.authority i.command.toModel before
  pure (before, result.1, result.2)

structure EditImageWire where
  queue : QueueImageWire
  receipts : List SessionQueue.InputEdit.Receipt
  deriving ToJson

def EditImageWire.ofModel (s : SessionQueue.InputEdit.State) : EditImageWire :=
  { queue := { active := s.queue.active, pending := s.queue.pending.map (·.requestId)
               folding := s.queue.folding.map (·.requestId)
               terminal := s.queue.terminal.sort (· ≤ ·) }
    receipts := s.receipts }

structure InputEditObservation where
  before : EditImageWire
  after : EditImageWire
  receipt : Option SessionQueue.InputEdit.Receipt
  deriving ToJson

def inputEditObservation (i : InputEditCaseWire) : Option InputEditObservation := do
  let (before, after, receipt) ← i.run
  pure { before := EditImageWire.ofModel before, after := EditImageWire.ofModel after, receipt }

/-- The session command table indexes `command_id` as unique, so the queue owner
stores at most one physical command per ID, and its fixture holds one session's
queue. -/
def atomicOwnerCase (c : SessionQueue.InputEditCases.Case) : Bool :=
  c.authority.commandIdentityUnique && c.history.all fun step => match step with
    | .apply _ a _ => a.commandIdentityUnique
    | .loseReceipts => true
    | .otherSession _ => false

def inputEditCasesNamed (names : List String) : List InputEditCaseWire :=
  (SessionQueue.InputEditCases.cases.filter (names.contains ·.name)).map InputEditCaseWire.ofCase

@[conformance_family]
def inputEditAtomic : Family InputEditCaseWire (Option InputEditObservation) where
  inputs := (SessionQueue.InputEditCases.cases.filter atomicOwnerCase).map InputEditCaseWire.ofCase
  model := inputEditObservation

/-- The enrolled User authors each endpoint edit through the signed client
endpoint under a fresh command ID, as the session's requester, with no prior
history, and the scenario waits for each receipt; another editor, a recorded
history and a deferred decision are replayed by the atomic queue owner family. -/
def endpointNames : List String :=
  ["local_self_applied", "expired_rejected", "signature_rejected",
   "replacement_signature_rejected", "stale_queue_rejected"]

/-- The endpoint's command shape owner refuses a command naming a session its
author does not own before publication, so the runtime never answers it; the
atomic queue owner family replays the runtime rejection. -/
def endpointRefusedNames : List String := ["foreign_session_rejected"]

structure EndpointImageWire where
  pending : List Nat
  terminal : List Nat
  receipts : List SessionQueue.InputEdit.Receipt
  deriving ToJson

def EndpointImageWire.ofModel (s : SessionQueue.InputEdit.State) : EndpointImageWire :=
  { pending := s.queue.pending.map (·.requestId), terminal := s.queue.terminal.sort (· ≤ ·)
    receipts := s.receipts }

structure EndpointObservation where
  before : EndpointImageWire
  after : EndpointImageWire
  receipt : Option SessionQueue.InputEdit.Receipt
  deriving ToJson

def endpointObservation (i : InputEditCaseWire) : Option EndpointObservation := do
  let (before, after, receipt) ← i.run
  pure { before := EndpointImageWire.ofModel before, after := EndpointImageWire.ofModel after
         receipt }

structure EndpointQueueWire where
  pending : List Nat
  terminal : List Nat
  deriving ToJson

structure EndpointRefusedObservation where
  after : EndpointQueueWire
  deriving ToJson

@[conformance_family]
def inputEditEndpoint : Family InputEditCaseWire (Option EndpointObservation) where
  inputs := inputEditCasesNamed endpointNames
  model := endpointObservation

@[conformance_family]
def inputEditEndpointRefused : Family InputEditCaseWire (Option EndpointRefusedObservation) where
  inputs := inputEditCasesNamed endpointRefusedNames
  model := fun i => do
    let (_, after, _) ← i.run
    pure { after := { pending := after.queue.pending.map (·.requestId)
                      terminal := after.queue.terminal.sort (· ≤ ·) } }

/-- The same signed intent published twice at once is one physical command; its
history step is that publication and the replay is a restarted reconciler
reading the recorded receipt, which is final whatever the clock. -/
@[conformance_family]
def inputEditConcurrentReplay : Family InputEditCaseWire (Option EndpointObservation) where
  inputs := inputEditCasesNamed ["exact_replay_after_expiry"]
  model := endpointObservation

/-- Two different intents race for one command ID at the endpoint; its unique
command identity admits one, which the model decides as a lone fresh command,
and the refused intent leaves its own session untouched. -/
@[conformance_family]
def inputEditRacingIntents : Family InputEditCaseWire (Option EndpointObservation) where
  inputs := inputEditCasesNamed ["local_self_applied"]
  model := endpointObservation

/-- A later intent reuses an applied command ID from another session. With a
unique command identity the model looks receipts up by command ID alone, so
the twin is the model's command-identity collision: no receipt answers it and
the prior receipt and queue stand. -/
@[conformance_family]
def inputEditLateTwin : Family InputEditCaseWire (Option EndpointObservation) where
  inputs := inputEditCasesNamed ["command_identity_collision"]
  model := endpointObservation

/-- The identical signed intent republished after its receipt is refused as a
second physical document; the recorded receipt answers it unchanged. -/
@[conformance_family]
def inputEditIdenticalRepublish : Family InputEditCaseWire (Option EndpointObservation) where
  inputs := inputEditCasesNamed ["exact_replay_after_expiry"]
  model := endpointObservation

/-- model-production-gap: `process_command` leaves a command unanswered when
another physical command shares its ID, but the session command table's unique
`command_id` index refuses the second document, so no fixture can store two
physical commands under one ID, before application or after a receipt in
another session. -/
@[conformance_family]
def inputEditUnreached : Family InputEditCaseWire (Option InputEditObservation) where
  inputs := (SessionQueue.InputEditCases.cases.filter (!atomicOwnerCase ·)).map
    InputEditCaseWire.ofCase
  model := inputEditObservation

example : (EditStateWire.ofModel SessionQueue.InputEditCases.state).toModel =
    SessionQueue.InputEditCases.state := by native_decide

example : SessionQueue.InputEditCases.cases.all fun c =>
    (InputEditCaseWire.ofCase c).run == some (c.before, SessionQueue.InputEditCases.run c) := by
  native_decide

example : (SessionQueue.InputEditCases.cases.filter atomicOwnerCase).length = 11 := by
  native_decide

example : (SessionQueue.InputEditCases.cases.filter (!atomicOwnerCase ·)).map (·.name) =
    ["simultaneous_command_identity_collision", "observed_collision_preserves_prior_receipt",
     "cross_session_collision_preserves_prior_receipt"] := by native_decide

example : (SessionQueue.InputEditCases.cases.filter (!atomicOwnerCase ·)).all fun c =>
    SessionQueue.InputEditCases.run c = (c.before, none) := by native_decide

example : (SessionQueue.InputEditCases.cases.find? (·.name == "exact_replay_after_expiry")).all
    fun replay => (SessionQueue.InputEditCases.run replay).1 == replay.before &&
      (SessionQueue.InputEditCases.cases.find? (·.name == "local_self_applied")).all fun c =>
        (SessionQueue.InputEditCases.run replay).2 == (SessionQueue.InputEditCases.run c).2 := by
  native_decide

end InputEdit

end Families

end Conformance.FoldQueueContracts
