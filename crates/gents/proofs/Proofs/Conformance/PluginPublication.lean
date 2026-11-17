import Proofs.Tools.ToolPolicy.PluginPublication
import Proofs.Runtime.RuntimeReconcile.SurfacePublication
import Proofs.Conformance.Family

attribute [conformance_function] ToolPolicy.PluginPublication.retry
attribute [conformance_function] RuntimeReconcile.SurfacePublication.keepPrevious
attribute [conformance_function] ToolPolicy.PluginPublication.movePin
attribute [conformance_function] RuntimeReconcile.SurfacePublication.retryProposal
attribute [conformance_function] RuntimeReconcile.SurfacePublication.deferAuthority

namespace Conformance.PluginPublication
open Lean
structure DigestInput where
  pinnedDigests : List Nat
  runningDigest : Nat
  deriving ToJson

@[conformance_family]
def digestAuthorized : Family DigestInput Bool where
  inputs := [[], [0], [1], [0, 1]].flatMap fun pins =>
    [0, 1, 2].map fun digest => ⟨pins, digest⟩
  model := fun i => ToolPolicy.PluginPublication.digestAuthorized i.pinnedDigests i.runningDigest
end Conformance.PluginPublication
attribute [conformance_function] RuntimeReconcile.SurfacePublication.needsPublicationFence
attribute [conformance_function] RuntimeReconcile.SurfacePublication.recreateSlot
attribute [conformance_function] RuntimeReconcile.SurfacePublication.sendRecoveredProposal
