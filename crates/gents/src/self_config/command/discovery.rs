use super::*;
use crate::configuration_discovery::{
    discover_configuration_within, DiscoveryLimits, DiscoveryRequest, DiscoveryScope,
    DiscoverySourceKind, DiscoverySourceRoot, DEFAULT_MAX_DISCOVERY_SOURCES,
};

const MAX_SOURCE_PATH_BYTES: usize = 4_096;

struct RequestedSource {
    source_id: String,
    kind: DiscoverySourceKind,
    scope: DiscoveryScope,
    path: String,
}

impl ConfigCommandTool {
    pub(super) async fn discovery(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let requested = parse_sources(argv)?;
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(crate::tool_surface::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        anyhow::ensure!(
            mode != crate::tool_surface::FileToolMode::Off,
            "configuration discovery requires effective file read permission on the invoking behavior"
        );
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .context("configuration discovery requires an effective tool root")?;
        let context = crate::toolset::ToolContext::new(root.into(), false)?;
        let sources = requested
            .into_iter()
            .map(|source| {
                let root = context.resolve_path_allow_create(&source.path)?;
                Ok(DiscoverySourceRoot {
                    source_id: source.source_id,
                    kind: source.kind,
                    scope: source.scope,
                    root,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        discover_configuration_within(
            &DiscoveryRequest {
                sources,
                limits: DiscoveryLimits::default(),
            },
            &context.root(),
        )
        .to_model_json_pretty()
        .context("serializing sanitized configuration discovery inventory")
    }
}

fn parse_sources(argv: &[String]) -> Result<Vec<RequestedSource>> {
    anyhow::ensure!(
        argv.first().is_some_and(|value| value == "scan"),
        "use config discovery scan --source SOURCE_ID claude|codex|grok user|project PATH"
    );
    let mut index = 1;
    let mut sources = Vec::new();
    while index < argv.len() {
        anyhow::ensure!(
            argv.get(index).is_some_and(|value| value == "--source")
                && argv.len().saturating_sub(index) >= 5,
            "each discovery source must be --source SOURCE_ID claude|codex|grok user|project PATH"
        );
        let source_id = argv[index + 1].clone();
        anyhow::ensure!(
            !source_id.is_empty()
                && source_id.len() <= 96
                && source_id.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_' | b'-')),
            "discovery source ID must contain only ASCII letters, digits, '.', '_', or '-' and be at most 96 bytes"
        );
        let kind = match argv[index + 2].as_str() {
            "claude" => DiscoverySourceKind::Claude,
            "codex" => DiscoverySourceKind::Codex,
            "grok" => DiscoverySourceKind::Grok,
            other => anyhow::bail!(
                "unsupported discovery source kind {other:?}; expected claude, codex, or grok"
            ),
        };
        let scope = match argv[index + 3].as_str() {
            "user" => DiscoveryScope::User,
            "project" => DiscoveryScope::Project,
            other => anyhow::bail!(
                "unsupported discovery source scope {other:?}; expected user or project"
            ),
        };
        let path = argv[index + 4].clone();
        anyhow::ensure!(
            !path.is_empty() && path.len() <= MAX_SOURCE_PATH_BYTES,
            "discovery source path must be non-empty and at most {MAX_SOURCE_PATH_BYTES} bytes"
        );
        anyhow::ensure!(
            sources.len() < DEFAULT_MAX_DISCOVERY_SOURCES,
            "configuration discovery accepts at most {DEFAULT_MAX_DISCOVERY_SOURCES} explicit sources"
        );
        sources.push(RequestedSource {
            source_id,
            kind,
            scope,
            path,
        });
        index += 5;
    }
    anyhow::ensure!(
        !sources.is_empty(),
        "configuration discovery requires at least one explicit --source"
    );
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_arguments_are_explicit_and_typed() {
        let argv = [
            "scan",
            "--source",
            "codex-user",
            "codex",
            "user",
            "homes/codex",
            "--source",
            "grok-project",
            "grok",
            "project",
            "project",
        ]
        .map(str::to_owned);
        let parsed = parse_sources(&argv).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].source_id, "codex-user");
        assert_eq!(parsed[0].kind, DiscoverySourceKind::Codex);
        assert_eq!(parsed[0].scope, DiscoveryScope::User);
        assert_eq!(parsed[1].kind, DiscoverySourceKind::Grok);
        assert_eq!(parsed[1].scope, DiscoveryScope::Project);
    }

    #[test]
    fn malformed_source_arguments_fail_closed() {
        for argv in [
            vec!["scan"],
            vec!["scan", "--source", "id", "unknown", "user", "."],
            vec!["scan", "--source", "id", "codex", "machine", "."],
            vec!["scan", "source", "id", "codex", "user", "."],
        ] {
            let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(parse_sources(&argv).is_err(), "accepted {argv:?}");
        }
    }

    #[test]
    fn discovery_is_the_only_public_command_vocabulary() {
        assert!(CONFIG_USAGE.contains("[\"discovery\", \"scan\""));
        assert!(!CONFIG_USAGE.contains("[\"discover\", \"scan\""));
        assert!(parse_sources(
            &["scan", "--source", "bad/source", "codex", "user", ".",].map(str::to_owned)
        )
        .is_err());

        let mut too_many = vec!["scan".to_owned()];
        for index in 0..=DEFAULT_MAX_DISCOVERY_SOURCES {
            too_many.extend(
                [
                    "--source".to_owned(),
                    format!("source-{index}"),
                    "codex".to_owned(),
                    "user".to_owned(),
                    ".".to_owned(),
                ]
                .into_iter(),
            );
        }
        assert!(parse_sources(&too_many).is_err());
    }
}
