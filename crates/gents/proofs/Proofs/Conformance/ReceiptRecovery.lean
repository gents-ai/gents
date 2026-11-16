import Proofs.Store.ReceiptRecovery
import Proofs.Conformance.Family

namespace Conformance.ReceiptRecovery

open Lean _root_.ReceiptRecovery

deriving instance ToJson for Post, Read, Outcome

structure Script where
  posts : List Post
  reads : List Read
  deriving ToJson

structure Observed where
  outcome : Outcome
  posts : Nat
  reads : Nat
  durable : Bool
  deriving ToJson

def observe (script : Script) : Observed :=
  let run := run script.posts script.reads
  { outcome := run.outcome, posts := run.posts, reads := run.reads, durable := run.durable }

@[conformance_family]
def http : Family Script Observed where
  inputs :=
    [ { posts := [], reads := [] }
    , { posts := [.rejected], reads := [] }
    , { posts := [.lostReply], reads := [] }
    , { posts := [.conflict], reads := [] }
    , { posts := [.conflict, .lostReply], reads := [] }
    , { posts := [.conflict, .rejected], reads := [] }
    , { posts := [.lostReply], reads := [.transient] }
    , { posts := [.lostReply], reads := [.transient, .transient, .transient, .transient] }
    , { posts := [.lostReply], reads := List.replicate 5 .transient }
    , { posts := [.lostReply], reads := [.fails] }
    , { posts := [.conflict], reads := [.transient, .answers, .fails] }
    , { posts := [.conflict, .conflict], reads := [.answers, .transient] }
    , { posts := List.replicate 4 .conflict, reads := [] }
    , { posts := List.replicate 5 .conflict, reads := [] }
    , { posts := List.replicate 4 .conflict ++ [.lostReply], reads := [] } ]
  model := observe

end Conformance.ReceiptRecovery
