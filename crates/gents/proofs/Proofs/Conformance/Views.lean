import Proofs.Store.Views
import Proofs.Tools.Views.Mcp
import Proofs.Documents.Views.TaskInventory
import Proofs.Documents.ConfigurationViews
import Proofs.Inference.ActivityViews
import Proofs.Session.Views
import Proofs.Session.TranscriptViews
import Proofs.Session.RequestViews
import Proofs.Conformance.SessionViews
import Proofs.Conformance.InferenceContextViews
import Proofs.Conformance.Family

attribute [conformance_function] ReadViews.runner ReadViews.counted McpViews.inventory
  McpViews.page McpViews.health TaskInventory.selection SessionViews.visible
  ConfigurationViews.inventory InferenceActivityViews.included
  TranscriptViews.visible TranscriptViews.window TranscriptViews.sourceVisible
  RequestViews.observedSeed RequestViews.causalChild

attribute [conformance_vocabulary] InferenceActivityViews.callStateName
