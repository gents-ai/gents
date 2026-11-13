import Lean

namespace Runtime.ReasoningCapabilities
open Lean

structure Input where
  messages : Bool
  params : Option String
  advertised : Option (List String)
  deriving ToJson

/-- Only the admitted provider's advertised efforts may shape Messages
capabilities. User-supplied values cannot claim capabilities the provider did
not advertise; other provider parameter objects remain unchanged. -/
def authoritative (input : Input) : Option String :=
  if !input.messages then input.params.map fun params => ((Json.parse params).toOption.getD Json.null).compress
  else
    let params := ((input.params.bind fun params => (Json.parse params).toOption).getD (Json.mkObj [])).getObj?
    let fields := params.toOption.getD .leaf
    let efforts := match input.advertised with
      | none => Json.null
      | some values => toJson values
    some (Json.obj (fields.insert compare "_gents_advertised_reasoning_efforts" efforts)).compress

end Runtime.ReasoningCapabilities
