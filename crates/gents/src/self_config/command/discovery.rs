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
        if mode == crate::tool_surface::FileToolMode::Off {
            return Err(CommandError::DiscoveryFileReadOff.into());
        }
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .ok_or(CommandError::DiscoveryToolRoot)?;
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
        Ok(discover_configuration_within(
            &DiscoveryRequest {
                sources,
                limits: DiscoveryLimits::default(),
            },
            &context.root(),
        )
        .to_model_json_pretty()
        .map_err(CommandError::DiscoveryInventory)?)
    }
}

fn parse_sources(argv: &[String]) -> Result<Vec<RequestedSource>, CommandError> {
    if !argv.first().is_some_and(|value| value == "scan") {
        return Err(CommandError::DiscoveryUsage);
    }
    let mut index = 1;
    let mut sources = Vec::new();
    while index < argv.len() {
        if !(argv.get(index).is_some_and(|value| value == "--source")
            && argv.len().saturating_sub(index) >= 5)
        {
            return Err(CommandError::DiscoverySourceUsage);
        }
        let source_id = argv[index + 1].clone();
        if !(!source_id.is_empty()
            && source_id.len() <= 96
            && source_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
        {
            return Err(CommandError::DiscoverySourceId);
        }
        let kind = match argv[index + 2].as_str() {
            "claude" => DiscoverySourceKind::Claude,
            "codex" => DiscoverySourceKind::Codex,
            "grok" => DiscoverySourceKind::Grok,
            other => {
                return Err(CommandError::DiscoverySourceKind {
                    kind: other.to_owned(),
                })
            }
        };
        let scope = match argv[index + 3].as_str() {
            "user" => DiscoveryScope::User,
            "project" => DiscoveryScope::Project,
            other => {
                return Err(CommandError::DiscoverySourceScope {
                    scope: other.to_owned(),
                })
            }
        };
        let path = argv[index + 4].clone();
        if path.is_empty() || path.len() > MAX_SOURCE_PATH_BYTES {
            return Err(CommandError::DiscoverySourcePath {
                max: MAX_SOURCE_PATH_BYTES,
            });
        }
        if sources.len() >= DEFAULT_MAX_DISCOVERY_SOURCES {
            return Err(CommandError::DiscoverySourceLimit {
                max: DEFAULT_MAX_DISCOVERY_SOURCES,
            });
        }
        sources.push(RequestedSource {
            source_id,
            kind,
            scope,
            path,
        });
        index += 5;
    }
    if sources.is_empty() {
        return Err(CommandError::DiscoveryNoSources);
    }
    Ok(sources)
}
