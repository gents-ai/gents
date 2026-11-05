//! Adapter-boundary classifier for MCP servers that refuse the handshake for
//! authorization. External premise: rmcp's streamable HTTP client reports a 401
//! with `WWW-Authenticate` as `StreamableHttpError::AuthRequired` and a 403 scope
//! challenge as `StreamableHttpError::InsufficientScope`, carried in
//! `ClientInitializeError::TransportError`.

use rmcp::service::ClientInitializeError;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransport, StreamableHttpError,
};

/// Why an MCP server refused the connection. Only the challenge parameters a
/// client acts on are kept; the raw header is server-chosen text and is never
/// stored or echoed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpAuthRefusal {
    #[error(
        "authorization required (HTTP 401){}",
        challenge(resource_metadata, scope)
    )]
    Required {
        /// RFC 9728 protected-resource metadata URL, when the server names one.
        resource_metadata: Option<String>,
        scope: Option<String>,
    },
    #[error("insufficient scope (HTTP 403){}", challenge(&None, scope))]
    InsufficientScope { scope: Option<String> },
}

impl McpAuthRefusal {
    /// Health `last_error_class` for this refusal.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Required { .. } => "auth_required",
            Self::InsufficientScope { .. } => "insufficient_scope",
        }
    }
}

/// The handshake error when the server refused for authorization; its source is
/// the typed refusal so later owners can ask for it instead of parsing text.
#[derive(Debug, thiserror::Error)]
#[error("MCP handshake failed for {service_id} ({endpoint}): {refusal}")]
pub struct McpHandshakeRefused {
    pub service_id: String,
    pub endpoint: String,
    #[source]
    pub refusal: McpAuthRefusal,
}

/// Classifier bound to the client type of `transport`, captured before the
/// transport is consumed so the downcast names exactly its error type.
pub(super) fn refusal_classifier<C>(
    _transport: &StreamableHttpClientTransport<C>,
) -> fn(&ClientInitializeError) -> Option<McpAuthRefusal>
where
    C: StreamableHttpClient + 'static,
{
    classify::<C>
}

fn classify<C>(error: &ClientInitializeError) -> Option<McpAuthRefusal>
where
    C: StreamableHttpClient + 'static,
{
    let ClientInitializeError::TransportError { error, .. } = error else {
        return None;
    };
    match error
        .error
        .downcast_ref::<StreamableHttpError<C::Error>>()?
    {
        StreamableHttpError::AuthRequired(required) => {
            let params = challenge_params(&required.www_authenticate_header);
            Some(McpAuthRefusal::Required {
                resource_metadata: params.resource_metadata,
                scope: params.scope,
            })
        }
        StreamableHttpError::InsufficientScope(insufficient) => {
            Some(McpAuthRefusal::InsufficientScope {
                scope: insufficient
                    .required_scope
                    .clone()
                    .or_else(|| challenge_params(&insufficient.www_authenticate_header).scope),
            })
        }
        _ => None,
    }
}

#[derive(Default)]
struct ChallengeParams {
    resource_metadata: Option<String>,
    scope: Option<String>,
}

/// Reads `resource_metadata` and `scope` from a `WWW-Authenticate` challenge and
/// ignores every other parameter (`error_description`, `realm`, ...).
fn challenge_params(header: &str) -> ChallengeParams {
    let mut params = ChallengeParams::default();
    let mut rest = header.trim();
    // Skip the auth scheme token ("Bearer").
    if let Some((scheme, tail)) = rest.split_once(char::is_whitespace) {
        if !scheme.contains('=') {
            rest = tail;
        }
    }
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        let Some((name, after)) = rest.split_once('=') else {
            break;
        };
        let name = name.trim().to_ascii_lowercase();
        let after = after.trim_start();
        let (value, tail) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => (quoted, ""),
            }
        } else {
            match after.find(',') {
                Some(end) => (after[..end].trim(), &after[end..]),
                None => (after.trim(), ""),
            }
        };
        match name.as_str() {
            "resource_metadata" => {
                params.resource_metadata = reqwest::Url::parse(value)
                    .ok()
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                    .map(String::from);
            }
            "scope" if !value.is_empty() => params.scope = Some(value.to_owned()),
            _ => {}
        }
        rest = tail;
    }
    params
}

fn challenge(resource_metadata: &Option<String>, scope: &Option<String>) -> String {
    let mut text = String::new();
    if let Some(url) = resource_metadata {
        text.push_str("; resource_metadata=");
        text.push_str(url);
    }
    if let Some(scope) = scope {
        text.push_str("; scope=");
        text.push_str(scope);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_keeps_only_resource_metadata_and_scope() {
        let params = challenge_params(
            r#"Bearer realm="x", error="invalid_token", error_description="token abc123 expired", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="read write""#,
        );
        assert_eq!(
            params.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(params.scope.as_deref(), Some("read write"));
        let refusal = McpAuthRefusal::Required {
            resource_metadata: params.resource_metadata,
            scope: params.scope,
        };
        assert!(!refusal.to_string().contains("abc123"));
    }

    #[test]
    fn challenge_tolerates_bare_tokens_and_rejects_non_http_metadata() {
        let params = challenge_params("Bearer scope=read, resource_metadata=javascript:alert(1)");
        assert_eq!(params.scope.as_deref(), Some("read"));
        assert!(params.resource_metadata.is_none());
        let empty = challenge_params("Bearer");
        assert!(empty.scope.is_none() && empty.resource_metadata.is_none());
    }
}
