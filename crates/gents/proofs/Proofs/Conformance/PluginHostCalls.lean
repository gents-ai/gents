import Proofs.Tools.ToolPolicy.PluginCalls
import Proofs.Tools.ToolPolicy.PluginHostServices
import Proofs.Conformance.Family

namespace Conformance.PluginHostCalls

open Lean ToolPolicy

deriving instance ToJson for PluginHostServices.NetGrant, PluginCalls.UncoveredBind

/-- An echo plugin called with the caller input `callerKeys` names, its data
declared or not, a data context supplied or not, and a network grant with an
optional per-request timeout. -/
structure PreparedInput where
  dataDeclared : Bool
  dataCaller : Bool
  net : PluginHostServices.NetGrant
  httpTimeoutMs : Option Nat
  deriving ToJson

structure GuestField where
  key : String
  hostSet : Bool
  deriving ToJson

structure PreparedOutput where
  guest : List GuestField
  hostHttpTimeoutMs : Option Nat
  deriving ToJson

def callerKeys : List String := ["data_results", "http_results", "state", "value"]

@[conformance_family]
def prepared : Family PreparedInput PreparedOutput where
  inputs := [false, true].flatMap fun dataDeclared => [false, true].flatMap fun dataCaller =>
    [PluginHostServices.NetGrant.sealed, .noHosts, .hosts].flatMap fun net =>
      [none, some 1000, some 30000, some 60000].map fun httpTimeoutMs =>
        { dataDeclared, dataCaller, net, httpTimeoutMs }
  model := fun i =>
    let http := PluginHostServices.httpOffered i.net
    let data := PluginHostServices.dataOffered i.dataDeclared i.dataCaller
    { guest := (PluginHostServices.guestInput callerKeys http data).map fun (key, hostSet) =>
        { key, hostSet }
      hostHttpTimeoutMs := PluginHostServices.receiptHttpTimeout i.net i.httpTimeoutMs }

/-- A read of a path outside every allowed folder: interactive or headless,
and, when interactive, whether the operator allows it once. -/
structure BindInput where
  interactive : Bool
  operatorAllows : Bool
  deriving ToJson

@[conformance_family]
def bind : Family BindInput PluginCalls.UncoveredBind where
  inputs := [{ interactive := false, operatorAllows := false },
    { interactive := true, operatorAllows := true }, { interactive := true, operatorAllows := false }]
  model := fun i => PluginCalls.uncoveredBind i.interactive i.operatorAllows

end Conformance.PluginHostCalls
