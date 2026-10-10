namespace ToolPolicy.PluginReceipt

/-- Receipt bytes are the host's canonical serialization of native evidence.
The enclosing transaction authenticates the physical tool row and compares the
accepted request generation with its current generation. A terminal without
this evidence cannot acquire it later; retries must retain identical evidence. -/
def mayCommit (generationMatches terminal : Bool)
    (existing : Option String) (receipt : String) : Bool :=
  generationMatches && match existing with
    | none => !terminal
    | some prior => prior == receipt

theorem stale_generation_denied (terminal : Bool) (existing : Option String)
    (receipt : String) : mayCommit false terminal existing receipt = false := by
  simp [mayCommit]

theorem terminal_absence_denied (receipt : String) :
    mayCommit true true none receipt = false := rfl

theorem existing_is_immutable (terminal : Bool) (prior receipt : String)
    (h : mayCommit true terminal (some prior) receipt = true) : prior = receipt := by
  simpa [mayCommit] using h

end ToolPolicy.PluginReceipt
