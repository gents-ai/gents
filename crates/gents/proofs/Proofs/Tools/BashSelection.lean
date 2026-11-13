import Lean
import Proofs.Session.CommandPolicy.Cases

namespace Tools.BashSelection
open Lean

structure Input where
  mode : String
  execution : Option String
  allowlist : Option (List String)
  background : Bool
  deriving ToJson

structure Policy where
  mode : String
  allowedArgvPrefixes : List (List String)
  forbiddenArgvPrefixes : List (List String)
  networkMode : String
  readOnlyAllowlist : List String
  denyAllArgv : Bool
  denyGitMetadataWrites : Bool
  deriving ToJson

structure Observation where
  policy : Option Policy
  background : List String
  deriving ToJson

/-- Only unrestricted Bash adopts a write-capable execution mode. An empty
custom allowlist is absent; read-only tools use the established default policy. -/
def select (input : Input) : Observation :=
  let allowlist := input.allowlist.filter (!·.isEmpty)
  let hasPolicy := input.execution.isSome || allowlist.isSome
  let mode := if input.mode == "unrestricted" then input.execution.getD "unrestricted" else "read_only"
  let policy := if !hasPolicy && input.mode != "unrestricted" then none else
    some ⟨mode, [], [], "inherit",
      if mode == "read_only" then allowlist.getD CommandPolicy.defaultReadOnlyAllowlist else [],
      false, false⟩
  ⟨policy, if input.background && input.mode != "off" then
    [if input.mode == "unrestricted" then "bash_unrestricted" else "bash"] else []⟩

end Tools.BashSelection
