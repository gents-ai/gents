import Proofs.Tools.ToolPolicy.PluginApproval

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

/-- Workspace consent is an optional restriction on native command authority,
not its source. A command runs inside one selected workspace whose configured
ceiling already admitted it and for which no command grant was recorded; a
read-only command does not cross the workspace boundary. -/
def commandInput (mode : Mode) (readOnly : Bool) : Input :=
  { current := { workspace := 0, revision := 1 }
    configuredWorkspace := 0
    mode := mode
    action := 0
    ceilingAllows := true
    crossesBoundary := !readOnly }

/-- A caller with no selected workspace preference is never routed: it keeps its
configured policy. -/
def commandRoute (preference : Option Mode) (readOnly : Bool) : Option Route :=
  preference.map fun mode => route (commandInput mode readOnly)

/-- The route after review when no reviewer is attached to the request. -/
def unattendedRoute (preference : Option Mode) (readOnly : Bool) : Option Route :=
  preference.map fun mode =>
    let input := commandInput mode readOnly
    reviewedRoute input .unavailable { scope := input.current, action := input.action }

def needsPerson (preference : Option Mode) (readOnly : Bool) : Bool :=
  match unattendedRoute preference readOnly with
  | some .ask | some .review => true
  | _ => false

/-- A command routed to a person runs only on that person's answer; a caller
that cannot ask (not interactive) is refused instead. -/
def commandRuns (preference : Option Mode) (readOnly : Bool) : Bool :=
  match unattendedRoute preference readOnly with
  | none | some .allow => true
  | _ => false

def asksPerson (preference : Option Mode) (readOnly interactive : Bool) : Bool :=
  interactive && needsPerson preference readOnly

theorem callers_without_workspace_preferences_keep_their_configured_policy
    (readOnly interactive : Bool) :
    commandRoute none readOnly = none ∧ commandRuns none readOnly = true ∧
      asksPerson none readOnly interactive = false := by
  cases interactive <;> simp [commandRoute, commandRuns, unattendedRoute, asksPerson, needsPerson]

theorem strict_ask_refuses_an_unattended_command (readOnly : Bool) :
    commandRuns (some .ask) readOnly = false := by
  cases readOnly <;> decide

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

/-- The host consent store addresses a stored selection by a key. Node,
requester, agent and the canonical workspace folder are abstracted into
`workspace`: canonicalizing them is the host adapter's premise. The session
component is absent for the agent and workspace default entry and present for
one session's own entry. -/
structure Key where
  workspace : Nat
  session : Option Nat
  deriving DecidableEq, Repr

structure Entry where
  key : Key
  preference : Preference
  deriving DecidableEq, Repr

/-- Only a request's own session entry and the agent and workspace default are
candidates for it. A sibling session's entry is never one. -/
def candidate (workspace : Nat) (session : Option Nat) (key : Key) : Bool :=
  key.workspace == workspace && (key.session == none || key.session == session)

/-- A request resolves to its own session's selection when one is stored and
otherwise to the agent and workspace default. Absence of both leaves the
workspace consent layer disengaged under the configured ceiling, so resolving
a session that has made no selection to nothing would drop the gate its
default configured; the default is the only stored object carrying that
selection's reviewer and allowed folders. A request carrying no session, such
as a scheduled run, takes the same path and can never reach a session entry. -/
def resolve (entries : List Entry) (workspace : Nat) (session : Option Nat) :
    Option Entry :=
  let own : Option Entry := match session with
    | none => none
    | some s => entries.find? fun entry => entry.key == { workspace := workspace, session := some s }
  match own with
  | some entry => some entry
  | none => entries.find? fun entry => entry.key == { workspace := workspace, session := none }

/-- An action may resume only under the selection it was authorized by. A
session entry stored while its question was pending changes the resolution and
withdraws a binding captured under the default. -/
def resolutionAdmits (entries : List Entry) (workspace : Nat) (session : Option Nat)
    (bound : Key) : Bool :=
  (resolve entries workspace session).map Entry.key == some bound

theorem session_selection_wins (entries : List Entry) (workspace session : Nat)
    (entry : Entry)
    (own : entries.find? (fun e => e.key == { workspace := workspace, session := some session })
      = some entry) :
    resolve entries workspace (some session) = some entry := by
  simp [resolve, own]

theorem absent_session_uses_the_default (entries : List Entry) (workspace session : Nat)
    (missing : entries.find? (fun e => e.key == { workspace := workspace, session := some session })
      = none) :
    resolve entries workspace (some session) = resolve entries workspace none := by
  simp [resolve, missing]

/-- The gate a user configured for an agent and folder still governs a session
that has made no selection: resolution never returns nothing while a default
is stored. -/
theorem a_new_session_cannot_lose_the_default_gate (entries : List Entry) (workspace : Nat)
    (session : Option Nat) (configured : resolve entries workspace none ≠ none) :
    resolve entries workspace session ≠ none := by
  cases session with
  | none => exact configured
  | some s =>
    simp only [resolve]
    cases own : entries.find? (fun e => e.key == { workspace := workspace, session := some s }) with
    | some entry => simp
    | none => simpa [resolve] using configured

/-- Whatever governs a request is one of its own two candidates, so a sibling
session's selection is never inherited. -/
theorem a_sibling_session_is_never_inherited (entries : List Entry) (workspace : Nat)
    (session : Option Nat) (entry : Entry)
    (resolved : resolve entries workspace session = some entry) :
    candidate workspace session entry.key = true := by
  cases session with
  | none =>
    simp only [resolve] at resolved
    have := List.find?_some resolved
    simp only [beq_iff_eq] at this
    simp [candidate, this]
  | some s =>
    simp only [resolve] at resolved
    cases own : entries.find? (fun e => e.key == { workspace := workspace, session := some s }) with
    | some found =>
      rw [own] at resolved
      have := List.find?_some own
      simp only [beq_iff_eq] at this
      simp_all [candidate]
    | none =>
      rw [own] at resolved
      have := List.find?_some resolved
      simp only [beq_iff_eq] at this
      simp [candidate, this]

theorem no_session_request_uses_the_default (entries : List Entry) (workspace : Nat)
    (entry : Entry) (resolved : resolve entries workspace none = some entry) :
    entry.key.session = none := by
  have := a_sibling_session_is_never_inherited entries workspace none entry resolved
  simp [candidate] at this
  simp [this]

theorem a_resolved_entry_is_stored (entries : List Entry) (workspace : Nat)
    (session : Option Nat) (entry : Entry)
    (resolved : resolve entries workspace session = some entry) :
    entry ∈ entries := by
  cases session with
  | none => exact List.mem_of_find?_eq_some (by simpa [resolve] using resolved)
  | some s =>
    simp only [resolve] at resolved
    cases own : entries.find? (fun e => e.key == { workspace := workspace, session := some s }) with
    | some found =>
      rw [own] at resolved
      cases resolved
      exact List.mem_of_find?_eq_some own
    | none =>
      rw [own] at resolved
      exact List.mem_of_find?_eq_some resolved

/-- Resolution selects a stored mode; it cannot raise the configured ceiling. -/
theorem a_resolved_selection_cannot_exceed_the_ceiling (entries : List Entry)
    (workspace : Nat) (session : Option Nat) (entry : Entry) (input : Input)
    (_resolved : resolve entries workspace session = some entry)
    (_selected : input.mode = entry.preference.mode)
    (ceiling : input.ceilingAllows = false) :
    route input = .deny := by
  simp [route, ceiling]

theorem a_sibling_binding_is_never_admitted (entries : List Entry) (workspace : Nat)
    (session : Option Nat) (bound : Key)
    (other : bound.session ≠ none) (sibling : bound.session ≠ session) :
    resolutionAdmits entries workspace session bound = false := by
  simp only [resolutionAdmits]
  cases resolved : resolve entries workspace session with
  | none => simp
  | some entry =>
    have cand := a_sibling_session_is_never_inherited entries workspace session entry resolved
    simp only [candidate, Bool.and_eq_true, Bool.or_eq_true, beq_iff_eq] at cand
    simp only [Option.map_some', beq_eq_false_iff_ne, ne_eq, Option.some.injEq]
    intro equal
    rcases cand.2 with h | h <;> rw [← equal] at other sibling <;> simp_all

/-- A chat that chooses its own selection withdraws the folder default's
authority over it, including for an action already bound under the default. -/
theorem a_session_selection_invalidates_a_default_binding (entries : List Entry)
    (workspace session : Nat) (entry : Entry) (bound : Key)
    (own : entries.find? (fun e => e.key == { workspace := workspace, session := some session })
      = some entry)
    (fromDefault : bound.session = none) :
    resolutionAdmits entries workspace (some session) bound = false := by
  have resolved : resolve entries workspace (some session) = some entry := by
    simp [resolve, own]
  have stored := List.find?_some own
  simp only [beq_iff_eq] at stored
  simp only [resolutionAdmits, resolved, Option.map_some', beq_eq_false_iff_ne, ne_eq,
    Option.some.injEq]
  intro equal
  rw [← equal, stored] at fromDefault
  simp at fromDefault

structure ResolutionCase where
  entries : List Entry
  workspace : Nat
  session : Option Nat
  bound : Key

def resolutionKeys : List Key :=
  [ { workspace := 1, session := none }
  , { workspace := 1, session := some 1 }
  , { workspace := 1, session := some 2 }
  , { workspace := 2, session := none }
  , { workspace := 2, session := some 1 } ]

def resolutionPreference (key : Key) : Preference :=
  let revision := key.workspace * 10 + (match key.session with | none => 0 | some s => s)
  let mode : Mode := match key.session with
    | none => Mode.ask
    | some 1 => Mode.fullAccess
    | some _ => Mode.autoReview
  let reviewTask : Option Nat := match key.session with | none => some 7 | some _ => none
  { revision := revision, mode := mode, reviewTask := reviewTask }

def resolutionEntry (key : Key) : Entry :=
  { key := key, preference := resolutionPreference key }

/-- Every subset of the stored keys, so the cases cover an empty store, a
default alone, a session alone, both, a sibling alone and two siblings. -/
def keySubsets : List Key → List (List Key)
  | [] => [[]]
  | key :: rest =>
    let tails := keySubsets rest
    tails.map (fun tail => key :: tail) ++ tails

def resolutionCases : List ResolutionCase := Id.run do
  let bounds : List Key :=
    [ { workspace := 1, session := none }
    , { workspace := 1, session := some 1 }
    , { workspace := 1, session := some 2 }
    , { workspace := 2, session := none } ]
  let mut rows : List ResolutionCase := []
  for keys in keySubsets resolutionKeys do
    let entries := keys.map resolutionEntry
    for workspace in [1, 2] do
      for session in [none, some 1, some 2, some 3] do
        for bound in bounds do
          rows := rows ++ [(⟨entries, workspace, session, bound⟩ : ResolutionCase)]
  return rows

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

/-- Native file tools consult workspace consent only for a mutation under a
saved preference. With no saved preference the workspace imposes no gate, and a
preview (dry run) changes nothing, so it is routine under any mode. -/
def fileToolRoute (saved : Option Input) (dryRun : Bool) : Route :=
  match saved with
  | none => .allow
  | some input => if dryRun then .allow else route input

theorem previews_and_callers_without_preferences_keep_their_existing_access
    (saved : Option Input) (dryRun : Bool)
    (unsaved : saved = none ∨ dryRun = true) :
    fileToolRoute saved dryRun = .allow := by
  rcases unsaved with h | h
  · simp [fileToolRoute, h]
  · subst h
    cases saved <;> simp [fileToolRoute]

end ToolPolicy.WorkspacePermissions
