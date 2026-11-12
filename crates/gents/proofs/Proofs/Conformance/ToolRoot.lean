import Proofs.Tools.ToolRoot
import Proofs.Conformance.Family

namespace Conformance.ToolRoot

open Lean _root_.ToolRoot

structure Input where
  staticRoot : List String
  staticBase : Option (List String)
  cwd : Option (List String)
  root : Option (List String)
  absolute : Bool
  target : List String
  deriving ToJson

structure Output where
  resolved : Option (List String)
  display : Option String
  deriving ToJson

@[conformance_family]
def resolution : Family Input Output where
  inputs :=
    [ ⟨["root"], some ["root"], some ["root", "repo"], none, false, ["README.md"]⟩
    , ⟨["baked"], none, some ["placement"], some ["placement"], false, ["notes.txt"]⟩
    , ⟨["baked"], none, some ["placement"], some ["placement"], true, ["baked", "secret.txt"]⟩
    , ⟨["root"], some ["root", "repo"], none, none, false, ["README.md"]⟩
    , ⟨["root"], some ["outside"], none, none, false, ["README.md"]⟩
    , ⟨["root"], some ["root", "repo"], some ["outside"], none, false, ["README.md"]⟩
    , ⟨["baked"], some ["baked"], none, some ["placement"], false, ["README.md"]⟩
    , ⟨["root"], some ["root", "repo"], none, none, true, ["root", "notes.txt"]⟩
    , ⟨["root"], none, none, none, true, ["outside", "notes.txt"]⟩
    , ⟨["root"], none, some ["root", "other"], some ["root", "repo"], false, ["README.md"]⟩ ]
  model := fun i =>
    let overlay : Overlay := ⟨i.cwd, i.root⟩
    let resolved := resolve i.staticRoot i.staticBase overlay ⟨i.absolute, i.target⟩
    { resolved
      display := resolved.bind (display i.staticRoot i.staticBase overlay) }

end Conformance.ToolRoot
