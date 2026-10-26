import Proofs.ToolPolicy.PluginApproval

namespace ToolPolicy.WorkspacePermissions

inductive Mode where
  | ask | autoReview | fullAccess
  deriving DecidableEq, Repr

inductive Route where
  | allow | ask | review | deny
  deriving DecidableEq, Repr

structure Scope where
  workspace : Nat
  revision : Nat
  deriving DecidableEq, Repr

structure Grant where
  scope : Scope
  action : Nat
  deriving DecidableEq, Repr

structure Input where
  current : Scope
  configuredWorkspace : Nat
  mode : Mode
  action : Nat
  ceilingAllows : Bool
  crossesBoundary : Bool
  grant : Option Grant := none
  deriving DecidableEq, Repr

/-- Workspace mode changes consent routing, not the configured tool ceiling,
workspace authority, document ACP, or another workspace's permissions.
A grant names the immutable action and the policy revision under which it was
reviewed; host adapters must canonicalize paths and bind their complete command
arguments before constructing these facts. -/
def validGrant (input : Input) : Bool :=
  match input.grant with
  | none => false
  | some grant => decide (grant.scope = input.current ∧ grant.action = input.action)

def effectiveMode (input : Input) : Mode :=
  if input.configuredWorkspace = input.current.workspace then input.mode else .ask

/-- Strict Ask requires a fresh decision for each governed action, including
routine actions and previously remembered invocations. Automatic review alone
may reuse routine classification or a revision-bound grant; neither mode can
exceed the configured ceiling. -/
def route (input : Input) : Route :=
  if !input.ceilingAllows then .deny
  else if effectiveMode input = .ask then .ask
  else if !input.crossesBoundary || validGrant input then .allow
  else match effectiveMode input with
    | .ask => .ask
    | .autoReview => .review
    | .fullAccess => .allow

inductive Review where
  | allow | ask | deny | unavailable
  deriving DecidableEq, Repr

/-- Automatic review may authorize only the action that was reviewed. An
unavailable reviewer falls back to a person; it cannot remove the workspace
boundary. Review accuracy is an external premise, not a theorem of routing. -/
def reviewedRoute (input : Input) (review : Review) (grant : Grant) : Route :=
  if route input ≠ .review then route input
  else if grant.scope ≠ input.current ∨ grant.action ≠ input.action then .deny
  else match review with
    | .allow => .allow
    | .ask | .unavailable => .ask
    | .deny => .deny

theorem ceiling_is_preserved (input : Input) (h : input.ceilingAllows = false) :
    route input = .deny := by simp [route, h]

theorem strict_ask_requires_a_person (input : Input)
    (h : input.ceilingAllows = true) (mode : effectiveMode input = .ask) :
    route input = .ask := by simp [route, h, mode]

theorem routine_actions_do_not_prompt (input : Input)
    (h : input.ceilingAllows = true) (mode : effectiveMode input ≠ .ask)
    (boundary : input.crossesBoundary = false) :
    route input = .allow := by simp [route, h, mode, boundary]

theorem another_workspace_cannot_inherit_full_access (input : Input)
    (different : input.configuredWorkspace ≠ input.current.workspace) :
    effectiveMode input = .ask := by
  simp [effectiveMode, different]

theorem stale_grant_cannot_authorize (input : Input) (grant : Grant)
    (current : input.grant = some grant) (stale : grant.scope ≠ input.current) :
    validGrant input = false := by
  simp [validGrant, current, stale]

theorem another_action_cannot_reuse_a_grant (input : Input) (grant : Grant)
    (current : input.grant = some grant) (different : grant.action ≠ input.action) :
    validGrant input = false := by
  simp [validGrant, current, different]

theorem unavailable_review_requires_a_person (input : Input) (grant : Grant)
    (routed : route input = .review) (sameScope : grant.scope = input.current)
    (sameAction : grant.action = input.action) :
    reviewedRoute input .unavailable grant = .ask := by
  simp [reviewedRoute, routed, sameScope, sameAction]

theorem review_cannot_authorize_another_action (input : Input) (grant : Grant)
    (routed : route input = .review) (different : grant.action ≠ input.action) :
    reviewedRoute input .allow grant = .deny := by
  simp [reviewedRoute, routed, different]

/-- Workspace observations come from the host preference owner. That owner
rejects duplicate workspace keys before supplying these facts. A question
never selects a different workspace's settings as a fallback. -/
def approvalScope (available : List Scope) (expected : Scope) : Option Scope :=
  available.find? fun current => current.workspace == expected.workspace

def approvalScopeMatches (available : List Scope) (expected : Scope) : Bool :=
  approvalScope available expected == some expected

inductive ApprovalKind where
  | filesystem | command | fileMutation
  deriving DecidableEq, Repr

/-- Commands and native file mutations bind a complete immutable invocation.
Remembering a path cannot authorize a different invocation, including a later
creation at a currently absent target; these questions admit only an allow-once
or deny answer. Pack filesystem grants retain their explicit path scope. -/
def answerAllowed (kind : ApprovalKind) (remembered : Bool) : Bool :=
  kind == .filesystem || !remembered

def rememberedAnswer : ToolPolicy.PluginApproval.Answer → Bool
  | .path | .folder => true
  | .once | .deny => false

/-- Scoped answers reuse the ordinary question's expiry, cancellation and
single-answer rules. Native adapters persist remembered paths only under
the question's exact workspace key and keep their queue format separate
from unscoped questions that older responders can answer. -/
def answerApproval (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat)
    (choice : ToolPolicy.PluginApproval.Answer) (kind : ApprovalKind := .filesystem) :
    ToolPolicy.PluginApproval.State × Bool :=
  if approvalScopeMatches available expected.scope && answerAllowed kind (rememberedAnswer choice) then
    ToolPolicy.PluginApproval.answer state id now choice
  else (state, false)

/-- A delivered answer is checked again when the waiting call resumes.
Changing workspace permissions after an answer was written invalidates
that answer; an action cannot consume another action's delivery. Explicit
remembered paths retain the ordinary approval owner's independent lifetime. -/
def consumeApproval (available : List Scope) (expected received : Grant)
    (allowed : Bool) : Option Bool :=
  let input : Input := {
    current := expected.scope
    configuredWorkspace := expected.scope.workspace
    mode := .ask
    action := expected.action
    ceilingAllows := true
    crossesBoundary := true
    grant := some received }
  if approvalScopeMatches available expected.scope && validGrant input then some allowed else none

theorem unavailable_workspace_cannot_receive_an_answer (expected : Grant) (kind : ApprovalKind)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat)
    (choice : ToolPolicy.PluginApproval.Answer) :
    answerApproval [] expected state id now choice kind = (state, false) := by
  simp [answerApproval, approvalScopeMatches, approvalScope]

theorem a_changed_selection_rejects_the_answer (available : List Scope) (expected : Grant)
    (kind : ApprovalKind)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat)
    (choice : ToolPolicy.PluginApproval.Answer)
    (changed : approvalScopeMatches available expected.scope = false) :
    answerApproval available expected state id now choice kind = (state, false) := by
  simp [answerApproval, changed]

theorem command_questions_cannot_remember_a_file (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat) :
    answerApproval available expected state id now .path .command = (state, false) := by
  simp [answerApproval, answerAllowed, rememberedAnswer]

theorem command_questions_cannot_remember_a_folder (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat) :
    answerApproval available expected state id now .folder .command = (state, false) := by
  simp [answerApproval, answerAllowed, rememberedAnswer]

theorem file_mutations_cannot_remember_a_path (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat) :
    answerApproval available expected state id now .path .fileMutation = (state, false) := by
  simp [answerApproval, answerAllowed, rememberedAnswer]

theorem file_mutations_cannot_remember_a_folder (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat) :
    answerApproval available expected state id now .folder .fileMutation = (state, false) := by
  simp [answerApproval, answerAllowed, rememberedAnswer]

theorem transient_answers_use_the_same_admission (available : List Scope) (expected : Grant)
    (state : ToolPolicy.PluginApproval.State) (id : String) (now : Nat)
    (choice : ToolPolicy.PluginApproval.Answer) (transient : rememberedAnswer choice = false) :
    answerApproval available expected state id now choice .command =
      answerApproval available expected state id now choice .filesystem := by
  simp [answerApproval, answerAllowed, transient]

theorem a_changed_selection_rejects_delivery (available : List Scope)
    (expected received : Grant) (allowed : Bool)
    (changed : approvalScopeMatches available expected.scope = false) :
    consumeApproval available expected received allowed = none := by
  simp [consumeApproval, changed]

theorem a_delivery_cannot_authorize_another_action (available : List Scope)
    (expected received : Grant) (allowed : Bool) (different : received.action ≠ expected.action) :
    consumeApproval available expected received allowed = none := by
  simp [consumeApproval, validGrant, different]

theorem a_delivery_cannot_authorize_another_workspace (available : List Scope)
    (expected received : Grant) (allowed : Bool) (different : received.scope ≠ expected.scope) :
    consumeApproval available expected received allowed = none := by
  simp [consumeApproval, validGrant, different]

def Mode.wireName : Mode → String
  | .ask => "ask"
  | .autoReview => "autoReview"
  | .fullAccess => "fullAccess"

def Route.wireName : Route → String
  | .allow => "allow"
  | .ask => "ask"
  | .review => "review"
  | .deny => "deny"

def Review.wireName : Review → String
  | .allow => "allow"
  | .ask => "ask"
  | .deny => "deny"
  | .unavailable => "unavailable"

def ApprovalKind.wireName : ApprovalKind → String
  | .filesystem => "filesystem"
  | .command => "command"
  | .fileMutation => "fileMutation"

structure Case where
  input : Input
  review : Review
  reviewedGrant : Grant

def cases : List Case := Id.run do
  let current : Scope := { workspace := 1, revision := 2 }
  let same : Grant := { scope := current, action := 3 }
  let stale : Grant := { scope := { current with revision := 1 }, action := 3 }
  let otherWorkspace : Grant := { scope := { current with workspace := 2 }, action := 3 }
  let otherAction : Grant := { same with action := 4 }
  let mut cases := []
  for mode in [Mode.ask, .autoReview, .fullAccess] do
    for configuredWorkspace in [1, 2] do
      for ceilingAllows in [false, true] do
        for crossesBoundary in [false, true] do
          for grant in [none, some same, some stale, some otherWorkspace, some otherAction] do
            let input : Input := {
              current := current
              configuredWorkspace := configuredWorkspace
              mode := mode
              action := 3
              ceilingAllows := ceilingAllows
              crossesBoundary := crossesBoundary
              grant := grant }
            if route input = .review then
              for review in [Review.allow, .ask, .deny, .unavailable] do
                for reviewedGrant in [same, stale, otherWorkspace, otherAction] do
                  cases := cases ++ [{ input := input, review := review, reviewedGrant := reviewedGrant }]
            else
              cases := cases ++ [{ input, review := .allow, reviewedGrant := same }]
  return cases

structure Preference where
  revision : Nat
  mode : Mode
  reviewTask : Option Nat
  deriving DecidableEq, Repr

structure WriteResult where
  accepted : Bool
  changed : Bool
  next : Option Preference
  deriving DecidableEq, Repr

def maxRevision : Nat := 18446744073709551615

/-- Only the observed revision may change. A retained reviewer remains bound
when a user switches modes; a no-op consumes no revision or filesystem write. -/
def selectMode (previous : Option Preference) (expected : Nat) (mode : Mode)
    (requestedTask : Option Nat) : WriteResult := Id.run do
  let revision := (previous.map Preference.revision).getD 0
  let reviewTask := requestedTask.or (previous.bind Preference.reviewTask)
  if revision != expected || (mode == .autoReview && reviewTask.isNone) then
    return { accepted := false, changed := false, next := previous }
  if previous.isSome && previous.map Preference.mode == some mode &&
      previous.bind Preference.reviewTask == reviewTask then
    return { accepted := true, changed := false, next := previous }
  if revision >= maxRevision then
    return { accepted := false, changed := false, next := previous }
  return {
    accepted := true
    changed := true
    next := some {
      revision := revision + 1
      mode := mode
      reviewTask := reviewTask } }

theorem stale_write_keeps_the_prior_selection (previous : Option Preference)
    (expected : Nat) (mode : Mode) (requestedTask : Option Nat)
    (stale : (previous.map Preference.revision).getD 0 ≠ expected) :
    (selectMode previous expected mode requestedTask).next = previous := by
  simp [selectMode, stale, Id.run]

structure WriteCase where
  previous : Option Preference
  expected : Nat
  mode : Mode
  requestedTask : Option Nat

def writeCases : List WriteCase := Id.run do
  let mut rows := []
  let previous : List (Option Preference) := none :: ([0, 1, maxRevision].flatMap fun revision =>
    [Mode.ask, .autoReview, .fullAccess].flatMap fun mode =>
      [none, some 1].map fun reviewTask => some { revision := revision, mode := mode, reviewTask := reviewTask })
  for before in previous do
    for expected in [0, 1, maxRevision] do
      for mode in [Mode.ask, .autoReview, .fullAccess] do
        for requestedTask in [none, some 1, some 2] do
          rows := rows ++ [{ previous := before, expected := expected, mode := mode, requestedTask := requestedTask }]
  return rows

end ToolPolicy.WorkspacePermissions
