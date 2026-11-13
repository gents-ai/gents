/-! Configuration discovery inspects each selected source root through a fixed
allowlist. A root that cannot be inspected is reported, never guessed at; a
file that cannot be used makes its source partial and says why, as a warning
or as a truncation when a limit stopped it. -/

namespace SelfConfig.ConfigurationDiscovery

inductive Root where
  | present
  | missing
  | relative
  | notDirectory
  deriving DecidableEq, Repr

inductive FileCondition where
  | wellFormed
  | malformed
  | oversized
  deriving DecidableEq, Repr

inductive Notice where
  | missingRoot
  | relativeRoot
  | invalidRoot
  | malformedFile
  | fileTooLarge
  deriving DecidableEq, Repr

inductive Outcome where
  | complete
  | «partial»
  | unavailable
  | rejected
  deriving DecidableEq, Repr

/-- A source's settings file, when present, is parsed; its instruction file,
when present, is excerpted unparsed (`true` when it exceeds the per-file
limit). -/
structure Source where
  root : Root
  settings : Option FileCondition
  instructionsOversized : Option Bool
  deriving DecidableEq, Repr

def settingsWarnings : Option FileCondition → List Notice
  | some .malformed => [.malformedFile]
  | _ => []

def settingsTruncations : Option FileCondition → List Notice
  | some .oversized => [.fileTooLarge]
  | _ => []

def warnings (s : Source) : List Notice :=
  match s.root with
  | .present => settingsWarnings s.settings
  | .missing => [.missingRoot]
  | .relative => [.relativeRoot]
  | .notDirectory => [.invalidRoot]

def truncations (s : Source) : List Notice :=
  match s.root with
  | .present =>
      settingsTruncations s.settings ++ (if s.instructionsOversized = some true then [.fileTooLarge] else [])
  | _ => []

def outcome (s : Source) : Outcome :=
  match s.root with
  | .present => if (warnings s).isEmpty && (truncations s).isEmpty then .complete else .«partial»
  | .missing => .unavailable
  | .relative | .notDirectory => .rejected

theorem malformed_missing_and_limits_are_explicit :
    outcome ⟨.present, some .malformed, some true⟩ = .«partial» ∧
    warnings ⟨.present, some .malformed, some true⟩ = [.malformedFile] ∧
    truncations ⟨.present, some .malformed, some true⟩ = [.fileTooLarge] ∧
    outcome ⟨.missing, none, none⟩ = .unavailable ∧
    warnings ⟨.missing, none, none⟩ = [.missingRoot] := by
  decide

end SelfConfig.ConfigurationDiscovery
