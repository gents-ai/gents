import Proofs.Tools.ToolPolicy.Configuration
import Proofs.Conformance.Family

namespace Conformance.ToolTimeouts
open ToolPolicy Lean

structure Pair where
  default : Nat
  maximum : Nat
  deriving ToJson

private def pair? : Option (Nat × Nat) → Option Pair
  | none => none
  | some (d, m) => some { default := d, maximum := m }

structure Authored where
  timeoutSecs : Option Int
  maxTimeoutSecs : Option Int
  deriving ToJson

private def foregroundCases : List (String × Nat × Nat × Option Int × Option Int) := [
  ("unconfigured_uses_host_pair", 120, 120, none, none),
  ("unconfigured_keeps_decoupled_host_maximum", 120, 3600, none, none),
  ("authored_default_fixes_maximum", 120, 3600, some 60, none),
  ("authored_pair_within_host", 120, 3600, some 300, some 1800),
  ("authored_default_above_host_cap_clamped", 120, 120, some 600, none),
  ("authored_maximum_above_host_cap_clamped", 120, 600, some 60, some 3600),
  ("authored_maximum_only", 120, 3600, none, some 1800),
  ("host_default_narrowed_by_authored_maximum", 600, 600, none, some 300),
  ("authored_maximum_below_documented_default_rejected", 120, 3600, none, some 60),
  ("zero_default_rejected", 120, 120, some 0, none),
  ("maximum_below_default_rejected", 120, 3600, some 300, some 200)]

structure ForegroundInput where
  name : String
  hostDefault : Nat
  hostMaximum : Nat
  foreground : Authored
  deriving ToJson

@[conformance_family]
def foregroundPair : Family ForegroundInput (Option Pair) where
  inputs := foregroundCases.map fun (name, hostDefault, hostMaximum, execution, maximum) =>
    { name, hostDefault, hostMaximum
      foreground := { timeoutSecs := execution, maxTimeoutSecs := maximum } }
  model := fun i => pair? (effectiveBashForeground ⟨i.hostDefault, i.hostMaximum⟩
    ⟨i.foreground.timeoutSecs, i.foreground.maxTimeoutSecs, none, none, none⟩)

private def cliCases : List (String × Nat × Nat × Nat × Option Int) := [
  ("unauthored_keeps_registration", 120, 120, 10, none),
  ("unauthored_keeps_registration_above_host_cap", 120, 120, 600, none),
  ("authored_raises_within_host_cap", 120, 3600, 10, some 300),
  ("authored_clamped_to_host_cap", 120, 120, 10, some 900),
  ("zero_rejected", 120, 120, 10, some 0)]

structure CliInput where
  name : String
  hostDefault : Nat
  hostMaximum : Nat
  registrationSecs : Nat
  timeoutSecs : Option Int
  deriving ToJson

structure CliOutput where
  cli : Option Nat
  deriving ToJson

@[conformance_family]
def cliTimeout : Family CliInput CliOutput where
  inputs := cliCases.map fun (name, hostDefault, hostMaximum, registrationSecs, timeoutSecs) =>
    { name, hostDefault, hostMaximum, registrationSecs, timeoutSecs }
  model := fun i =>
    { cli := effectiveCliTimeout ⟨i.hostDefault, i.hostMaximum⟩ i.registrationSecs i.timeoutSecs }

private def backgroundCases : List (String × Option Int) := [
  ("unconfigured_keeps_ceiling", none),
  ("authored_shortens_lifetime", some 60),
  ("authored_above_ceiling_clamped", some 86400),
  ("zero_rejected", some 0),
  ("negative_rejected", some (-5))]

structure BackgroundInput where
  name : String
  backgroundTimeoutSecs : Option Int
  deriving ToJson

structure BackgroundOutput where
  background : Option Nat
  deriving ToJson

@[conformance_family]
def backgroundLifetime : Family BackgroundInput BackgroundOutput where
  inputs := backgroundCases.map fun (name, backgroundTimeoutSecs) => { name, backgroundTimeoutSecs }
  model := fun i => { background := effectiveBackgroundLifetime i.backgroundTimeoutSecs }

private def requests : List (Option Nat) := [none, some 0, some 1, some 45, some 5000]

structure Request where
  requested : Option Nat
  effective : Option Nat
  deriving ToJson

structure BoundedOutput where
  expected : Option Pair
  requests : List Request
  deriving ToJson

private def bounded (policy : Option (Nat × Nat)) (effective : Nat × Nat → Option Nat → Nat)
    (requested : List (Option Nat)) : BoundedOutput :=
  { expected := pair? policy
    requests := requested.map fun r => { requested := r, effective := policy.map (effective · r) } }

structure WaitInput where
  name : String
  wait : Authored
  requests : List (Option Nat)
  deriving ToJson

private def waitCases : List (String × Option Int × Option Int) := [
  ("unconfigured_keeps_fixed_defaults", none, none),
  ("authored_pair", some 5, some 20),
  ("authored_default_keeps_documented_maximum", some 60, none),
  ("authored_maximum_above_ceiling_clamped", none, some 1200),
  ("authored_pair_above_ceiling_clamped", some 900, some 1200),
  ("zero_rejected", some 0, none),
  ("maximum_below_documented_default_rejected", none, some 5)]

@[conformance_family]
def waitPolicy : Family WaitInput BoundedOutput where
  inputs := waitCases.map fun (name, wait, maximum) =>
    { name, wait := { timeoutSecs := wait, maxTimeoutSecs := maximum }, requests }
  model := fun i => bounded
    ((resolvedRemoteWait i.wait.timeoutSecs i.wait.maxTimeoutSecs).map (underCeiling waitCeiling))
    waitFor i.requests

structure LspInput where
  name : String
  lsp : Authored
  requests : List (Option Nat)
  deriving ToJson

private def lspCases : List (String × Option Int × Option Int) := [
  ("unconfigured_keeps_fixed_defaults", none, none),
  ("authored_pair_below_request_floor", some 2, some 3),
  ("authored_default_keeps_documented_maximum", some 60, none),
  ("authored_maximum_above_ceiling_clamped", none, some 500),
  ("zero_rejected", some 0, none),
  ("maximum_below_documented_default_rejected", none, some 5)]

@[conformance_family]
def lspPolicy : Family LspInput BoundedOutput where
  inputs := lspCases.map fun (name, timeout, maximum) =>
    { name, lsp := { timeoutSecs := timeout, maxTimeoutSecs := maximum }, requests }
  model := fun i => bounded
    ((resolvedLspTimeout i.lsp.timeoutSecs i.lsp.maxTimeoutSecs).map (underCeiling lspActionCeiling))
    lspActionFor i.requests

example : effectiveBashForeground ⟨120, 3600⟩ ⟨some 60, none, none, none, none⟩ =
    some (60, 60) := by decide
example : effectiveBashForeground ⟨120, 120⟩ ⟨some 600, none, none, none, none⟩ =
    some (120, 120) := by decide
example : effectiveCliTimeout ⟨120, 120⟩ 10 (some 900) = some 120 := by decide
example : (resolvedRemoteWait (some 900) (some 1200)).map (underCeiling waitCeiling) =
    some (600, 600) := by decide

end Conformance.ToolTimeouts
