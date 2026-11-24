-- Lean's own library, for looking at declarations and the assumptions they rest on.
import Lean
-- Every proof file of this project.
import Regolith

/-!
# Audit: every Regolith theorem rests on Lean's standard foundations only

`just lean` runs this file after `lake build`. It walks every declaration
in the `Regolith` namespace and lists the foundational assumptions its
proof depends on. Lean's own three (`propext`, `Classical.choice`,
`Quot.sound`) are allowed. Anything else fails the check: an unfinished
proof shows up here as `sorryAx`, and an assumption added by hand shows up
under its own name.
-/

-- Use Lean's names (Name, CoreM and so on) without writing `Lean.` in front.
open Lean
/-- Fail, naming each offender, unless every `Regolith` declaration depends
only on the allowed assumptions. -/
def auditRegolith : CoreM Unit := do
  -- Lean's standard assumptions: propositional extensionality, choice,
  -- and quotient soundness.
  let allowed : List Name := [``propext, ``Classical.choice, ``Quot.sound]
  -- The environment: every declaration Lean knows at this point.
  let env ← getEnv
  -- Every declaration whose name starts with `Regolith`.
  let names := env.constants.toList.filterMap fun (n, _) =>
    -- Keep a name when it starts with `Regolith`.
    if (`Regolith).isPrefixOf n then some n else none
  -- The offenders found so far, as pairs (declaration, assumption): none yet.
  let mut bad : Array (Name × Name) := #[]
  -- Go through every Regolith declaration,
  for n in names do
    -- and through every assumption its proof depends on.
    for a in ← collectAxioms n do
      -- An assumption that is not one of the three allowed ones
      if !allowed.contains a then
        -- is recorded, with the declaration that rests on it.
        bad := bad.push (n, a)
  -- No offender?
  if bad.isEmpty then
    -- Print how many declarations were checked. `just lean` shows this line.
    IO.println s!"audit: {names.length} Regolith declarations, standard foundations only"
  -- Otherwise:
  else
    -- Fail, naming every offender.
    throwError m!"audit: declarations resting on a disallowed assumption: {bad.toList}"

-- Run the audit when this file is checked.
#eval auditRegolith
