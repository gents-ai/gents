import Proofs.Clients.ClientShell.SessionShown
import Proofs.Conformance.Family

namespace SessionShown.Contracts

open SessionShown Lean

deriving instance ToJson for Page

structure Input where
  visible : Bool
  wide : Bool
  page : Page
  listed : Bool
  deriving ToJson

private def bools : List Bool := [false, true]

/-- Every visibility, width, page and listing. -/
@[conformance_family]
def seen : Conformance.Family Input Bool where
  inputs := bools.flatMap fun visible => bools.flatMap fun wide =>
    [Page.session, .other].flatMap fun page => bools.map fun listed =>
      { visible, wide, page, listed }
  model := fun i => keepsSeen i.visible i.wide i.page i.listed

end SessionShown.Contracts
