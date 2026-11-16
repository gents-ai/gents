import Proofs.Store.Identity.SessionChild
import Proofs.Conformance.Family

namespace Conformance.BlockScope

open Lean Identity.SessionChild

structure ScopeInput where
  firstSession : String
  secondSession : String
  firstDigest : String
  secondDigest : String
  deriving ToJson

@[conformance_family]
def storageIdentity : Family ScopeInput Bool where
  inputs :=
    [{ firstSession := "session-a", secondSession := "session-a",
       firstDigest := "digest-a", secondDigest := "digest-a" },
     { firstSession := "session-a", secondSession := "session-b",
       firstDigest := "digest-a", secondDigest := "digest-a" },
     { firstSession := "session-a", secondSession := "session-a",
       firstDigest := "digest-a", secondDigest := "digest-b" },
     { firstSession := "a:b", secondSession := "a",
       firstDigest := "c", secondDigest := "b:c" }]
  model := fun i =>
    blockStorageIdentity i.firstSession i.firstDigest ==
      blockStorageIdentity i.secondSession i.secondDigest

end Conformance.BlockScope
