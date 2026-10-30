use super::*;

impl ConfigCommandTool {
    pub(super) async fn patch(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        let stringified = request
            .patch
            .iter()
            .find(|(_, value)| {
                value.as_ref().and_then(Value::as_str).is_some_and(|text| {
                    serde_json::from_str::<Value>(text)
                        .is_ok_and(|parsed| parsed.is_object() || parsed.is_array())
                })
            })
            .map(|(field, _)| field.clone());
        let collection = request.target.collection_name();
        self.patch_outcome(core, verb, request).await.map_err(|error| {
            match stringified.filter(|_| serde_found_string(&error)) {
                Some(field) => anyhow!(
                    "{collection} field {field:?} holds a JSON string; send a native JSON object or array in set, not a string. ({error:#})"
                ),
                None => error,
            }
        })
    }

    async fn patch_outcome(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        let outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.preview, "preview is not granted for this agent");
                core.preview(request).await?
            }
            "edit" => {
                self.execution.enter_mutation();
                core.apply(request).await?
            }
            _ => unreachable!("patch verb"),
        };
        outcome_text(&outcome)
    }
}

/// External premise: serde reports a JSON string decoded where a map or
/// sequence was expected only in its `invalid type: string` message.
fn serde_found_string(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("invalid type: string")
}
