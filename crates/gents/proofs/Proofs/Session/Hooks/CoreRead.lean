import Proofs.Hooks.Staging

namespace Hooks

/-- A declared read: a query over rows, or a named core read.

Decision 2026-11-11, maintainer-approved: a plugin may name a read core
computes, with arguments as data, when the answer is an aggregate of core rows
the plugin should not reconstruct (the session token-usage sum a Task Goal's
baseline records). It is a name in core's registry, not a free query: core
answers it from its own state and the plugin sees only the rows it returns.
Every staging theorem holds for it, since `Host` is generic in the query. -/
inductive Read (Q : Type) where
  | rows (query : Q)
  | core (name : String) (args : String)
  deriving DecidableEq, Repr

/-- Core's named reads: an answer from the name, the arguments and core state. -/
abbrev CoreReads (Core Doc Val : Type) := String → String → Core → List (Row Doc Val)

variable {Core Q Doc Val Op : Type}

def Host.withCoreReads (host : Host Core Q Doc Val Op) (reads : CoreReads Core Doc Val) :
    Host Core (Read Q) Doc Val Op :=
  { select := fun core docs read =>
      match read with
      | .rows query => host.select core docs query
      | .core name args => reads name args core
    perform := host.perform
    allowed := host.allowed }

/-- A named read is answered from core state alone: no plugin row reaches it. -/
theorem core_read_ignores_plugin_rows (host : Host Core Q Doc Val Op)
    (reads : CoreReads Core Doc Val) (core : Core) (docs others : Store Doc Val)
    (name args : String) :
    (host.withCoreReads reads).select core docs (.core name args) =
      (host.withCoreReads reads).select core others (.core name args) := rfl

/-- Row queries are answered as the host answered them before. -/
theorem row_reads_are_the_hosts (host : Host Core Q Doc Val Op)
    (reads : CoreReads Core Doc Val) (core : Core) (docs : Store Doc Val) (query : Q) :
    (host.withCoreReads reads).select core docs (.rows query) = host.select core docs query := rfl

end Hooks
