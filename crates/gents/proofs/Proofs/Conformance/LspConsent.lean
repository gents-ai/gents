import Proofs.Tools.Lsp.Consent
import Proofs.Conformance.Family

namespace Conformance.LspConsent

open Lean Lsp.Consent

structure Input where
  inside : List Bool
  denyAt : Option Nat
  deriving ToJson

structure Output where
  reviews : Nat
  applied : Option Nat
  written : List Bool
  deriving ToJson

@[conformance_family]
def edit : Family Input Output where
  inputs :=
    [ ⟨[true], none⟩
    , ⟨[true], some 0⟩
    , ⟨[false], none⟩
    , ⟨[false], some 0⟩
    , ⟨[false, false], some 1⟩
    , ⟨[false, false], some 0⟩
    , ⟨[true, false], some 0⟩
    , ⟨[false, true], none⟩ ]
  model := fun i =>
    { reviews := (consent i.inside i.denyAt).reviews
      applied := applied i.inside i.denyAt
      written := written i.inside i.denyAt }

end Conformance.LspConsent
