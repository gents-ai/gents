use anyhow::{Context, Result};

pub fn checked_component(kind: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "invalid plugin {kind} {value:?}: use letters, digits, underscore or hyphen"
    );
    Ok(())
}

pub fn parse_coordinate(coordinate: &str) -> Result<(&str, &str)> {
    let (namespace, name) = coordinate
        .split_once('/')
        .with_context(|| format!("{coordinate:?} is not a plugin; name it as namespace/name"))?;
    checked_component("namespace", namespace)?;
    checked_component("name", name)?;
    Ok((namespace, name))
}
