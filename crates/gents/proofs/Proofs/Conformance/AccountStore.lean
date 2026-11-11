import Proofs.Inference.AccountStore
import Proofs.Conformance.Family

namespace AccountStore.Contracts

open Lean

deriving instance ToJson for Provider, Outcome, StepResult

inductive StepKind where
  | signIn
  | setLabel
  | setEnabled
  | remove
  | loseBackend
  | renameBackend
  deriving ToJson

structure Seed where
  provider : Provider
  tokenKeys : List String
  key : Option String
  accountId : Option String
  label : Option String
  original : Bool
  enabled : Bool
  token : String
  deriving ToJson

structure Step where
  kind : StepKind
  row : Nat
  provider : Provider
  tokenKeys : List String
  key : Option String
  accountId : Option String
  label : Option String
  enabled : Bool
  token : String
  deriving ToJson

structure Input where
  name : String
  seeds : List Seed
  steps : List Step
  deriving ToJson

/-- `resolves` is whether the row's slot reference resolves to it, and `listed`
whether the account listing shows it. Rows are in the resolver's order. -/
structure RowView where
  id : Nat
  provider : Provider
  key : Option String
  accountId : Option String
  label : Option String
  effectiveLabel : String
  original : Bool
  enabled : Bool
  token : String
  listed : Bool
  resolves : Bool
  backends : List String
  deriving ToJson

structure Output where
  results : List StepResult
  rows : List RowView
  backendCount : Nat
  deriving ToJson

def Step.action (step : Step) : Action :=
  match step.kind with
  | .signIn => .signIn
      { provider := step.provider, tokenKeys := step.tokenKeys, key := step.key
        accountId := step.accountId, label := step.label, token := step.token }
  | .setLabel => .setLabel step.row (step.label.getD "")
  | .setEnabled => .setEnabled step.row step.enabled
  | .remove => .remove step.row
  | .loseBackend => .loseBackend step.row
  | .renameBackend => .renameBackend step.row (step.label.getD "")

def seeded (seeds : List Seed) : State :=
  { rows := seeds.zipIdx.map fun (seed, id) =>
      { id, provider := seed.provider, tokenKeys := seed.tokenKeys, key := seed.key
        accountId := seed.accountId, label := seed.label, original := seed.original
        enabled := seed.enabled, token := seed.token }
    backends := []
    next := seeds.length }

def view (state : State) (row : Row) : RowView :=
  { id := row.id, provider := row.provider, key := row.key, accountId := row.accountId
    label := row.label, effectiveLabel := row.effectiveLabel, original := row.original
    enabled := row.enabled, token := row.token, listed := row.provider.account
    resolves := row.enabled
    backends := (state.backends.filter fun backend => backend.row == row.id).map (·.name) }

def blank (kind : StepKind) (row : Nat) : Step :=
  { kind, row, provider := .claude, tokenKeys := [], key := none, accountId := none
    label := none, enabled := true, token := "" }

def signIn (provider : Provider) (tokenKeys : List String) (key accountId : Option String)
    (token : String) (label : Option String := none) : Step :=
  { blank .signIn 0 with provider, tokenKeys, key, accountId, label, token }

/-- One person's sign-in as each product's tokens show it: a ChatGPT member id
under the shared workspace, a Claude organization and account, a Grok
principal. -/
def person (provider : Provider) (who token : String) (label : Option String := none) : Step :=
  match provider with
  | .chatGpt => signIn provider [s!"member-{who}"] (some s!"member-{who}") (some "acct-ws") token label
  | .grok =>
    signIn provider [s!"user:principal-{who}"] (some s!"user:principal-{who}") none token label
  | _ => signIn provider [] (some s!"org-1:account-{who}") (some s!"label-{who}") token label

def claude (account label token : String) : Step :=
  signIn .claude [] (some s!"org-1:{account}") (some label) token

def grok (principal token : String) : Step :=
  signIn .grok [s!"user:{principal}"] (some s!"user:{principal}") none token

def chatGpt (keys : List String) (token : String) : Step :=
  signIn .chatGpt keys keys.head? (some "acct-ws") token

def setLabel (row : Nat) (label : String) : Step :=
  { blank .setLabel row with label := some label }

def setEnabled (row : Nat) (enabled : Bool) : Step :=
  { blank .setEnabled row with enabled }

def renameBackend (row : Nat) (name : String) : Step :=
  { blank .renameBackend row with label := some name }

def seed (provider : Provider) (tokenKeys : List String) (key accountId : Option String)
    (original : Bool := true) : Seed :=
  { provider, tokenKeys, key, accountId, label := none, original, enabled := true
    token := "seeded" }

def products : List Provider := [.chatGpt, .claude, .grok]

def perProduct : List Input := products.flatMap fun p =>
  let scenario (name : String) (steps : List Step) : Input :=
    { name := s!"{p.name}: {name}", seeds := [], steps }
  [ scenario "a first sign-in is the original account" [person p "a" "1"]
  , scenario "the same account refreshes in place and is enabled again"
      [person p "a" "1", setEnabled 0 false, person p "a" "2"]
  , scenario "new accounts take the next free name"
      [ person p "a" "1", person p "b" "1", person p "c" "1"
      , person p "b" "2" (some "Work"), person p "d" "1" ]
  , scenario "a label names an account and a refresh renames it"
      [person p "a" "1" (some "Work"), person p "a" "2", person p "a" "3" (some "Personal")]
  , scenario "an added account's backend follows its label"
      [person p "a" "1", person p "b" "1", person p "b" "2", person p "b" "3" (some "Personal")]
  , scenario "a lost backend comes back at the next sign-in"
      [ person p "a" "1", person p "b" "1", blank .loseBackend 1, person p "b" "2"
      , person p "b" "3" ]
  , scenario "a backend someone renamed keeps its name"
      [ person p "a" "1", person p "b" "1", setLabel 1 " Work ", renameBackend 1 "Custom"
      , setLabel 1 "Home" ]
  , scenario "the next sign-in after the only account is removed is the original"
      [person p "a" "1", blank .remove 0, person p "b" "1", person p "c" "1"]
  , scenario "a removed account is gone and cannot be removed again"
      [person p "a" "1", person p "b" "1", blank .remove 1, blank .remove 1, setEnabled 1 false]
  , scenario "a disabled account keeps its tokens"
      [person p "a" "1", setEnabled 0 false, setEnabled 0 true] ]

def longLabel (length : Nat) : String := String.mk (List.replicate length 'x')

def labels : List Input :=
  [ { name := "a label is unique within its provider only"
      seeds := []
      steps :=
        [ person .claude "a" "1" (some "Work"), person .claude "b" "1" (some "Work")
        , person .claude "b" "1" (some " Work "), person .grok "a" "1" (some "Work")
        , person .claude "a" "2" (some "Work") ] }
  , { name := "a sign-in with an unusable label stores nothing"
      seeds := []
      steps :=
        ["", "   ", longLabel 65, "Work\nmore", "tab\there", "bell\x07"].map (fun label =>
          person .chatGpt "a" "1" (some label))
        ++ [person .chatGpt "a" "1" (some (longLabel 64))] }
  , { name := "a label change is validated and unique"
      seeds := []
      steps :=
        [ person .claude "a" "1", person .claude "b" "1", setLabel 1 "Claude", setLabel 1 ""
        , setLabel 1 "a\x07b", setLabel 1 "Claude 2", setLabel 0 "Claude 2", setLabel 0 "Home"
        , setLabel 1 "Claude", setLabel 7 "Nobody" ] } ]

def identities : List Input :=
  [ { name := "a ChatGPT account matches across key forms and keeps its stored key"
      seeds := []
      steps :=
        [chatGpt ["acct-ws:user-a"] "1", chatGpt ["member-a", "acct-ws:user-a"] "2"] }
  , { name := "a stored key form the sign-in does not show is rewritten"
      seeds := []
      steps :=
        [chatGpt ["member-a", "acct-ws:user-a"] "1", chatGpt ["acct-ws:user-a"] "2"] }
  , { name := "a keyless row with the same account id is filled"
      seeds := [seed .claude [] none (some "label-a")]
      steps := [claude "account-a" "label-a" "1"] }
  , { name := "a keyless row with another account id is another account"
      seeds := [seed .claude [] none (some "label-a")]
      steps := [claude "account-b" "label-b" "1"] }
  , { name := "a row with no identity is adopted"
      seeds := [seed .chatGpt [] none none]
      steps := [person .chatGpt "a" "1"] }
  , { name := "a keyless sign-in is added beside a keyed row"
      seeds := []
      steps := [claude "account-a" "label-a" "1", signIn .claude [] none (some "label-a") "2"] }
  , { name := "a Grok key stored beside other tokens is not trusted"
      seeds := [seed .grok ["user:principal-2"] (some "user:principal-1") none]
      steps := [grok "principal-2" "1", grok "principal-1" "2"] }
  , { name := "a ChatGPT key stored beside other tokens is not trusted"
      seeds := [seed .chatGpt ["member-b"] (some "member-a") (some "acct-ws")]
      steps := [person .chatGpt "b" "1", person .chatGpt "a" "2"] }
  , { name := "the same key under another account id is another account"
      seeds := []
      steps :=
        [ claude "account-1" "label-b" "1", claude "account-1" "label-a" "2"
        , claude "account-2" "label-b" "3" ] }
  , { name := "a sign-in refreshes the row that stores its key"
      seeds := []
      steps :=
        [ claude "account-1" "label-1" "1", claude "account-2" "label-2" "2"
        , claude "account-2" "label-2" "3" ] }
  , { name := "a padded sign-in key matches and the stored key stays"
      seeds := []
      steps := [grok "principal-1" "1", grok "principal-1 " "2"] }
  , { name := "a padded stored key matches and stays"
      seeds := [seed .claude [] (some " org-1:account-1 ") (some " label-a ")]
      steps := [claude "account-1" "label-a" "1"] }
  , { name := "the earliest of two rows sharing a key is refreshed"
      seeds :=
        [ seed .chatGpt ["member-a"] none (some "acct-ws")
        , seed .chatGpt ["acct-ws:user-a"] none (some "acct-ws") false ]
      steps := [chatGpt ["member-a", "acct-ws:user-a"] "1"] }
  , { name := "a later row sharing a key wins over an earlier row with no identity"
      seeds :=
        [ seed .grok [] none none
        , seed .grok ["user:principal-a"] (some "user:principal-a") none false ]
      steps := [person .grok "a" "1", person .grok "b" "2", person .grok "c" "3"] }
  , { name := "a refreshed row keeps its place before later rows"
      seeds :=
        [ seed .claude [] none (some "label-a")
        , seed .claude [] (some "org-1:account-z") (some "label-z") false ]
      steps := [claude "account-a" "label-a" "1"] }
  , { name := "a disabled seeded account that signs in is enabled"
      seeds := [{ seed .grok ["user:principal-a"] (some "user:principal-a") none with
                  enabled := false, label := some "Mine" }]
      steps := [person .grok "a" "1"] } ]

def credentials : List Input :=
  [ { name := "a cloud workspace token is not an account"
      seeds := [seed .cloud [] none none]
      steps :=
        [ signIn .cloud [] none none "1", setLabel 0 "Work", setEnabled 0 false
        , blank .remove 0 ] }
  , { name := "each provider has its own original slot"
      seeds := [seed .cloud [] none none]
      steps := products.map fun p => person p "a" "1" } ]

@[conformance_family]
def scenarios : Conformance.Family Input Output where
  inputs := perProduct ++ labels ++ identities ++ credentials
  model := fun input =>
    let (state, results) := run (seeded input.seeds) (input.steps.map Step.action)
    { results, rows := state.rows.map (view state), backendCount := state.backends.length }

end AccountStore.Contracts
