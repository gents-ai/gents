import Proofs.Automation.EventDelivery.Watcher
import Proofs.Automation.EventDelivery.EventSource
import Lean

namespace EventDelivery

inductive SourceKind where
  | watcher | eventSource
  deriving DecidableEq, Repr, Lean.ToJson

def sourceInstance : SourceKind → SourceInstance
  | .watcher => Watcher.watcherSrc
  | .eventSource => EventSource.eventSourceSrc

structure SourceContract where
  name : String
  dedupePolicy : String
  rescanBoundedBy : Nat
  deviation : Option String
  deriving Lean.ToJson

def sourceContract (source : SourceKind) : SourceContract :=
  let src :=  sourceInstance source
  ⟨src.name, src.dedupePolicy.toContract, src.rescanBoundedBy, none⟩

theorem source_rescan_positive (source : SourceKind) :
    0 < (sourceInstance source).rescanBoundedBy := by
  cases source <;> decide

theorem source_dedupe_policy (source : SourceKind) :
    DedupePolicy.fromContract? (sourceContract source).dedupePolicy =
      some (sourceInstance source).dedupePolicy :=
  DedupePolicy.fromContract_toContract _

end EventDelivery
