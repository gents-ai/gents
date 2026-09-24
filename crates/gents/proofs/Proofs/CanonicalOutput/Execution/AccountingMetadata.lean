import Proofs.CanonicalOutput.Execution.ToolMapFrame

namespace CanonicalOutput.Execution

theorem ownedToolByDocument_map (world : World) (edit : OwnedTool → OwnedTool)
    (document : DocId) (hdocument : ∀ tool, (edit tool).document = tool.document) :
    ownedToolByDocument? { world with toolContexts := world.toolContexts.map edit } document =
      (ownedToolByDocument? world document).map edit := by
  unfold ownedToolByDocument?
  change (match (world.toolContexts.map edit).filter
      (fun tool : OwnedTool => tool.document == document) with
    | [tool] => some tool | _ => none) =
    (match world.toolContexts.filter (fun tool : OwnedTool => tool.document == document) with
    | [tool] => some tool | _ => none).map edit
  rw [filter_map_key world.toolContexts (·.document) document edit hdocument]
  cases world.toolContexts.filter (fun tool => tool.document == document) with
  | nil => rfl
  | cons head tail => cases tail <;> rfl

/-- Immutable accepted-header ownership survives a pointwise tool edit.  The
edit may change execution accounting fields, but not physical/provenance
identity or the two spawned-background configuration fields read by the
header projection. -/
theorem acceptedHeaderBindsTool_map
    (world : World) (edit : OwnedTool → OwnedTool) (tool : OwnedTool)
    (hdocument : ∀ value, (edit value).document = value.document)
    (hrequest : ∀ value, (edit value).requestDoc = value.requestDoc)
    (hsession : ∀ value, (edit value).session = value.session)
    (hsequence : ∀ value, (edit value).acceptedSequence = value.acceptedSequence)
    (hprovenance : ∀ value, (edit value).provenance = value.provenance)
    (hawait : ∀ value, (edit value).context.awaitMode = value.context.awaitMode)
    (hchild : ∀ value, (edit value).context.childRequestId = value.context.childRequestId) :
    acceptedHeaderBindsTool
      { world with toolContexts := world.toolContexts.map edit } (edit tool) =
      acceptedHeaderBindsTool world tool := by
  have hlookup (document : DocId) := ownedToolByDocument_map world edit document hdocument
  unfold acceptedHeaderBindsTool spawnParentIntentValid
  rw [hprovenance]
  cases hp : tool.provenance with
  | acceptedIntent =>
      simp only [directAcceptedHeaderMetadataBindsTool, hdocument, hrequest, hsession,
        hsequence]
  | spawnedBackground parent =>
      simp only [hlookup, hdocument, hrequest, hsession, hsequence, hprovenance,
        hawait, hchild]
      cases hl : ownedToolByDocument? world parent <;> simp [hl, hrequest, hsession,
        hsequence, hprovenance, directAcceptedHeaderMetadataBindsTool, hdocument]

end CanonicalOutput.Execution
