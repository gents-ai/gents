use anyhow::Result;

pub const DEFAULT_NAMESPACE: &str = "gents";

pub fn is_valid_pack_name(name: &str) -> bool {
    crate::is_snake_case_name(name)
}

pub fn is_valid_index_version(version: &str) -> bool {
    version != "."
        && version != ".."
        && !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSpec<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
}

pub fn parse_pack_spec(spec: &str) -> Result<PackSpec<'_>> {
    let (coordinate, version) = match spec.split_once('@') {
        Some((coordinate, version)) => {
            anyhow::ensure!(!version.is_empty(), "{spec:?} names no version after @");
            anyhow::ensure!(
                is_valid_index_version(version),
                "{spec:?} names an invalid version; a version is ASCII letters, \
                 digits, '.', '+' or '-', and never a bare '.' or '..'"
            );
            (coordinate, Some(version))
        }
        None => (spec, None),
    };
    let (namespace, name) = coordinate
        .split_once('/')
        .unwrap_or((DEFAULT_NAMESPACE, coordinate));
    anyhow::ensure!(
        is_valid_pack_name(namespace) && is_valid_pack_name(name),
        "{spec:?} is not a pack name; expected name, ns/name, or either with @version, \
         namespace and name both snake_case"
    );
    Ok(PackSpec {
        namespace,
        name,
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pack_spec_accepts_an_uppercase_semver_pin() {
        assert_eq!(
            parse_pack_spec("demo@1.0.0-RC1").unwrap().version,
            Some("1.0.0-RC1")
        );
    }

    #[test]
    fn parse_pack_spec_refuses_malformed_or_non_snake_case_specs() {
        for spec in [
            "",
            "acme/",
            "/demo",
            "acme/demo/extra",
            "demo@",
            "Acme/Demo",
            "acme/de-mo",
            "demo@1@2",
            "demo@a b",
            "demo@../x",
        ] {
            assert!(parse_pack_spec(spec).is_err(), "{spec:?} should be refused");
        }
    }
}
