use std::collections::BTreeMap;

use gents_protocol::operations::pack::{
    DigestAdmission, DigestExpectation, EditedAdmission, EditedDocuments, PackActor,
    PackChangeInput, PackOperation, PackOperationError, PackVariables, ReaderAction, RootReader,
    VariableSource,
};
use gents_store::config_client::ConfigAccess;

use crate::pack::{DependencyVariableOverrides, DriftPolicy};

const MAX_ENTRIES: usize = 32;
const MAX_VARIABLE_NAME_BYTES: usize = 128;
const MAX_VARIABLE_VALUE_BYTES: usize = 4096;
const RUNTIME_NODE_VARIABLE: &str = "GENTS_PACK_NODE_DID";

/// Lean `Pack.Operations.digestAdmission`.
pub fn digest_admission(
    actor: PackActor,
    commit: bool,
    digest: DigestExpectation,
) -> DigestAdmission {
    match digest {
        DigestExpectation::Differing => DigestAdmission::Changed,
        DigestExpectation::Matching => DigestAdmission::Admitted,
        DigestExpectation::Absent if commit && actor == PackActor::Agent => {
            DigestAdmission::PreviewRequired
        }
        DigestExpectation::Absent => DigestAdmission::Admitted,
    }
}

/// Admits `operation` on the artifact that resolved to `resolved`, before any
/// write.
pub fn admit_digest(
    actor: PackActor,
    operation: PackOperation,
    commit: bool,
    expected: Option<&str>,
    resolved: &str,
) -> Result<(), PackOperationError> {
    match digest_admission(actor, commit, DigestExpectation::of(expected, resolved)) {
        DigestAdmission::Admitted => Ok(()),
        DigestAdmission::PreviewRequired => Err(PackOperationError::PreviewRequired(operation)),
        DigestAdmission::Changed => Err(PackOperationError::DigestChanged {
            expected: expected.unwrap_or_default().to_owned(),
            resolved: resolved.to_owned(),
        }),
    }
}

/// Lean `Pack.Operations.editedAdmission`.
pub fn edited_admission(
    commit: bool,
    edited_present: bool,
    decision: EditedDocuments,
) -> EditedAdmission {
    if commit && edited_present && decision == EditedDocuments::Refuse {
        EditedAdmission::DecisionRequired
    } else {
        EditedAdmission::Admitted
    }
}

pub fn drift_policy(decision: EditedDocuments) -> DriftPolicy {
    match decision {
        EditedDocuments::Refuse => DriftPolicy::Refuse,
        EditedDocuments::Overwrite => DriftPolicy::Overwrite,
        EditedDocuments::Keep => DriftPolicy::Keep,
    }
}

/// Refuses a commit that would meet documents of `coordinate` edited since
/// its install without the caller's decision.
pub async fn admit_edited(
    access: &ConfigAccess,
    node_did: &str,
    coordinate: &str,
    removal: bool,
    decision: EditedDocuments,
) -> Result<(), PackOperationError> {
    if decision != EditedDocuments::Refuse {
        return Ok(());
    }
    let documents = crate::pack::edited_installed_documents(access, node_did, coordinate, removal)
        .await
        .map_err(|error| PackOperationError::Passthrough(error.into()))?;
    match edited_admission(true, !documents.is_empty(), decision) {
        EditedAdmission::Admitted => Ok(()),
        EditedAdmission::DecisionRequired => Err(PackOperationError::EditedUndecided {
            coordinate: coordinate.to_owned(),
            documents,
            removal,
        }),
    }
}

/// Lean `Pack.Operations.variableSource`.
pub fn variable_source(
    actor: PackActor,
    for_pack: bool,
    for_install: bool,
    in_environment: bool,
) -> VariableSource {
    if for_pack {
        VariableSource::Scoped
    } else if for_install {
        VariableSource::Shared
    } else if actor == PackActor::User && in_environment {
        VariableSource::Environment
    } else {
        VariableSource::Unset
    }
}

fn variable_name(name: &str, raw: &str) -> Result<(), PackOperationError> {
    let well_formed = name != RUNTIME_NODE_VARIABLE
        && !name.is_empty()
        && name.len() <= MAX_VARIABLE_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if !well_formed {
        return Err(PackOperationError::VariableName(raw.to_owned()));
    }
    if name.ends_with("_MODEL") || name.ends_with("_ENDPOINT") {
        return Err(PackOperationError::VariableInference(name.to_owned()));
    }
    Ok(())
}

/// The install variables `values` supply, each `NAME=VALUE` or
/// `NAMESPACE/PACK:NAME=VALUE`.
pub fn parse_variables(values: &[String]) -> Result<PackVariables, PackOperationError> {
    let mut variables = PackVariables::default();
    for raw in values {
        let (key, value) = raw
            .split_once('=')
            .ok_or_else(|| PackOperationError::VariableMalformed(raw.clone()))?;
        if value.len() > MAX_VARIABLE_VALUE_BYTES {
            return Err(PackOperationError::VariableTooLarge(key.to_owned()));
        }
        let previous = match key.rsplit_once(':') {
            Some((coordinate, name)) => {
                if coordinate.is_empty() {
                    return Err(PackOperationError::VariableMalformed(raw.clone()));
                }
                variable_name(name, key)?;
                variables
                    .scoped
                    .entry(coordinate.to_owned())
                    .or_default()
                    .insert(name.to_owned(), value.to_owned())
            }
            None => {
                variable_name(key, key)?;
                variables.shared.insert(key.to_owned(), value.to_owned())
            }
        };
        if previous.is_some() {
            return Err(PackOperationError::VariableDuplicate(key.to_owned()));
        }
    }
    if variables.len() > MAX_ENTRIES {
        return Err(PackOperationError::TooManyVariables);
    }
    Ok(variables)
}

/// The inference slot bindings `values` name, each `NAME=PROFILE_ID`.
pub fn parse_inference_slots(
    values: &[String],
) -> Result<BTreeMap<String, String>, PackOperationError> {
    let mut bindings = BTreeMap::new();
    for raw in values {
        let (slot, profile) = raw
            .split_once('=')
            .map(|(slot, profile)| (slot.trim(), profile.trim()))
            .filter(|(slot, profile)| !slot.is_empty() && !profile.is_empty())
            .ok_or_else(|| PackOperationError::SlotMalformed(raw.clone()))?;
        if bindings
            .insert(slot.to_owned(), profile.to_owned())
            .is_some()
        {
            return Err(PackOperationError::SlotDuplicate(slot.to_owned()));
        }
    }
    if bindings.len() > MAX_ENTRIES {
        return Err(PackOperationError::TooManySlots);
    }
    Ok(bindings)
}

/// The value the root pack and each dependency read for `name` when no value
/// was supplied for that pack alone.
pub fn install_value(actor: PackActor, variables: &PackVariables, name: &str) -> Option<String> {
    let shared = variables.shared.get(name);
    let in_environment = std::env::var_os(name).is_some();
    match variable_source(actor, false, shared.is_some(), in_environment) {
        VariableSource::Shared => shared.cloned(),
        VariableSource::Environment => std::env::var(name).ok(),
        VariableSource::Scoped | VariableSource::Unset => None,
    }
}

/// The values supplied for one pack alone, as the install engine reads them.
pub fn dependency_overrides(variables: &PackVariables) -> DependencyVariableOverrides {
    variables
        .scoped
        .iter()
        .map(|(coordinate, values)| {
            let values = values
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect();
            (coordinate.clone(), values)
        })
        .collect()
}

/// Checks what a change names before anything is resolved.
pub fn admit_input(input: &PackChangeInput) -> Result<(), PackOperationError> {
    let package = input.package.trim();
    if package.is_empty() {
        return Err(PackOperationError::PackageBlank);
    }
    if crate::pack::local::classify(package).is_none() {
        gents_documents::pack_spec::parse_pack_spec(package)
            .map_err(|error| PackOperationError::Passthrough(error.into()))?;
    }
    if input.variables.len() > MAX_ENTRIES {
        return Err(PackOperationError::TooManyVariables);
    }
    if input.inference_slots.len() > MAX_ENTRIES {
        return Err(PackOperationError::TooManySlots);
    }
    Ok(())
}

/// Lean `Pack.Operations.readerAction`.
pub fn reader_action(actor: PackActor, reader: RootReader) -> ReaderAction {
    match (reader, actor) {
        (RootReader::NotNeeded, _) => ReaderAction::None,
        (RootReader::Unpublished, _) => ReaderAction::ReportPublishStep,
        (RootReader::Published, PackActor::User) => ReaderAction::Grant,
        (RootReader::Published, PackActor::Agent) => ReaderAction::ReportGrantStep,
    }
}

#[cfg(test)]
mod conformance {
    use gents_lean_vocab::families;

    use super::*;

    #[test]
    fn digest_admission_matches_the_lean_model() {
        families::pack_operations_digest_admission::replay(|input| {
            digest_admission(input.actor, input.commit, input.digest)
        });
    }

    #[test]
    fn edited_admission_matches_the_lean_model() {
        families::pack_operations_edited_admission::replay(|input| {
            edited_admission(input.commit, input.edited_present, input.decision)
        });
    }

    #[test]
    fn variable_source_matches_the_lean_model() {
        families::pack_operations_variable_source::replay(|input| {
            variable_source(
                input.actor,
                input.for_pack,
                input.for_install,
                input.in_environment,
            )
        });
    }

    #[test]
    fn reader_action_matches_the_lean_model() {
        families::pack_operations_reader_action::replay(|input| {
            reader_action(input.actor, input.root_reader)
        });
    }
}
