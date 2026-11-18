use defra_node::{LensConfig, LensModule};

use crate::registry::LensSpec;

pub fn lens_config(spec: &LensSpec<'_>, source: &str, destination: &str) -> LensConfig {
    let mut module = LensModule::from_bytes(spec.wasm.to_vec());
    if let Some(args) = spec.args_json {
        if let Ok(value) = serde_json::from_str(args) {
            module = module.with_arguments(value);
        }
    }
    LensConfig::new(source, destination, module)
}

pub fn predict_transform_id(spec: &LensSpec<'_>) -> String {
    let config = lens_config(spec, "", "");
    let module = &config.lenses[0];
    let mut arguments = module
        .arguments
        .as_ref()
        .map(|value| value.as_object().expect("lens arguments must be named"))
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), defra_core::encode_lens_argument(value)))
        .collect::<Vec<_>>();
    arguments.sort_by(|a, b| a.0.cmp(&b.0));
    let (cid, _) = defra_core::build_lens_ipld_blocks(&[defra_core::LensModuleParts {
        wasm_bytes: spec.wasm,
        inverse: module.inverse,
        arguments: &arguments,
    }])
    .expect("encode canonical lens blocks");
    cid.to_string()
}
