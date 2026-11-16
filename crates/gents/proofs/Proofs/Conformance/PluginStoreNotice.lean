import Proofs.Documents.Configuration
import Proofs.Conformance.Family

namespace Conformance.PluginStoreNotice

open Lean Configuration

deriving instance ToJson for PluginNoticePrior

structure Input where
  prior : PluginNoticePrior
  retired : Bool
  deriving ToJson

structure Output where
  recorded : Bool
  listed : Bool
  removed : Bool
  deriving ToJson

/-- A served node builds an absent coordinate and a pack installed without
documents; a plugin-store record needs plugins installed in a home, which the
pack installation suite drives on its own. -/
@[conformance_family]
def record : Family Input Output where
  inputs := [PluginNoticePrior.absent, .pack].flatMap fun prior =>
    [false, true].map fun retired => { prior, retired }
  model := fun i =>
    { recorded := pluginNoticeKeepsRecord i.prior [] i.retired
      listed := pluginNoticeListsPack i.prior [] i.retired
      removed := pluginNoticeListsPack i.prior [] i.retired }

end Conformance.PluginStoreNotice
