import Proofs.Session.PinnedTokenLimit
import Proofs.Conformance.Family

namespace Request.PinnedTokenLimit

@[conformance_family]
def pinnedLimit : Conformance.Family Input Outcome :=
  ⟨[none, some (-9223372036854775808), some (-1), some 0, some 1,
    some 4096, some 9223372036854775807].map (⟨·⟩), parse⟩

end Request.PinnedTokenLimit
