import Proofs.Shared.RequestEncoding
import Proofs.Conformance.Family

namespace RequestEncoding.Contracts

open RequestEncoding

/-- An endpoint's answers so far, oldest first (`true` accepted gzip request
bodies), then one request body's length in bytes. -/
structure Input where
  name : String
  answers : List Bool
  length : Nat
  deriving Lean.ToJson

private def case (name : String) (answers : List Bool) (length : Nat) : Input :=
  { name, answers, length }

private def cases : List Input :=
  [ case "a_host_not_yet_heard_from_gets_plain_bodies" [] 62_000
  , case "a_host_that_never_accepted_gets_plain_bodies" [false, false] 62_000
  , case "a_host_that_accepted_gets_a_large_body_gzipped" [true] 62_000
  , case "a_small_body_stays_plain" [true] 4_096
  , case "a_body_of_exactly_the_threshold_stays_plain" [true] 8_192
  , case "a_body_one_byte_over_is_gzipped" [true] 8_193
  , case "a_host_that_stopped_accepting_gets_plain_bodies" [true, false] 62_000
  , case "a_host_that_started_accepting_gets_gzip" [false, true] 62_000 ]

@[conformance_family]
def gzip : Conformance.Family Input Bool where
  inputs := cases
  model := fun i => gzipBody (known i.answers) i.length

end RequestEncoding.Contracts
