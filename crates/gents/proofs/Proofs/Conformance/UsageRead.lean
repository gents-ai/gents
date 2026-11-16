import Proofs.Inference.UsageRead
import Proofs.Conformance.Family

namespace UsageRead.Contracts

open Lean InferenceCall.LimitObservation

deriving instance ToJson for Provider, BackendKind, Response, Outcome, Trigger, Account, Backend

structure Pass where
  trigger : Trigger
  second : Nat
  response : Response
  deriving ToJson

structure Input where
  name : String
  accounts : List Account
  backends : List Backend
  filter : Option Provider
  passes : List Pass
  deriving ToJson

structure Seen where
  windows : Bool
  error : Option String
  deriving ToJson

/-- `usage` is the stored observation shown with the entry; a disabled account
shows none. -/
structure Entry where
  account : Option Nat
  backend : Option Nat
  outcome : Outcome
  reason : Option String
  usage : Option Seen
  deriving ToJson

structure PassSeen where
  requests : Nat
  entries : List Entry
  deriving ToJson

abbrev Stores := List (Subject × Stored)

def runPass (input : Input) (stores : Stores) (pass : Pass) : Stores × PassSeen :=
  let results := (plan input.accounts input.backends input.filter).map fun job =>
    (job, read input.backends job pass.trigger pass.second pass.response
      ((stores.lookup job.subject).getD unread))
  ( results.map fun (job, result) => (job.subject, result.stored)
  , { requests := (results.filter fun (_, result) => result.contacted).length
      entries := results.map fun (job, result) =>
        { account := match job.subject with | .account index => some index | _ => none
          backend := match job.subject with | .backend index => some index | _ => none
          outcome := result.outcome
          reason := result.reason
          usage :=
            if job.live then some { windows := result.stored.windows, error := result.stored.error }
            else none } } )

def run (input : Input) : List PassSeen :=
  (input.passes.foldl (fun (stores, seen) pass =>
    let (next, entry) := runPass input stores pass
    (next, seen ++ [entry])) (([] : Stores), ([] : List PassSeen))).2

def grok (slot : Option String := none) (enabled : Bool := true) : Account :=
  { provider := .grok, slot, enabled }

def claude (slot : Option String := none) (enabled : Bool := true) : Account :=
  { provider := .claude, slot, enabled }

def serves (slot : Option String := none) (enabled : Bool := true)
    (provider : Provider := .grok) : Backend :=
  { kind := .account, provider, slot, enabled }

def keyless (kind : BackendKind) (enabled : Bool := true) : Backend :=
  { kind, provider := .grok, slot := none, enabled }

def once : List Pass := [{ trigger := .open, second := 0, response := .windows }]

def plans : List Input :=
  [ { name := "a disabled account is not contacted"
      accounts := [grok, grok (some "acct-g2") false]
      backends := [serves, serves (some "acct-g2")], filter := none, passes := once }
  , { name := "two backends on one account read it once"
      accounts := [grok], backends := [serves, serves], filter := none, passes := once }
  , { name := "a disabled backend does not hide its account"
      accounts := [grok, grok (some "acct-g2")]
      backends := [serves none false, serves, serves (some "acct-g2") false]
      filter := none, passes := once }
  , { name := "an account no backend names is read"
      accounts := [grok, grok (some "acct-g2")], backends := [serves], filter := none
      passes := once }
  , { name := "a backend naming an account the node does not hold reads nothing"
      accounts := [grok]
      backends := [serves, serves (some "acct-other"), keyless .unreported]
      filter := none, passes := once }
  , { name := "a read of one provider's accounts leaves out the rest"
      accounts := [grok, claude, grok (some "acct-g2") false]
      backends := [serves, keyless .keyed, keyless .unreported]
      filter := some .grok, passes := once }
  , { name := "a read of a provider with no account contacts no one"
      accounts := [grok], backends := [serves, keyless .keyed], filter := some .claude
      passes := once }
  , { name := "a node with nothing to read"
      accounts := [], backends := [], filter := none, passes := once }
  , { name := "a pool is read through its accounts"
      accounts := [claude, claude (some "acct-c2"), claude (some "acct-c3") false]
      backends := [{ kind := .pool, provider := .claude, slot := none, enabled := true }]
      filter := none, passes := once }
  , { name := "account-free backends are settled by what they can report"
      accounts := []
      backends :=
        [ keyless .keyed, keyless .unkeyed, keyless .keyed false, keyless .unreported
        , keyless .unreported false, keyless .unkeyed false ]
      filter := none, passes := once } ]

def failures : List Response := [.throttled, .malformed, .unauthorized, .failed, .unreachable]

def outcomes : List Input :=
  (failures.map fun response =>
    { name := s!"a failed read keeps stored windows and counts as a read: {response.error.getD ""}"
      accounts := [grok, claude], backends := [keyless .keyed], filter := none
      passes :=
        [ { trigger := .refresh, second := 0, response := .windows }
        , { trigger := .refresh, second := 10, response }
        , { trigger := .open, second := 20, response := .windows }
        , { trigger := .open, second := 310, response := .windows } ] })
  ++ failures.map fun response =>
    { name := s!"a first read that fails invents no usage: {response.error.getD ""}"
      accounts := [grok], backends := [keyless .keyed], filter := none
      passes := [{ trigger := .open, second := 0, response }] }

def triggers : List Input :=
  [ { name := "a cached read contacts no one"
      accounts := [grok], backends := [keyless .keyed], filter := none
      passes := [{ trigger := .cached, second := 0, response := .windows }] }
  , { name := "an automatic read waits out the window and a refresh does not"
      accounts := [grok, claude], backends := [keyless .keyed], filter := none
      passes :=
        [ { trigger := .open, second := 0, response := .windows }
        , { trigger := .open, second := 299, response := .windows }
        , { trigger := .cached, second := 299, response := .windows }
        , { trigger := .refresh, second := 299, response := .windows }
        , { trigger := .open, second := 598, response := .windows }
        , { trigger := .open, second := 599, response := .windows } ] } ]

@[conformance_family]
def passes : Conformance.Family Input (List PassSeen) where
  inputs := plans ++ outcomes ++ triggers
  model := run

end UsageRead.Contracts
