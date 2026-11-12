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

/-- What a mount holds relative to operator state: nothing guarded, a guarded
object outside it, a reserved name inside it (existing or not), or a guarded
object that cannot be inspected. -/
inductive Guard where
  | unguarded
  | outside
  | namedWithin
  | uninspectable
  deriving DecidableEq

/-- A file inside a mount: an ordinary file, or one sharing an object with
operator state. -/
inductive MountEntry where
  | ordinary
  | sharesOperatorObject
  deriving DecidableEq

inductive MountVerdict where
  | clear
  | reserved
  | unreadable
  deriving DecidableEq

/-- A mount that cannot be read fails closed: a missing mount, or an operator
object that cannot be inspected, is refused rather than taken as unrelated. A
reserved name inside the mount protects it without reading its entries. -/
def mountVerdict (rootExists : Bool) (guard : Guard) (entries : List MountEntry) : MountVerdict :=
  if !rootExists then .unreadable
  else match guard with
    | .unguarded => .clear
    | .namedWithin => .reserved
    | .uninspectable => .unreadable
    | .outside => if entries.any (· == .sharesOperatorObject) then .reserved else .clear

theorem a_missing_mount_fails_closed (guard : Guard) (entries : List MountEntry) :
    mountVerdict false guard entries = .unreadable := by
  cases guard <;> simp [mountVerdict]

theorem an_uninspectable_guard_fails_closed (entries : List MountEntry) :
    mountVerdict true .uninspectable entries = .unreadable := by
  simp [mountVerdict]

theorem a_mount_rejects_nested_operator_aliases_and_reserved_names :
    mountVerdict true .outside [.ordinary] = .clear
      ∧ mountVerdict true .outside [.ordinary, .sharesOperatorObject] = .reserved
      ∧ mountVerdict true .namedWithin [] = .reserved := by
  decide

theorem a_mount_fails_when_operator_objects_cannot_be_inspected :
    mountVerdict true .uninspectable [] = .unreadable
      ∧ mountVerdict false .unguarded [] = .unreadable := by
  decide

inductive ProtectionStep where
  | capture
  | revalidate
  | addAlias
  | removeAlias
  deriving DecidableEq

inductive ProtectionOutcome where
  | applied
  | allowed
  | refused
  deriving DecidableEq

/-- A capture or a revalidation of the command guard, with the state's
hard-link alias present or not. -/
def checked (aliased : Bool) : ProtectionOutcome :=
  if commandGuardAllows true true true (!aliased) then .allowed else .refused

/-- Steps run in order on one operator state: a capture or revalidation sees
the alias the steps before it created or removed. -/
def runProtection (aliased : Bool) : List ProtectionStep → List ProtectionOutcome
  | [] => []
  | .capture :: rest => checked aliased :: runProtection aliased rest
  | .revalidate :: rest => checked aliased :: runProtection aliased rest
  | .addAlias :: rest => .applied :: runProtection true rest
  | .removeAlias :: rest => .applied :: runProtection false rest

theorem command_protection_rejects_existing_and_late_aliases :
    runProtection false [.addAlias, .capture] = [.applied, .refused]
      ∧ runProtection false [.capture, .addAlias, .revalidate] = [.allowed, .applied, .refused] := by
  decide

end ToolPolicy.OperatorPaths
