namespace ToolPolicy.OperatorPaths

/-- Canonical names and live filesystem objects both protect operator state.
Native adapters must obtain object identity from simultaneously open handles;
filesystem identity is an external premise. Failure to inspect an object is
not evidence that it is unrelated. A missing target has no shared object.
Permission writers reserve their staging namespace before creating temporary
files; the protected boundary includes the complete atomic publication path,
not only its final filename. -/
def isProtected (named : Bool) (sameObject : Option Bool) : Bool :=
  named || sameObject.getD true

theorem named_state_is_protected (object : Option Bool) :
    isProtected true object = true := by simp [isProtected]

theorem aliases_are_protected (named : Bool) :
    isProtected named (some true) = true := by simp [isProtected]

theorem unknown_objects_are_protected (named : Bool) :
    isProtected named none = true := by simp [isProtected]

end ToolPolicy.OperatorPaths
