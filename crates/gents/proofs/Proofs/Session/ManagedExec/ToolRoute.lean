/-!
Which native tools run their blocking work in a managed executor, and on which
process boundary. Every routed tool registers its executor under its own tool
name, so liveness, deadline and cancellation reach the process that does the
work.
-/

namespace ManagedExec.ToolRoute

inductive WorkClass
  /-- The out-of-process filesystem runner walks the tree. -/
  | filesystemTraversal
  /-- A command process the tool launches. -/
  | shellCommand
  deriving DecidableEq, Repr

def route : String → Option WorkClass
  | "list_files" | "glob" | "grep" => some .filesystemTraversal
  | "bash" | "bash_unrestricted" => some .shellCommand
  | _ => none

theorem filesystem_tools_never_run_a_shell (tool : String)
    (h : route tool = some .filesystemTraversal) :
    tool = "list_files" ∨ tool = "glob" ∨ tool = "grep" := by
  unfold route at h
  split at h <;> simp_all

end ManagedExec.ToolRoute
