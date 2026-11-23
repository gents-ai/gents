/-! # What a refused hosted pack install shows its installer

A hosted install runs in the runtime for a client. When it fails, the runtime
sends a fixed class in place of its error chain, and decides here whether the
text it holds for that class may cross the wire beside it. -/

namespace Pack.HostedRefusal

/-- The step of a hosted install or wire that failed, as it crosses the wire.
The first eleven are steps; the last four are typed refusals the runtime holds
as values, not as an error chain. -/
inductive Failure
  | runtimeUnavailable
  | notAuthorized
  | refusedForAPerson
  | invalidRequest
  | preparationFailed
  | previewFailed
  | schemaPreparation
  | schemaApplyFailed
  | applyRejected
  | applyUnconfirmed
  | receiptFailed
  | unsetVariables
  | pluginUnresolved
  | documentsDiffer
  | documentsEdited
  deriving DecidableEq, Repr

/-- Whether the runtime's text for a failure crosses the wire with its class,
when every identifier the received archives author is `literal`.

Only a typed refusal's own text does, written from names the received
archives author: install variable names, a pack, a Tools id, a plugin
coordinate and digests, documents by collection and id. A step's failure is
an error chain, which can quote a resolved install value, a rendered document
field or a GraphQL error echoing a document, so it shows only its class's
fixed description. A variable's name is never rendered; an identifier is
rendered from install values wherever an archive authors it with `${VAR}`, so
an archive authoring any identifier that way discloses only the description.

External premise: the loader renders `${VAR}` in identifier fields from
install values, which can be credentials. -/
def showsRuntimeText (failure : Failure) (literal : Bool) : Bool :=
  match failure with
  | .unsetVariables => true
  | .pluginUnresolved | .documentsDiffer | .documentsEdited => literal
  | .runtimeUnavailable | .notAuthorized | .refusedForAPerson | .invalidRequest
  | .preparationFailed | .previewFailed | .schemaPreparation | .schemaApplyFailed
  | .applyRejected | .applyUnconfirmed | .receiptFailed => false

/-- Whether an edited document is named in a `documentsEdited` refusal rather
than counted. The edited documents can include ones an earlier installation
wrote, under identifiers the received archives cannot vouch for; only one the
received, literal archives ship is named. -/
def namesEditedDocument (literal shipped : Bool) : Bool :=
  literal && shipped

theorem a_failed_step_shows_no_runtime_text (literal : Bool) :
    showsRuntimeText .preparationFailed literal = false ∧
      showsRuntimeText .previewFailed literal = false ∧
      showsRuntimeText .applyRejected literal = false ∧
      showsRuntimeText .applyUnconfirmed literal = false := by
  cases literal <;> exact ⟨rfl, rfl, rfl, rfl⟩

theorem rendered_identifiers_are_never_shown (failure : Failure)
    (h : failure ≠ .unsetVariables) : showsRuntimeText failure false = false := by
  cases failure <;> simp_all [showsRuntimeText]

theorem unset_variables_are_named (literal : Bool) :
    showsRuntimeText .unsetVariables literal = true := rfl

theorem an_edited_document_is_named_only_if_shipped_literally (literal shipped : Bool)
    (h : namesEditedDocument literal shipped = true) : literal = true ∧ shipped = true := by
  cases literal <;> cases shipped <;> simp_all [namesEditedDocument]

end Pack.HostedRefusal
