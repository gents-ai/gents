//! The typed `claude_platform` settings of an inference backend and the rules
//! that bind them to a Claude platform provider kind.
//!
//! A platform backend (Bedrock, Vertex, Foundry, Claude Platform on AWS or
//! Google Cloud, Mantle, a gateway) is served from a place the settings name;
//! its credential stays in the backend's `auth`. A kind needs the settings its
//! shape requires and refuses every other one; a kind that is not a platform
//! refuses the object itself. The required set, the optional set and the
//! default base URL of each kind are the Lean `Configuration.ClaudePlatform`
//! owner; the generated cases bind this module to it. The field value checks
//! (region, project, location, workspace and resource spelling) follow Claude
//! Code 2.1.311 where it checks them and are Rust-only.

use serde::{Deserialize, Serialize};

use crate::backend_provider::BackendProviderKind;

/// Where a Claude platform backend is served from. Only the fields the
/// backend's provider kind uses are set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ClaudePlatform {
    /// Cloud region (AWS platforms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub region: Option<String>,
    /// Google Cloud project id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub project: Option<String>,
    /// Google Cloud location; Vertex defaults to us-east5, Google Cloud to global.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub location: Option<String>,
    /// Workspace id (Claude Platform on AWS and on Google Cloud).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub workspace_id: Option<String>,
    /// Foundry resource name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub resource: Option<String>,
    /// Foundry only: send the static credential as `Authorization: Bearer`
    /// instead of `x-api-key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub bearer_token: Option<bool>,
}

/// Why a backend's `claude_platform` settings are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaudePlatformError {
    /// The kind needs a setting the backend does not set.
    #[error("{kind} needs the {field} setting")]
    Missing {
        kind: BackendProviderKind,
        field: &'static str,
    },
    /// The kind does not use a setting the backend sets.
    #[error("{kind} does not use the {field} setting")]
    Refused {
        kind: BackendProviderKind,
        field: &'static str,
    },
    /// The kind is not a Claude platform and takes no settings.
    #[error("{kind} takes no claude_platform settings")]
    NotAPlatform { kind: BackendProviderKind },
    /// A setting is spelled in a way the platform cannot use.
    #[error("the {field} setting {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Region,
    Project,
    Location,
    WorkspaceId,
    Resource,
    BearerToken,
}

impl Field {
    const ALL: [Self; 6] = [
        Self::Region,
        Self::Project,
        Self::Location,
        Self::WorkspaceId,
        Self::Resource,
        Self::BearerToken,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Region => "region",
            Self::Project => "project",
            Self::Location => "location",
            Self::WorkspaceId => "workspace_id",
            Self::Resource => "resource",
            Self::BearerToken => "bearer_token",
        }
    }

    fn is_set(self, settings: &ClaudePlatform) -> bool {
        match self {
            Self::Region => settings.region.is_some(),
            Self::Project => settings.project.is_some(),
            Self::Location => settings.location.is_some(),
            Self::WorkspaceId => settings.workspace_id.is_some(),
            Self::Resource => settings.resource.is_some(),
            Self::BearerToken => settings.bearer_token.is_some(),
        }
    }
}

/// The required and optional settings of a platform kind (Lean `shape`);
/// `None` for a kind that is not a Claude platform.
fn shape(kind: BackendProviderKind) -> Option<(&'static [Field], &'static [Field])> {
    use BackendProviderKind as Kind;
    match kind {
        Kind::ClaudeBedrock | Kind::ClaudeMantle => Some((&[Field::Region], &[])),
        Kind::ClaudeVertex => Some((&[Field::Project], &[Field::Location])),
        Kind::ClaudeFoundry => Some((&[Field::Resource], &[Field::BearerToken])),
        Kind::ClaudeAnthropicAws => Some((&[Field::Region, Field::WorkspaceId], &[])),
        Kind::ClaudeGoogleCloud => {
            Some((&[Field::Project, Field::WorkspaceId], &[Field::Location]))
        }
        Kind::ClaudeGateway => Some((&[], &[])),
        _ => None,
    }
}

/// `^[a-z]{2,}(?:-[a-z0-9]+){0,4}$`, Claude Code's AWS region check.
fn aws_region(value: &str) -> bool {
    let mut parts = value.split('-');
    let head = parts.next().unwrap_or("");
    let tail: Vec<&str> = parts.collect();
    head.len() >= 2
        && head.bytes().all(|b| b.is_ascii_lowercase())
        && tail.len() <= 4
        && tail.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// `^[A-Za-z0-9_-]+$`, Claude Code's Google Cloud identifier check.
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])$` ignoring case, Claude Code's Foundry resource check.
fn foundry_resource(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

impl ClaudePlatform {
    /// Checks `settings` against the shape of `kind`, the field spellings included.
    pub fn validate(
        kind: BackendProviderKind,
        settings: Option<&Self>,
    ) -> Result<(), ClaudePlatformError> {
        let Some((required, optional)) = shape(kind) else {
            return match settings {
                None => Ok(()),
                Some(_) => Err(ClaudePlatformError::NotAPlatform { kind }),
            };
        };
        let empty = Self::default();
        let settings = settings.unwrap_or(&empty);
        if let Some(field) = required.iter().find(|field| !field.is_set(settings)) {
            return Err(ClaudePlatformError::Missing {
                kind,
                field: field.name(),
            });
        }
        if let Some(field) = Field::ALL.iter().find(|field| {
            field.is_set(settings) && !required.contains(field) && !optional.contains(field)
        }) {
            return Err(ClaudePlatformError::Refused {
                kind,
                field: field.name(),
            });
        }
        settings.validate_spelling()
    }

    fn validate_spelling(&self) -> Result<(), ClaudePlatformError> {
        let invalid = |field, reason| ClaudePlatformError::Invalid { field, reason };
        if let Some(region) = &self.region {
            if !aws_region(region) {
                return Err(invalid(
                    "region",
                    "must be a cloud region such as us-west-2",
                ));
            }
        }
        for (field, value) in [
            ("project", &self.project),
            ("location", &self.location),
            ("workspace_id", &self.workspace_id),
        ] {
            if value.as_deref().is_some_and(|value| !identifier(value)) {
                return Err(invalid(
                    field,
                    "may only hold letters, digits, hyphens and underscores",
                ));
            }
        }
        if self
            .resource
            .as_deref()
            .is_some_and(|resource| !foundry_resource(resource))
        {
            return Err(invalid(
                "resource",
                "must be a Foundry resource name such as my-resource, not a URL",
            ));
        }
        Ok(())
    }

    /// The base URL a platform kind is served from when `endpoint` is not
    /// overridden (Lean `defaultBaseUrl`); `None` for a gateway, which has no
    /// default, and for a kind that is not a platform.
    pub fn default_endpoint(
        kind: BackendProviderKind,
        settings: Option<&Self>,
    ) -> Result<Option<String>, ClaudePlatformError> {
        use BackendProviderKind as Kind;
        Self::validate(kind, settings)?;
        let settings = settings.cloned().unwrap_or_default();
        let get = |value: &Option<String>| value.clone().unwrap_or_default();
        Ok(match kind {
            Kind::ClaudeBedrock => Some(format!(
                "https://bedrock-runtime.{}.amazonaws.com",
                get(&settings.region)
            )),
            Kind::ClaudeMantle => Some(format!(
                "https://bedrock-mantle.{}.api.aws/anthropic",
                get(&settings.region)
            )),
            Kind::ClaudeAnthropicAws => Some(format!(
                "https://aws-external-anthropic.{}.api.aws",
                get(&settings.region)
            )),
            Kind::ClaudeFoundry => Some(format!(
                "https://{}.services.ai.azure.com/anthropic/",
                get(&settings.resource)
            )),
            Kind::ClaudeVertex => {
                let location = settings.location.as_deref().unwrap_or("us-east5");
                Some(match location {
                    "global" => "https://aiplatform.googleapis.com/v1".to_owned(),
                    "us" | "eu" => format!("https://aiplatform.{location}.rep.googleapis.com/v1"),
                    other => format!("https://{other}-aiplatform.googleapis.com/v1"),
                })
            }
            Kind::ClaudeGoogleCloud => Some(format!(
                "https://claude.googleapis.com/v1/projects/{}/locations/{}/workspaces/{}/invoke",
                get(&settings.project),
                settings.location.as_deref().unwrap_or("global"),
                get(&settings.workspace_id)
            )),
            _ => None,
        })
    }

    /// Whether `endpoint` is the default base URL of the kind and settings,
    /// ignoring a trailing slash.
    pub fn is_default_endpoint(
        kind: BackendProviderKind,
        settings: Option<&Self>,
        endpoint: &str,
    ) -> bool {
        matches!(
            Self::default_endpoint(kind, settings),
            Ok(Some(default)) if default.trim_end_matches('/') == endpoint.trim_end_matches('/')
        )
    }
}

/// The Messages URL under a platform base URL: `/v1/messages?beta=true`, with
/// the SDK's single-slash join of a base that ends in `/`.
pub fn messages_uri(base: &str) -> String {
    format!(
        "{}/v1/messages?beta=true",
        base.strip_suffix('/').unwrap_or(base)
    )
}

#[cfg(test)]
#[path = "claude_platform_tests.rs"]
mod tests;
