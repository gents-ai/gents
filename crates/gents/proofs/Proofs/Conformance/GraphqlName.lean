import Proofs.Shared.GraphqlName
import Proofs.Conformance.Family

namespace GraphqlName

open Lean

deriving instance Lean.ToJson for NameOutcome, CollectionOutcome

structure NameInput where
  value : String
  deriving ToJson

structure CollectionInput where
  value : String
  deriving ToJson

structure EscapeInput where
  value : String
  deriving ToJson

@[conformance_family]
def names : Conformance.Family NameInput NameOutcome where
  inputs :=
    [ { value := "AgentResponse" }
    , { value := "_docID" }
    , { value := "_eq" }
    , { value := "snake_case_2" }
    , { value := "a" }
    , { value := "_" }
    , { value := "WebhookEvent" }
    , { value := "" }
    , { value := "1abc" }
    , { value := "Msg (limit: 1) { _docID }" }
    , { value := "a b" }
    , { value := "a-b" }
    , { value := "a.b" }
    , { value := "Msg{" }
    , { value := "a\"b" }
    , { value := "a\nb" }
    , { value := "naïve" }
    , { value := "名前" }
    , { value := String.mk ['a', Char.ofNat 0x200b, 'b'] }
    , { value := " AgentResponse" }
    , { value := "AgentResponse " }
    , { value := "A" }
    , { value := "z" }
    , { value := "_0" }
    , { value := "a0" }
    , { value := "a_b" }
    , { value := "9a" }
    , { value := "-" }
    , { value := " " } ]
  model := fun i => nameOutcome i.value

@[conformance_family]
def collections : Conformance.Family CollectionInput CollectionOutcome where
  inputs :=
    [ { value := "__Type" }
    , { value := "__schema" }
    , { value := "__typename" }
    , { value := "__" }
    , { value := "AgentResponse" }
    , { value := "_Private" }
    , { value := "Msg) { x } (" }
    , { value := "" }
    , { value := "1abc" }
    , { value := "_" }
    , { value := "__1" }
    , { value := "__a b" }
    , { value := " Agent" }
    , { value := "Agent " }
    , { value := "a__b" } ]
  model := fun i => collectionOutcome i.value

@[conformance_family]
def escapes : Conformance.Family EscapeInput String where
  inputs :=
    [ { value := "hello" }
    , { value := "he\"llo" }
    , { value := "line1\nline2" }
    , { value := "cr\r" }
    , { value := "tab\there" }
    , { value := "back\\slash" }
    , { value := "mixed\"\n\\" }
    , { value := "" }
    , { value := "界" }
    , { value := String.mk [Char.ofNat 0] }
    , { value := String.mk ['a', Char.ofNat 1, 'b'] }
    , { value := String.mk [Char.ofNat 31] }
    , { value := String.mk [Char.ofNat 127] } ]
  model := fun i => escape i.value

end GraphqlName
