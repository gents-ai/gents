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

/-- A selected workspace permission policy requires command isolation. Kernel
pathname exclusions alone do not protect pre-existing hard-link aliases: the
native owner must inspect open regular files and establish a single link, deny
future access/link creation under reserved namespaces, and protect their directory
ancestors against moves. Cooperating writers publish only through the reserved
staging namespace. Host namespace changes after the final inspection are external
to this dispatch contract. An unconfigured host retains its configured ceiling. -/
def commandGuardAllows (required supported inspected singleLinked : Bool) : Bool :=
  !required || (supported && inspected && singleLinked)

theorem required_guard_cannot_fall_back (inspected linked : Bool) :
    commandGuardAllows true false inspected linked = false := by
  simp [commandGuardAllows]

theorem unknown_objects_cannot_dispatch (supported linked : Bool) :
    commandGuardAllows true supported false linked = false := by
  simp [commandGuardAllows]

theorem aliases_cannot_dispatch (supported inspected : Bool) :
    commandGuardAllows true supported inspected false = false := by
  simp [commandGuardAllows]

theorem an_unconfigured_host_preserves_its_ceiling (supported inspected linked : Bool) :
    commandGuardAllows false supported inspected linked = true := by
  simp [commandGuardAllows]

end ToolPolicy.OperatorPaths
