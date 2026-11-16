import Proofs.Runtime.RuntimeReconcile.UpdateHandoff
import Proofs.Conformance.Functions.Runtime
import Proofs.Conformance.Family
import Lean

namespace Conformance.UpdateHandoff

open Lean _root_.RuntimeReconcile.UpdateHandoff

inductive ActionKind where
  | register | acknowledge | retire | release | backup | install | verify | fail

deriving instance Lean.ToJson for ActionKind

structure HandoffCase where
  phase : Phase
  action : ActionKind
  flag : Bool
  deriving ToJson

structure HandoffVerdict where
  canRestore : Bool
  canRestoreSameBuild : Bool
  next : Option Phase
  deriving ToJson

private def handoffAction : ActionKind → Bool → Action
  | .register, flag => .register flag
  | .acknowledge, _ => .acknowledge
  | .retire, flag => .retire flag
  | .release, flag => .release flag
  | .backup, flag => .backup flag
  | .install, flag => .install flag
  | .verify, flag => .verify flag
  | .fail, _ => .fail

private def actionFlags : List (ActionKind × Bool) :=
  [false, true].flatMap (fun flag =>
    [(ActionKind.register, flag), (ActionKind.retire, flag), (ActionKind.release, flag),
     (ActionKind.backup, flag), (ActionKind.install, flag), (ActionKind.verify, flag)]) ++
  [(ActionKind.acknowledge, false), (ActionKind.fail, false)]

@[conformance_family]
def transitions : Conformance.Family HandoffCase HandoffVerdict where
  inputs := [Phase.staged, .registered, .accepted, .retired, .released, .backedUp,
      .installed, .healthy, .failed].flatMap fun phase =>
    actionFlags.map fun (action, flag) => { phase := phase, action := action, flag := flag }
  model := fun c => {
    canRestore := canRestore c.phase c.flag false,
    canRestoreSameBuild := canRestore c.phase c.flag true,
    next := step c.phase (handoffAction c.action c.flag) }

structure PreflightCase where
  verified : Bool
  keychainCustody : Bool
  keyReadableWithoutInteraction : Bool
  deriving ToJson

@[conformance_family]
def preflight : Conformance.Family PreflightCase (Option Phase) where
  inputs := [false, true].flatMap fun verified =>
    [false, true].flatMap fun keychain =>
      [false, true].map fun readable => {
        verified := verified, keychainCustody := keychain,
        keyReadableWithoutInteraction := readable }
  model := fun c => _root_.RuntimeReconcile.UpdateHandoff.preflight
    c.verified c.keychainCustody c.keyReadableWithoutInteraction

end Conformance.UpdateHandoff
