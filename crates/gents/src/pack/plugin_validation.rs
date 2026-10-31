use anyhow::{Context, Result};
use std::collections::BTreeSet;

use super::{
    is_distributable_asset_path, is_valid_pack_name, BindAccess, PackPlugin,
    PLUGIN_ARTIFACT_PREFIX, SUPPORTED_PLUGIN_LANGUAGES,
};

impl PackPlugin {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.host_imports.is_empty() || self.job.0,
            "host_imports needs job: true; the host runs a call's script only as a job step"
        );
        anyhow::ensure!(
            self.host_imports.is_empty()
                || self.input_schema.pointer("/properties/script").is_some(),
            "host_imports: run_script needs a `script` property in input_schema"
        );
        if let Some(service) = &self.service {
            anyhow::ensure!(
                crate::plugin::authority::declared_manifold(self)?.child_process,
                "plugin service requires child_process authority"
            );
            anyhow::ensure!(
                !service.argv.is_empty()
                    && service.argv.iter().all(|arg| !arg.contains('\0'))
                    && !service.argv[0].is_empty(),
                "plugin service needs a nonempty argv without NUL bytes"
            );
            anyhow::ensure!(
                (1..=300).contains(&service.startup_timeout_secs),
                "service startup_timeout_secs must be 1..300"
            );
            anyhow::ensure!(
                self.input_schema["properties"].get("service").is_none(),
                "service is a reserved host-filled plugin input"
            );
        }
        anyhow::ensure!(
            is_valid_pack_name(&self.name),
            "plugin name must be snake_case: {:?}",
            self.name
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "plugin {:?} needs a description; it is what the model is shown",
            self.name
        );
        anyhow::ensure!(
            self.artifact.ends_with(".afb"),
            "plugin {:?} artifact must be a compiled .afb path, got {:?}",
            self.name,
            self.artifact
        );
        anyhow::ensure!(
            is_distributable_asset_path(&self.artifact),
            "unsafe plugin artifact path: {:?}",
            self.artifact
        );
        anyhow::ensure!(
            self.artifact.starts_with(PLUGIN_ARTIFACT_PREFIX),
            "plugin {:?} artifact must live under {PLUGIN_ARTIFACT_PREFIX}, got {:?}",
            self.name,
            self.artifact
        );
        let language = self.language.trim().to_ascii_lowercase();
        anyhow::ensure!(
            SUPPORTED_PLUGIN_LANGUAGES.contains(&language.as_str()),
            "plugin {:?} declares language {:?}, which is not one gents pack build knows how to \
             compile; supported languages: {}",
            self.name,
            self.language,
            SUPPORTED_PLUGIN_LANGUAGES.join(", ")
        );
        if let Some(source) = &self.source {
            anyhow::ensure!(
                is_distributable_asset_path(source),
                "unsafe plugin source path: {source:?}"
            );
        }
        if let Some(instructions) = &self.instructions {
            anyhow::ensure!(
                *instructions == format!("{PLUGIN_ARTIFACT_PREFIX}{}/TOOL.md", self.name),
                "plugin {:?} instructions must be plugins/{}/TOOL.md, got {instructions:?}",
                self.name,
                self.name
            );
        }
        anyhow::ensure!(
            self.input_schema.is_object(),
            "plugin {:?} needs an object input_schema",
            self.name
        );
        if let Some(manifold) = &self.manifold {
            let listen = manifold.get("listen");
            anyhow::ensure!(
                listen.is_none_or(|value| value == "None" || value == &serde_json::json!("None")),
                "plugin {:?} asks to listen on a port; a pack's plugins are called, not served",
                self.name
            );
            crate::plugin::http_calls::validate_declared(
                &crate::plugin::authority::declared_manifold(self)?,
            )
            .map_err(|why| {
                anyhow::anyhow!(
                    "plugin {:?} declares a network grant the host cannot serve: {why}",
                    self.name
                )
            })?;
        }
        if let Some(bind_dir) = &self.bind_dir {
            anyhow::ensure!(
                !bind_dir.input_field.trim().is_empty(),
                "plugin {:?} bind_dir.input_field must not be blank",
                self.name
            );
            anyhow::ensure!(
                !bind_dir.description.trim().is_empty(),
                "plugin {:?} bind_dir needs a description; it is what an operator is shown",
                self.name
            );
            anyhow::ensure!(
                schema_declares_string_property(&self.input_schema, &bind_dir.input_field),
                "plugin {:?} bind_dir.input_field {:?} must be a string property of \
                 input_schema, or of at least one oneOf branch, since call_bound always \
                 injects a string",
                self.name,
                bind_dir.input_field
            );
            if let Some(original) = &bind_dir.original_field {
                anyhow::ensure!(
                    original != &bind_dir.input_field
                        && schema_declares_string_property(&self.input_schema, original),
                    "plugin {:?} bind_dir.original_field {:?} must be a string property of \
                     input_schema other than input_field",
                    self.name,
                    original
                );
            }
            anyhow::ensure!(
                bind_dir.write_fields.is_empty() || bind_dir.access == BindAccess::ReadWrite,
                "plugin {:?} declares bind_dir.write_fields but bind_dir.access is read; a \
                 call that sets one would write, so declare access read_write",
                self.name
            );
            for field in &bind_dir.write_fields {
                anyhow::ensure!(
                    field != &bind_dir.input_field
                        && Some(field) != bind_dir.original_field.as_ref()
                        && schema_declares_property(&self.input_schema, field, |_| true),
                    "plugin {:?} bind_dir.write_fields names {field:?}, which must be a property \
                     of input_schema other than the bound path fields",
                    self.name
                );
            }
            let declares_fs = !matches!(
                crate::plugin::authority::declared_manifold(self)?.fs,
                afterburner_core::manifold::FsAccess::None
            );
            anyhow::ensure!(
                !declares_fs,
                "plugin {:?} declares both bind_dir and a manifold fs grant; a directory bound \
                 per call and a standing filesystem grant cannot be expressed together",
                self.name
            );
        }
        if let Some(limits) = &self.limits {
            if let Some(memory_mib) = limits.memory_mib {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_MEMORY_MIB).contains(&memory_mib),
                    "plugin {:?} declares memory_mib {memory_mib}, but it {}",
                    self.name,
                    limit_range_reason(memory_mib, crate::plugin::MAX_DECLARED_MEMORY_MIB, "MiB")
                );
            }
            if let Some(wall_clock_secs) = limits.wall_clock_secs {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS).contains(&wall_clock_secs),
                    "plugin {:?} declares wall_clock_secs {wall_clock_secs}, but it {}",
                    self.name,
                    limit_range_reason(
                        wall_clock_secs,
                        crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS,
                        "s"
                    )
                );
            }
            if let Some(max_output_mib) = limits.max_output_mib {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_OUTPUT_MIB).contains(&max_output_mib),
                    "plugin {:?} declares max_output_mib {max_output_mib}, but it {}",
                    self.name,
                    limit_range_reason(
                        max_output_mib,
                        crate::plugin::MAX_DECLARED_OUTPUT_MIB,
                        "MiB"
                    )
                );
            }
        }
        self.validate_job_manifest_fields()
    }

    /// The static rules for the `data` field;
    /// checks against the archive itself belong to install.
    fn validate_job_manifest_fields(&self) -> Result<()> {
        let name = &self.name;
        if let Some(data) = &self.data {
            anyhow::ensure!(
                !data.collections.is_empty(),
                "plugin {name:?} data.collections is empty; list the collections it may use or \
                 drop data"
            );
            let mut seen = BTreeSet::new();
            for collection in &data.collections {
                anyhow::ensure!(
                    seen.insert(collection),
                    "plugin {name:?} data.collections lists {collection:?} twice; list each once"
                );
            }
            for op in &data.ops {
                anyhow::ensure!(
                    DATA_OPS.contains(&op.as_str()),
                    "plugin {name:?} data.ops has unknown op {op:?}; use {}",
                    DATA_OPS.join(", ")
                );
            }
            for field in data.stamped.keys() {
                crate::store::graphql::validate_graphql_name(field).with_context(|| {
                    format!(
                        "plugin {name:?} data.stamped key {field:?} must be a GraphQL field name"
                    )
                })?;
                anyhow::ensure!(
                    !crate::tools::native::is_secret_env_name(field),
                    "plugin {name:?} data.stamped field {field:?} looks like a secret name; \
                     rename the field"
                );
            }
        }
        Ok(())
    }
}

/// The operations `data.ops` may list.
const DATA_OPS: &[&str] = &["query", "insert", "update", "delete"];

fn limit_range_reason(value: u32, ceiling: u32, unit: &str) -> String {
    if value == 0 {
        "must be at least 1".to_owned()
    } else {
        format!("must be at most the host ceiling of {ceiling} {unit}")
    }
}

fn schema_declares_string_property(input_schema: &serde_json::Value, field: &str) -> bool {
    schema_declares_property(input_schema, field, |property| {
        property.get("type").and_then(serde_json::Value::as_str) == Some("string")
    })
}

fn schema_declares_property(
    input_schema: &serde_json::Value,
    field: &str,
    accept: impl Fn(&serde_json::Value) -> bool,
) -> bool {
    let declares = |schema: &serde_json::Value| {
        schema
            .get("properties")
            .and_then(|properties| properties.get(field))
            .is_some_and(&accept)
    };
    declares(input_schema)
        || input_schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| branches.iter().any(declares))
}
