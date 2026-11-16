/-! Native file tools resolve a requested path against a tool root and a base
directory. A request may overlay its own workspace root and working directory;
for that request the overlay replaces the configured root and base. Paths are
canonical segment lists; every directory named exists. -/

namespace ToolRoot

abbrev Path := List String

def within (root path : Path) : Bool := root.isPrefixOf path

/-- A base outside the root, or no base, is the root itself. -/
def resolveBase (root : Path) (base : Option Path) : Path :=
  match base with
  | some base => if within root base then base else root
  | none => root

structure Overlay where
  cwd : Option Path
  root : Option Path
  deriving DecidableEq, Repr

def effectiveRoot (staticRoot : Path) (overlay : Overlay) : Path :=
  overlay.root.getD staticRoot

/-- A request's working directory, or its root when it names none, replaces
the configured base; the configured base applies only to a request without
either. -/
def effectiveBase (staticRoot : Path) (staticBase : Option Path) (overlay : Overlay) : Path :=
  let root := effectiveRoot staticRoot overlay
  match overlay.cwd.or overlay.root with
  | some cwd => resolveBase root (some cwd)
  | none => resolveBase root (some (resolveBase staticRoot staticBase))

structure Target where
  absolute : Bool
  path : Path
  deriving DecidableEq, Repr

/-- The resolved path, or `none` when it lies outside the effective root. -/
def resolve (staticRoot : Path) (staticBase : Option Path) (overlay : Overlay)
    (target : Target) : Option Path :=
  let candidate :=
    if target.absolute then target.path else effectiveBase staticRoot staticBase overlay ++ target.path
  if within (effectiveRoot staticRoot overlay) candidate then some candidate else none

def stripPrefix (pre path : Path) : Option Path :=
  if pre.isPrefixOf path then some (path.drop pre.length) else none

/-- Relative to the base, else to the root; `none` when the path is shown
whole. -/
def display (staticRoot : Path) (staticBase : Option Path) (overlay : Overlay)
    (path : Path) : Option String :=
  match (stripPrefix (effectiveBase staticRoot staticBase overlay) path).or
      (stripPrefix (effectiveRoot staticRoot overlay) path) with
  | some [] => some "."
  | some rest => some ("/".intercalate rest)
  | none => none

theorem request_runtime_workspace_overrides_static_base (staticRoot cwd : Path)
    (staticBase : Option Path) (inside : within staticRoot cwd = true) :
    effectiveBase staticRoot staticBase ⟨some cwd, none⟩ = cwd := by
  simp [effectiveBase, effectiveRoot, resolveBase, inside]

theorem request_runtime_workspace_root_replaces_static_root (staticRoot root path : Path)
    (staticBase cwd : Option Path) (outside : within root path = false) :
    resolve staticRoot staticBase ⟨cwd, some root⟩ ⟨true, path⟩ = none := by
  simp [resolve, effectiveRoot, outside]

end ToolRoot
