mod automation;
mod live;
mod manifest;
mod projection;
mod tooling;

pub(crate) use live::validate_manifest_against_live;
pub(crate) use manifest::validate_manifest;
