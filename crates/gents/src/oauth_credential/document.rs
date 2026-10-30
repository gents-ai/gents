use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::row::OAuthCredentialRow;
use serde_json::Value;

use super::*;

pub(super) const OAUTH_CREDENTIAL_FIELDS: &str = "_docID credential_id node_did provider access_token refresh_token id_token account_id chatgpt_plan_type is_fedramp access_token_expires_at last_refresh enabled account_ref connected_at provider_account_key label";
pub(super) const OAUTH_PICK_FIELDS: &str =
    "_docID credential_id node_did provider enabled account_ref connected_at provider_account_key";

pub fn oauth_credential_by_id_query(credential_id: &str) -> String {
    let credential_id = crate::graphql::escape_graphql_string(credential_id);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                limit: 1
            ) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub(super) const SET_ONCE_FIELDS: &[&str] = &["node_did", "account_ref", "connected_at", "label"];

pub fn oauth_credential_upsert_mutation(credential: &OAuthCredential) -> String {
    let fields = oauth_credential_input_fields(credential);
    let add_input = render_oauth_input(&fields, &[]);
    // `node_did` is immutable, so the update branch must omit it on this DefraDB pin.
    let update_input = render_oauth_input(&fields, SET_ONCE_FIELDS);
    let credential_id = crate::graphql::escape_graphql_string(&credential.credential_id);
    format!(
        r#"mutation {{
            upsert_OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                add: {{
                    credential_id: "{credential_id}",
                    {add_input}
                }},
                update: {{
                    {update_input}
                }}
            ) {{ _docID }}
        }}"#
    )
}

pub(super) fn enabled_oauth_credentials_query(
    node_did: &str,
    provider: &str,
    fields: &str,
) -> String {
    let node_did = crate::graphql::escape_graphql_string(node_did);
    let provider = crate::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    provider: {{ _eq: "{provider}" }},
                    enabled: {{ _eq: true }}
                }}
            ) {{
                {fields}
            }}
        }}"#
    )
}

pub(super) fn pick_row_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    Ok(OAuthCredential {
        doc_id: row.doc_id,
        credential_id: row.credential_id,
        node_did: required(row.node_did, "node_did")?,
        provider: required(row.provider, "provider")?,
        access_token: String::new(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: DateTime::default(),
        last_refresh: None,
        enabled: row.enabled.unwrap_or(true),
        account_ref: clean_optional(row.account_ref),
        connected_at: parse_optional_datetime(row.connected_at, "connected_at")?,
        provider_account_key: clean_optional(row.provider_account_key),
        label: None,
    })
}

pub fn oauth_credentials_for_node_query(node_did: &str) -> String {
    let node_did = crate::graphql::escape_graphql_string(node_did);
    format!(
        r#"query {{
            OAuthCredential(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credential_by_doc_id_query(doc_id: &str) -> String {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    format!(
        r#"query {{
            OAuthCredential(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub(super) fn oauth_credentials_of_provider_query(node_did: &str, provider: &str) -> String {
    let node_did = crate::graphql::escape_graphql_string(node_did);
    let provider = crate::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                provider: {{ _eq: "{provider}" }}
            }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credentials_from_response(response: &Value) -> Vec<Result<OAuthCredential>> {
    gents_protocol::graphql::graphql_rows_from_response(response, "OAuthCredential")
        .into_iter()
        .map(oauth_credential_from_value)
        .collect()
}

pub(super) fn oauth_credential_input_fields(
    credential: &OAuthCredential,
) -> Vec<(&'static str, String)> {
    let field = |name: &str, value: &str| {
        format!(
            r#"{name}: "{}""#,
            crate::graphql::escape_graphql_string(value)
        )
    };
    let datetime_field = |name: &str, value: Option<DateTime<Utc>>| {
        value
            .map(|value| {
                format!(
                    r#"{name}: "{}""#,
                    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                )
            })
            .unwrap_or_else(|| format!("{name}: null"))
    };
    vec![
        ("node_did", field("node_did", &credential.node_did)),
        ("provider", field("provider", &credential.provider)),
        (
            "access_token",
            field("access_token", &credential.access_token),
        ),
        (
            "refresh_token",
            field("refresh_token", &credential.refresh_token),
        ),
        (
            "id_token",
            gents_protocol::graphql::nullable_string_field(
                "id_token",
                credential.id_token.as_deref(),
            ),
        ),
        (
            "account_id",
            gents_protocol::graphql::nullable_string_field(
                "account_id",
                credential.account_id.as_deref(),
            ),
        ),
        (
            "chatgpt_plan_type",
            gents_protocol::graphql::nullable_string_field(
                "chatgpt_plan_type",
                credential.chatgpt_plan_type.as_deref(),
            ),
        ),
        (
            "is_fedramp",
            format!(
                "is_fedramp: {}",
                gents_protocol::graphql::graphql_bool_literal(credential.is_fedramp)
            ),
        ),
        (
            "access_token_expires_at",
            datetime_field(
                "access_token_expires_at",
                Some(credential.access_token_expires_at),
            ),
        ),
        (
            "last_refresh",
            datetime_field("last_refresh", credential.last_refresh),
        ),
        (
            "enabled",
            format!(
                "enabled: {}",
                gents_protocol::graphql::graphql_bool_literal(credential.enabled)
            ),
        ),
        (
            "account_ref",
            gents_protocol::graphql::nullable_string_field(
                "account_ref",
                credential.account_ref.as_deref(),
            ),
        ),
        (
            "connected_at",
            datetime_field("connected_at", credential.connected_at),
        ),
        (
            "provider_account_key",
            gents_protocol::graphql::nullable_string_field(
                "provider_account_key",
                credential.provider_account_key.as_deref(),
            ),
        ),
        (
            "label",
            gents_protocol::graphql::nullable_string_field("label", credential.label.as_deref()),
        ),
    ]
}

pub(super) fn render_oauth_input(fields: &[(&'static str, String)], exclude: &[&str]) -> String {
    fields
        .iter()
        .filter(|(name, _)| !exclude.contains(name))
        .map(|(_, rendered)| rendered.as_str())
        .collect::<Vec<_>>()
        .join(",\n                    ")
}

pub(super) fn oauth_credential_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    let access_token = required(row.access_token, "access_token")?;
    let refresh_token = required(row.refresh_token, "refresh_token")?;
    Ok(OAuthCredential {
        doc_id: row.doc_id,
        credential_id: row.credential_id,
        node_did: required(row.node_did, "node_did")?,
        provider: required(row.provider, "provider")?,
        access_token,
        refresh_token,
        id_token: clean_optional(row.id_token),
        account_id: clean_optional(row.account_id),
        chatgpt_plan_type: clean_optional(row.chatgpt_plan_type),
        is_fedramp: row.is_fedramp.unwrap_or(false),
        access_token_expires_at: parse_required_datetime(
            row.access_token_expires_at,
            "access_token_expires_at",
        )?,
        last_refresh: parse_optional_datetime(row.last_refresh, "last_refresh")?,
        enabled: row.enabled.unwrap_or(true),
        account_ref: clean_optional(row.account_ref),
        connected_at: parse_optional_datetime(row.connected_at, "connected_at")?,
        provider_account_key: clean_optional(row.provider_account_key),
        label: clean_optional(row.label),
    })
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    clean_optional(value).ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(value)
    })
}

fn parse_required_datetime(value: Option<String>, field: &str) -> Result<DateTime<Utc>> {
    parse_optional_datetime(value, field)?
        .ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn parse_optional_datetime(value: Option<String>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .with_context(|| format!("parsing OAuthCredential {field} timestamp {value}"))
        })
        .transpose()
}

const REFRESH_FIELDS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "account_id",
    "chatgpt_plan_type",
    "is_fedramp",
    "access_token_expires_at",
    "last_refresh",
];

pub(super) fn refresh_update_mutation(
    credential: &OAuthCredential,
    stored_refresh_token: &str,
    key_filled: bool,
) -> String {
    let document = match &credential.doc_id {
        Some(doc_id) => format!(
            r#"_docID: {{ _eq: "{}" }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ),
        None => format!(
            r#"credential_id: {{ _eq: "{}" }}"#,
            crate::graphql::escape_graphql_string(&credential.credential_id)
        ),
    };
    let fields: Vec<_> = oauth_credential_input_fields(credential)
        .into_iter()
        .filter(|(name, _)| {
            REFRESH_FIELDS.contains(name) || (key_filled && *name == "provider_account_key")
        })
        .collect();
    format!(
        r#"mutation {{
            update_OAuthCredential(
                filter: {{ {document}, refresh_token: {{ _eq: "{}" }} }},
                input: {{
                    {}
                }}
            ) {{ _docID }}
        }}"#,
        crate::graphql::escape_graphql_string(stored_refresh_token),
        render_oauth_input(&fields, &[]),
    )
}
