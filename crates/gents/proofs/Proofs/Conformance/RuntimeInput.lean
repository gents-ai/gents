import Proofs.ClientShell.RuntimeInput
import Lean

namespace Conformance.RuntimeInput
open Lean

def cases : List Json :=
  [ ("first", true, some "doc", "authored:doc:output-obligation:0"),
    ("later", true, some "doc", "authored:doc:output-obligation:12"),
    ("maximum", true, some "doc", "authored:doc:output-obligation:4294967295"),
    ("overflow", true, some "doc", "authored:doc:output-obligation:4294967296"),
    ("foreign_request", true, some "other", "authored:doc:output-obligation:0"),
    ("missing_request", true, none, "authored:doc:output-obligation:0"),
    ("empty_request", true, some "", "authored::output-obligation:0"),
    ("assistant", false, some "doc", "authored:doc:output-obligation:0"),
    ("leading_zero", true, some "doc", "authored:doc:output-obligation:01"),
    ("extra_suffix", true, some "doc", "authored:doc:output-obligation:1:extra"),
    ("prompt", true, some "doc", "authored:doc:prompt"),
    ("unicode_scope", true, some "é-doc", "authored:é-doc:output-obligation:1") ].map
    fun (name, user, request, key) => Json.mkObj
      [("name", toJson name), ("operation", toJson "output_obligation_control"),
       ("user", toJson user), ("request", toJson request), ("key", toJson key),
       ("control", toJson (ClientShell.RuntimeInput.outputObligation user request key))]

end Conformance.RuntimeInput
