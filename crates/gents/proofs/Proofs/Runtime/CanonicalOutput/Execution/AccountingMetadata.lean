import Proofs.Runtime.CanonicalOutput.Execution.ToolMapFrame

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

theorem acceptedHeaderBindsTool_map
    (world : World) (edit : OwnedTool → OwnedTool) (tool : OwnedTool)
    (hdocument : ∀ value, (edit value).document = value.document)
    (hrequest : ∀ value, (edit value).requestDoc = value.requestDoc)
    (hsession : ∀ value, (edit value).session = value.session)
    (hsequence : ∀ value, (edit value).acceptedSequence = value.acceptedSequence)
    (hprovenance : ∀ value, (edit value).provenance = value.provenance)
    (hawait : ∀ value, (edit value).context.awaitMode = value.context.awaitMode)
    (harguments : ∀ value, (edit value).effectArguments = value.effectArguments) :
    acceptedHeaderBindsTool
      { world with toolContexts := world.toolContexts.map edit } (edit tool) =
      acceptedHeaderBindsTool world tool := by
  have hlookup (document : DocId) := ownedToolByDocument_map world edit document hdocument
  have hname (value : OwnedTool) :
      invocationName? { world with toolContexts := world.toolContexts.map edit } (edit value) =
        invocationName? world value := by
    simp only [invocationName?, hprovenance, hdocument, harguments]
  have frame (fuel : Nat) (value : OwnedTool) :
      invocationAncestryValid fuel { world with toolContexts := world.toolContexts.map edit }
        (edit value) = invocationAncestryValid fuel world value := by
    induction fuel generalizing value with
    | zero => rfl
    | succ fuel ih =>
      simp only [invocationAncestryValid, invocationAncestryWith]
      have hp : invocationParent? (edit value) = invocationParent? value := by
        simp [invocationParent?, hprovenance]
      rw [hp]
      cases hparent : invocationParent? value with
      | none => simp [directAcceptedHeaderMetadataBindsTool, hdocument, hrequest, hsession, hsequence]
      | some doc =>
        dsimp only
        rw [hlookup]
        cases hl : ownedToolByDocument? world doc with
        | none => rfl
        | some parent =>
          simp only [Option.map_some]
          have link : invocationLinkValid { world with toolContexts := world.toolContexts.map edit }
              (edit value) (edit parent) = invocationLinkValid world value parent := by
            simp only [invocationLinkValid, hdocument, hrequest, hsession, hsequence,
              hprovenance, hawait, hname]
          change (invocationLinkValid { world with toolContexts := world.toolContexts.map edit }
            (edit value) (edit parent) && invocationAncestryValid fuel
              { world with toolContexts := world.toolContexts.map edit } (edit parent)) =
            (invocationLinkValid world value parent && invocationAncestryValid fuel world parent)
          rw [link, ih]
  simpa only [acceptedHeaderBindsTool, List.length_map] using frame (world.toolContexts.length + 1) tool

end CanonicalOutput.Execution
