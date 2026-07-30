import Proofs.ApplyReconcile.Collections

namespace ReversePairingHandlers

abbrev Collection := ApplyReconcile.Collection

structure ReceiverState where
  subscribed : Finset Collection

def applyInstall (s : ReceiverState) (c : Collection) : ReceiverState :=
  { s with subscribed := insert c s.subscribed }

def applyTeardown (s : ReceiverState) (c : Collection) : ReceiverState :=
  { s with subscribed := s.subscribed.erase c }

theorem install_idempotent (s : ReceiverState) (c : Collection) :
    applyInstall (applyInstall s c) c = applyInstall s c := by
  simp [applyInstall]

theorem teardown_idempotent (s : ReceiverState) (c : Collection) :
    applyTeardown (applyTeardown s c) c = applyTeardown s c := by
  simp [applyTeardown]

theorem install_then_teardown (s : ReceiverState) (c : Collection) :
    applyTeardown (applyInstall s c) c = applyTeardown s c := by
  simp [applyInstall, applyTeardown]

theorem teardown_then_install (s : ReceiverState) (c : Collection) :
    applyInstall (applyTeardown s c) c = applyInstall s c := by
  cases s with
  | mk subscribed =>
      simp [applyInstall, applyTeardown]
      ext x
      by_cases h : x = c <;> simp [h]

end ReversePairingHandlers
