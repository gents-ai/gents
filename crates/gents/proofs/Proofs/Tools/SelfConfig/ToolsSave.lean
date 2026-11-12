import Proofs.Documents.DocumentOperations

/-!
# Saving Tools: one admission for every client

A Tools document is saved by one handler whoever asks. The save is a patch of
its top-level groups; a group the request omits is kept.

Decision (maintainer, 2026-11-11, #3654): the refusals the config tool raised
for an Agent apply to everyone, except where one is about an Agent's own
authority.

* For everyone: a save that silently drops a stored setting from a group it
  rewrites is refused unless the call allows the drop for that group; a save
  that newly selects a plugin this node has not installed is refused; a save
  to Tools that more than one Agent reaches is refused until acknowledged, and
  the acknowledgement needs the agent catalog.
* About the actor: an operator grant a save newly carries must be held by the
  actor. A User may do what an operator may, so it holds every one
  (`holdsOperatorGrant`). An Agent that keeps the no-lockout switch on cannot
  save its own Tools into a state that removes its config access
  (`ownControlBinds`); no such rule binds a User, who is never locked out of
  its own Node.

Rust owner: `crates/gents-tools/src/operations/tools.rs` (`decide_tools_save`).
-/

namespace SelfConfig.ToolsSave

open DocumentOperations (Actor)

/-- An operator-managed grant (`SelfConfig.Grants`) is bounded by what the
actor holds. A User is the operator. -/
def holdsOperatorGrant (actor : Actor) (agentHolds : Bool) : Bool :=
  actor = .user || agentHolds

/-- The no-lockout rule binds an Agent editing the Tools it runs on. -/
def ownControlBinds (actor : Actor) (ownTools noLockout : Bool) : Bool :=
  actor = .agent && ownTools && noLockout

theorem user_holds_every_operator_grant (agentHolds : Bool) :
    holdsOperatorGrant .user agentHolds = true := rfl

theorem no_lockout_never_binds_a_user (ownTools noLockout : Bool) :
    ownControlBinds .user ownTools noLockout = false := rfl

/-- How many Agents reach the Tools being saved, directly or through a
Context. -/
inductive Reach where
  | unselected
  | one
  | several
  deriving DecidableEq, Repr

inductive SharedOutcome where
  | proceed
  | refusedUnacknowledged
  | refusedWithoutCatalog
  deriving DecidableEq, Repr

/-- The acknowledgement waives only the sharing refusal: a write that lands on
other Agents' configuration still needs the agent catalog. -/
def sharedOutcome (reach : Reach) (acknowledged holdsCatalog : Bool) : SharedOutcome :=
  match reach with
  | .several =>
      if !acknowledged then .refusedUnacknowledged
      else if !holdsCatalog then .refusedWithoutCatalog
      else .proceed
  | _ => .proceed

theorem shared_tools_need_acknowledgement_and_catalog (acknowledged holdsCatalog : Bool) :
    sharedOutcome .several acknowledged holdsCatalog = .proceed →
      acknowledged = true ∧ holdsCatalog = true := by
  cases acknowledged <;> cases holdsCatalog <;> decide

inductive Outcome where
  | saved
  | refusedShared
  | refusedGrantNotHeld
  | refusedDropsSettings
  | refusedPluginNotInstalled
  | refusedLosesOwnControl
  deriving DecidableEq, Repr

/-- The save's admission, first failure wins. `grantsHeld` is
`SelfConfig.keepsGrants` for the actor's held grants; `keepsOwnControl` is true
wherever `ownControlBinds` is false. -/
def outcome (shared : SharedOutcome)
    (grantsHeld dropsAllowed pluginsInstalled keepsOwnControl : Bool) : Outcome :=
  if shared ≠ .proceed then .refusedShared
  else if !grantsHeld then .refusedGrantNotHeld
  else if !dropsAllowed then .refusedDropsSettings
  else if !pluginsInstalled then .refusedPluginNotInstalled
  else if !keepsOwnControl then .refusedLosesOwnControl
  else .saved

theorem saved_tools_pass_every_check (shared : SharedOutcome)
    (grantsHeld dropsAllowed pluginsInstalled keepsOwnControl : Bool) :
    outcome shared grantsHeld dropsAllowed pluginsInstalled keepsOwnControl = .saved →
      shared = .proceed ∧ grantsHeld = true ∧ dropsAllowed = true ∧
        pluginsInstalled = true ∧ keepsOwnControl = true := by
  cases shared <;> cases grantsHeld <;> cases dropsAllowed <;> cases pluginsInstalled <;>
    cases keepsOwnControl <;> decide

/-- One setting of a group the save rewrites. -/
inductive SettingChange where
  | kept
  | changed
  | removed
  deriving DecidableEq, Repr

/-- A stored setting the rewritten group no longer carries is a drop unless the
call allows drops for that group. A moved plugin pin is `changed`, not
`removed`. -/
def dropRefused (change : SettingChange) (groupAllowed : Bool) : Bool :=
  change = .removed && !groupAllowed

theorem a_changed_setting_is_never_a_drop (groupAllowed : Bool) :
    dropRefused .changed groupAllowed = false := rfl

end SelfConfig.ToolsSave
