/-! The host services a plugin call is offered and the input its guest sees. -/

namespace ToolPolicy.PluginHostServices

/-- The network grant of an installed plugin. A host list that names no host
grants nothing, as a sealed grant does. -/
inductive NetGrant where
  | sealed
  | noHosts
  | hosts
  deriving DecidableEq, Repr

def httpOffered : NetGrant → Bool
  | .hosts => true
  | _ => false

/-- `data_calls` is offered only to a plugin that declares data, and only when
the caller supplied its data context. -/
def dataOffered (declared callerSupplied : Bool) : Bool :=
  declared && callerSupplied

def requestTimeoutCapMs : Nat := 30000

/-- A granted per-request HTTP timeout is clamped to the host's cap. -/
def requestTimeoutMs (granted : Option Nat) : Nat :=
  min (granted.getD requestTimeoutCapMs) requestTimeoutCapMs

/-- The host HTTP grant a receipt records: the request timeout when HTTP is
offered, none otherwise. -/
def receiptHttpTimeout (net : NetGrant) (granted : Option Nat) : Option Nat :=
  if httpOffered net then some (requestTimeoutMs granted) else none

/-- The guest's input keys, sorted, each marked when the host set it. With no
service offered the caller's input passes unchanged. Otherwise the caller's
`state` is dropped (a non-job call has no stored guest state) and each offered
service replaces any caller `<service>_results` with its `<service>_calls`
flag. -/
def guestInput (callerKeys : List String) (http data : Bool) : List (String × Bool) :=
  if !(http || data) then callerKeys.map (·, false)
  else
    let removed := ["state"] ++ (if http then ["http_results"] else []) ++
      (if data then ["data_results"] else [])
    let added := (if http then ["http_calls"] else []) ++ (if data then ["data_calls"] else [])
    ((callerKeys.filter (· ∉ removed)).map (·, false) ++ added.map (·, true)).mergeSort
      fun a b => a.1 ≤ b.1

theorem data_calls_is_not_offered_to_plugins_without_data (declared callerSupplied : Bool) :
    dataOffered false callerSupplied = false ∧ dataOffered declared false = false := by
  cases declared <;> cases callerSupplied <;> decide

theorem receipt_hashes_host_prepared_input_and_actual_http_grant (granted : Option Nat) :
    (receiptHttpTimeout .hosts granted).all (· ≤ requestTimeoutCapMs) = true ∧
      receiptHttpTimeout .noHosts granted = none ∧
      guestInput ["http_results", "state", "value"] true false =
        [("http_calls", true), ("value", false)] := by
  refine ⟨?_, rfl, by native_decide⟩
  simp [receiptHttpTimeout, httpOffered, requestTimeoutMs, Nat.min_le_right]

end ToolPolicy.PluginHostServices
