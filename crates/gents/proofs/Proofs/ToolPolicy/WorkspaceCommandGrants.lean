import Proofs.ToolPolicy.WorkspacePermissions

namespace ToolPolicy.WorkspaceCommandGrants

open WorkspacePermissions

inductive Choice where
  | once | remember | deny
  deriving DecidableEq, Repr

structure State where
  question : PluginApproval.State
  grants : List Grant
  deriving DecidableEq, Repr

/-- Action identities stand for collision-resistant digests of the complete
bound command, working/root/writable paths and execution policy. Adapters retain
no command arguments in the grant ledger. Remembering an invocation authorizes
that command under its captured ceiling, not another command or a wider policy.
As with filesystem grants, the permission is independent of answer delivery:
a later answer-file failure cannot be reported as delivered but need not revoke
an explicitly persisted permission. -/
def hasGrant (grants : List Grant) (expected : Grant) : Bool :=
  grants.contains expected

def canRememberCommand (kind : ApprovalKind) (supported : Bool) : Bool :=
  kind == .command && supported

def answer (available : List Scope) (expected : Grant) (state : State)
    (id : String) (now : Nat) (choice : Choice) (kind : ApprovalKind) (supported : Bool) : State × Bool :=
  if kind != .command || (choice == .remember && !canRememberCommand kind supported) then (state, false)
  else
    let ordinary := if choice == .deny then PluginApproval.Answer.deny else .once
    let (question, accepted) := answerApproval available expected state.question id now ordinary .command
    let grants := if accepted && choice == .remember && !hasGrant state.grants expected then
      state.grants ++ [expected] else state.grants
    ({ question, grants }, accepted)

theorem unsupported_cannot_be_remembered (available : List Scope) (expected : Grant)
    (state : State) (id : String) (now : Nat) (kind : ApprovalKind) :
    answer available expected state id now .remember kind false = (state, false) := by
  cases kind <;> simp [answer, canRememberCommand]

theorem once_keeps_grants (available : List Scope) (expected : Grant) (state : State)
    (id : String) (now : Nat) (kind : ApprovalKind) (supported : Bool) :
    (answer available expected state id now .once kind supported).1.grants = state.grants := by
  cases kind <;> simp [answer, canRememberCommand]

theorem deny_keeps_grants (available : List Scope) (expected : Grant) (state : State)
    (id : String) (now : Nat) (kind : ApprovalKind) (supported : Bool) :
    (answer available expected state id now .deny kind supported).1.grants = state.grants := by
  cases kind <;> simp [answer, canRememberCommand]

theorem noncommand_cannot_be_remembered (available : List Scope) (expected : Grant)
    (state : State) (id : String) (now : Nat) (choice : Choice) (kind : ApprovalKind) (supported : Bool)
    (other : kind ≠ .command) :
    answer available expected state id now choice kind supported = (state, false) := by
  simp [answer, other, canRememberCommand]

theorem changed_scope_cannot_add_a_grant (available : List Scope) (expected : Grant)
    (state : State) (id : String) (now : Nat) (choice : Choice) (supported : Bool)
    (changed : approvalScopeMatches available expected.scope = false) :
    (answer available expected state id now choice .command supported).1.grants = state.grants := by
  simp [answer, answerApproval, changed, canRememberCommand]

theorem cancelled_question_cannot_add_a_grant (available : List Scope) (expected : Grant)
    (state : State) (id : String) (now : Nat) (choice : Choice) (supported : Bool) :
    (answer available expected { state with question := PluginApproval.cancel state.question }
      id now choice .command supported).1.grants = state.grants := by
  simp [answer, answerApproval, PluginApproval.answer, PluginApproval.admits, PluginApproval.cancel, canRememberCommand]

theorem unadmitted_question_keeps_grants (available : List Scope) (expected : Grant)
    (state : State) (id : String) (now : Nat) (choice : Choice) (kind : ApprovalKind) (supported : Bool)
    (closed : PluginApproval.admits state.question id now = false) :
    (answer available expected state id now choice kind supported).1.grants = state.grants := by
  cases kind <;> simp [answer, answerApproval, PluginApproval.answer, closed, canRememberCommand]

/-- Reset advances the same permission revision used by pending answers. This
also rejects a pre-reset question that has not yet delivered its remembered
permission. Mode, reviewer selection and independent filesystem grants remain
with their existing owners. The host persists this transition under the existing
workspace permission lock and reports write failures rather than success. -/
def revoke (previous : Preference) (expected : Nat) : WriteResult :=
  if previous.revision != expected || previous.revision >= maxRevision then
    { accepted := false, changed := false, next := some previous }
  else
    { accepted := true, changed := true,
      next := some { previous with revision := previous.revision + 1 } }

def revokeLedger (previous : Preference) (expected : Nat) (grants : List Grant) :
    WriteResult × List Grant :=
  let result := revoke previous expected
  (result, if result.accepted then [] else grants)

theorem accepted_revoke_clears_grants (previous : Preference) (expected : Nat)
    (grants : List Grant) (accepted : (revoke previous expected).accepted = true) :
    (revokeLedger previous expected grants).2 = [] := by
  simp [revokeLedger, accepted]

theorem stale_revoke_preserves_selection (previous : Preference) (expected : Nat)
    (stale : previous.revision ≠ expected) :
    (revoke previous expected).next = some previous := by
  simp [revoke, stale]

theorem revoked_revision_changes (previous : Preference) (expected : Nat)
    (same : previous.revision = expected) (bounded : previous.revision < maxRevision) :
    (revoke previous expected).next = some { previous with revision := previous.revision + 1 } := by
  simp [revoke, same, Nat.not_le.mpr (same ▸ bounded)]

end ToolPolicy.WorkspaceCommandGrants
