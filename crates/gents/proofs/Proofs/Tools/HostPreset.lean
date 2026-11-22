import Proofs.Session.CommandPolicy.Sandbox

namespace Tools.HostPreset
open CommandPolicy

/-- The host tool packages a User names: `init --tool-package` (`--write`,
`--yolo`) and the self-config `preset` of `agent create`/`edit`. -/
inductive Package where
  | readOnly
  | write
  | unrestricted
  deriving DecidableEq, Repr

/-- The bash a package writes into its Tools document: the bash mode, the
command execution mode and whether commands may run in the background. -/
structure Commands where
  bashMode : String
  execution : Option ExecutionMode
  background : Bool
  deriving DecidableEq, Repr

/-- The bash a package grants on a host that does or does not enforce the
workspace-write sandbox (`selectSandbox`'s host fact).

Decision (maintainer, 2026-11-22): the write package promises write access
with sandboxed command execution. On a host that cannot enforce the sandbox it
grants no command execution, never unsandboxed commands; unsandboxed commands
come only from the unrestricted package, which the User names deliberately
(`--yolo`). -/
def bash (package : Package) (workspaceWriteSandboxEnforced : Bool) : Option Commands :=
  match package with
  | .readOnly => some ⟨"read_only", none, false⟩
  | .write =>
      if workspaceWriteSandboxEnforced then some ⟨"unrestricted", some .workspaceWrite, true⟩
      else none
  | .unrestricted => some ⟨"unrestricted", some .unrestricted, true⟩

/-- Every command the write package grants runs under the sandbox the host
selects for it. -/
theorem write_commands_run_sandboxed (enforced : Bool) (c : Commands)
    (h : bash .write enforced = some c) :
    c.execution = some .workspaceWrite ∧
    selectSandbox { workspaceWriteSandboxEnforced := enforced } .workspaceWrite =
      .selected .macosSeatbelt := by
  cases enforced with
  | false => simp [bash] at h
  | true =>
    simp [bash] at h
    subst h
    exact ⟨rfl, by simp [selectSandbox]⟩

theorem write_without_sandbox_grants_no_commands : bash .write false = none := rfl

/-- Only the unrestricted package grants unsandboxed commands, on any host. -/
theorem only_unrestricted_runs_unsandboxed (package : Package) (enforced : Bool) (c : Commands)
    (h : bash package enforced = some c) (unsandboxed : c.execution = some .unrestricted) :
    package = .unrestricted := by
  cases package with
  | unrestricted => rfl
  | readOnly =>
    simp [bash] at h
    subst h
    simp at unsandboxed
  | write =>
    cases enforced with
    | false => simp [bash] at h
    | true =>
      simp [bash] at h
      subst h
      simp at unsandboxed

/-- The host fact changes only the write package. -/
theorem host_changes_only_write (package : Package) (h : package ≠ .write) :
    bash package true = bash package false := by
  cases package <;> simp_all [bash]

end Tools.HostPreset
