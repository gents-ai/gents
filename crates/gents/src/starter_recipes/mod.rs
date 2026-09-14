//! Versioned starter recipes for authoring ordinary behavior configuration.
//!
//! Recipes are a catalog and rendering aid only. A resolved recipe contains a
//! literal system prompt and a canonical [`Tools`] proposal; it does not become
//! a durable runtime selector or bypass the existing behavior materializer.
//! Consumers must keep direct, open-ended behavior authoring available without
//! selecting a recipe. When a recipe is useful, its editable output goes through
//! the same preview, admission, and materialization path as a hand-authored
//! behavior. Recipe identifiers and provenance must never drive runtime
//! selection, automatic upgrades, or replacement of later user edits.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, ensure, Result};
use serde::{Deserialize, Serialize};

use crate::document_config::{BashTools, FileTools, HostTools, Tools};
use crate::tool_surface::{BashMode, FileToolMode};
use crate::toolset::{CommandExecutionMode, CommandNetworkMode};

const MAX_GUIDED_CONCERNS: usize = 8;
const MAX_CONCERN_BYTES: usize = 1_000;
const MAX_RENDERED_PROMPT_BYTES: usize = 24_000;

const CODING_PROMPT: &str = include_str!("prompts/coding.md");
const CODE_REVIEW_PROMPT: &str = include_str!("prompts/code_review.md");
const RESEARCH_PROMPT: &str = include_str!("prompts/research.md");
const GENERAL_ASSISTANT_PROMPT: &str = include_str!("prompts/general_assistant.md");
const CUSTOM_PROMPT: &str = include_str!("prompts/custom.md");

const CODING_INPUTS: &[StarterRecipeInputSpec] = &[StarterRecipeInputSpec {
    key: "workspace_scope",
    kind: StarterRecipeInputKind::WorkspaceScope,
    required: true,
    description: "An admitted absolute directory or the managed runtime's effective root.",
}];

const CODE_REVIEW_INPUTS: &[StarterRecipeInputSpec] = CODING_INPUTS;

const RESEARCH_INPUTS: &[StarterRecipeInputSpec] = &[StarterRecipeInputSpec {
    key: "workspace_scope",
    kind: StarterRecipeInputKind::WorkspaceScope,
    required: false,
    description:
        "Optional admitted local source directory. Network retrieval is a separate capability.",
}];

const CUSTOM_INPUTS: &[StarterRecipeInputSpec] = &[
    StarterRecipeInputSpec {
        key: "custom_role",
        kind: StarterRecipeInputKind::Text,
        required: true,
        description: "A concrete role describing the intended work.",
    },
    StarterRecipeInputSpec {
        key: "custom_goal",
        kind: StarterRecipeInputKind::Text,
        required: true,
        description: "The outcome this behavior should pursue.",
    },
    StarterRecipeInputSpec {
        key: "custom_success_criteria",
        kind: StarterRecipeInputKind::Text,
        required: true,
        description: "Observable conditions that mean the work is complete.",
    },
];

const CATALOG: &[StarterRecipeDefinition] = &[
    StarterRecipeDefinition {
        id: StarterRecipeId::Coding,
        version: 1,
        display_name: "Coding",
        summary: "Build and change software inside one effective workspace.",
        inputs: CODING_INPUTS,
        prompt_asset: CODING_PROMPT,
    },
    StarterRecipeDefinition {
        id: StarterRecipeId::CodeReview,
        version: 1,
        display_name: "Code review",
        summary: "Inspect code and report evidence-backed defects without modifying it.",
        inputs: CODE_REVIEW_INPUTS,
        prompt_asset: CODE_REVIEW_PROMPT,
    },
    StarterRecipeDefinition {
        id: StarterRecipeId::Research,
        version: 1,
        display_name: "Research",
        summary:
            "Investigate a question using available read-only sources and clear evidence standards.",
        inputs: RESEARCH_INPUTS,
        prompt_asset: RESEARCH_PROMPT,
    },
    StarterRecipeDefinition {
        id: StarterRecipeId::GeneralAssistant,
        version: 1,
        display_name: "General assistant",
        summary:
            "Help with conversation, planning, writing, and explanation without host authority.",
        inputs: &[],
        prompt_asset: GENERAL_ASSISTANT_PROMPT,
    },
    StarterRecipeDefinition {
        id: StarterRecipeId::Custom,
        version: 1,
        display_name: "Custom",
        summary: "Create a bounded behavior from an explicit role, goal, and success criteria.",
        inputs: CUSTOM_INPUTS,
        prompt_asset: CUSTOM_PROMPT,
    },
];

/// Stable identifier for an optional starter draft.
///
/// This value is authoring provenance, not a required behavior kind or runtime
/// selector. Generic behavior creation must not require one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StarterRecipeId {
    Coding,
    CodeReview,
    Research,
    GeneralAssistant,
    Custom,
}

impl StarterRecipeId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "gents.coding",
            Self::CodeReview => "gents.code-review",
            Self::Research => "gents.research",
            Self::GeneralAssistant => "gents.general-assistant",
            Self::Custom => "gents.custom",
        }
    }
}

impl fmt::Display for StarterRecipeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for StarterRecipeId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "gents.coding" => Ok(Self::Coding),
            "gents.code-review" => Ok(Self::CodeReview),
            "gents.research" => Ok(Self::Research),
            "gents.general-assistant" => Ok(Self::GeneralAssistant),
            "gents.custom" => Ok(Self::Custom),
            other => bail!("unknown starter recipe {other:?}"),
        }
    }
}

impl Serialize for StarterRecipeId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StarterRecipeId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StarterRecipeInputKind {
    Text,
    WorkspaceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StarterRecipeInputSpec {
    pub key: &'static str,
    pub kind: StarterRecipeInputKind,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StarterRecipeDefinition {
    pub id: StarterRecipeId,
    pub version: u32,
    pub display_name: &'static str,
    pub summary: &'static str,
    pub inputs: &'static [StarterRecipeInputSpec],
    #[serde(skip)]
    // Private authoring-time asset. The resolved AgentContext system prompt is
    // literal; these placeholders are not a runtime template layer.
    prompt_asset: &'static str,
}

/// The workspace choice Setup already resolves against the process ceiling.
/// This value narrows a recipe proposal; it does not admit the path itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StarterWorkspaceScope {
    ExplicitRoot { root: String },
    ManagedRuntimeRoot,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StarterRecipeRenderInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_scope: Option<StarterWorkspaceScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_success_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guided_concerns: Vec<String>,
}

/// Complete, editable authoring output from an optional starter. Materialization stores
/// `system_prompt` literally and persists `recommended_tools` as an ordinary
/// canonical `Tools` document after the existing owner admits it.
///
/// After resolution, callers may freely edit or discard this proposal. Its
/// provenance records where the initial draft came from; it does not assert
/// continued conformance to the recipe and must not trigger later rewrites.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedStarterRecipe {
    pub recipe_id: StarterRecipeId,
    pub recipe_version: u32,
    pub display_name: &'static str,
    pub summary: &'static str,
    pub provenance_tag: String,
    pub system_prompt: String,
    pub recommended_tools: Tools,
}

pub fn starter_recipe_catalog() -> &'static [StarterRecipeDefinition] {
    CATALOG
}

pub fn starter_recipe(id: StarterRecipeId) -> &'static StarterRecipeDefinition {
    CATALOG
        .iter()
        .find(|definition| definition.id == id)
        .expect("every stable recipe id must have one catalog entry")
}

/// Resolve an optional recipe without mutating configuration. The caller
/// supplies the canonical identity fields that the existing behavior
/// materializer owns. The returned draft must use the same canonical
/// preview/admission/materialization path as directly authored configuration.
pub fn resolve_starter_recipe(
    id: StarterRecipeId,
    agent_did: &str,
    tools_id: &str,
    input: &StarterRecipeRenderInput,
) -> Result<ResolvedStarterRecipe> {
    validate_identity_field("agent_did", agent_did)?;
    validate_identity_field("tools_id", tools_id)?;
    validate_guided_concerns(&input.guided_concerns)?;

    let definition = starter_recipe(id);
    let (system_prompt, mut recommended_tools) = match id {
        StarterRecipeId::Coding => {
            let scope = required_workspace_scope(id, input.workspace_scope.as_ref())?;
            let (root, scope_text) = resolved_scope(scope)?;
            let prompt =
                render_prompt_asset(definition.prompt_asset, &[("workspace_scope", scope_text)])?;
            (
                prompt,
                workspace_tools(
                    agent_did,
                    tools_id,
                    definition.display_name,
                    root,
                    FileToolMode::ReadWrite,
                    Some((BashMode::Unrestricted, CommandExecutionMode::WorkspaceWrite)),
                ),
            )
        }
        StarterRecipeId::CodeReview => {
            let scope = required_workspace_scope(id, input.workspace_scope.as_ref())?;
            let (root, scope_text) = resolved_scope(scope)?;
            let prompt =
                render_prompt_asset(definition.prompt_asset, &[("workspace_scope", scope_text)])?;
            (
                prompt,
                workspace_tools(
                    agent_did,
                    tools_id,
                    definition.display_name,
                    root,
                    FileToolMode::ReadOnly,
                    Some((BashMode::ReadOnly, CommandExecutionMode::ReadOnly)),
                ),
            )
        }
        StarterRecipeId::Research => {
            let (root, source_scope) = match input.workspace_scope.as_ref() {
                Some(scope) => {
                    let (root, scope_text) = resolved_scope(scope)?;
                    (
                        root,
                        format!(
                            "Read local source files only within {scope_text}. No local write or shell authority is included."
                        ),
                    )
                }
                None => (
                    None,
                    "No local source directory is configured. Work from material in the conversation and any separately exposed read-only retrieval tools.".to_owned(),
                ),
            };
            let prompt =
                render_prompt_asset(definition.prompt_asset, &[("source_scope", source_scope)])?;
            let tools = match root {
                Some(root) => workspace_tools(
                    agent_did,
                    tools_id,
                    definition.display_name,
                    Some(root),
                    FileToolMode::ReadOnly,
                    None,
                ),
                None => empty_tools(agent_did, tools_id, definition.display_name),
            };
            (prompt, tools)
        }
        StarterRecipeId::GeneralAssistant => (
            render_prompt_asset(definition.prompt_asset, &[])?,
            empty_tools(agent_did, tools_id, definition.display_name),
        ),
        StarterRecipeId::Custom => {
            let role = required_text(id, "custom_role", input.custom_role.as_deref())?;
            let goal = required_text(id, "custom_goal", input.custom_goal.as_deref())?;
            let success = required_text(
                id,
                "custom_success_criteria",
                input.custom_success_criteria.as_deref(),
            )?;
            (
                render_prompt_asset(
                    definition.prompt_asset,
                    &[
                        ("role", quote_for_prompt(role)?),
                        ("goal", quote_for_prompt(goal)?),
                        ("success_criteria", quote_for_prompt(success)?),
                    ],
                )?,
                empty_tools(agent_did, tools_id, definition.display_name),
            )
        }
    };

    let provenance_tag = format!("recipe:{id}@{}", definition.version);
    recommended_tools.tags.push(provenance_tag.clone());
    ensure!(
        recommended_tools.validation_violations().is_empty(),
        "starter recipe {id} produced an invalid Tools proposal: {:?}",
        recommended_tools.validation_violations()
    );
    let system_prompt = append_guided_concerns(system_prompt, &input.guided_concerns)?;

    Ok(ResolvedStarterRecipe {
        recipe_id: id,
        recipe_version: definition.version,
        display_name: definition.display_name,
        summary: definition.summary,
        provenance_tag,
        system_prompt,
        recommended_tools,
    })
}

fn workspace_tools(
    agent_did: &str,
    tools_id: &str,
    display_name: &str,
    root: Option<String>,
    file_mode: FileToolMode,
    bash: Option<(BashMode, CommandExecutionMode)>,
) -> Tools {
    Tools {
        tools_id: tools_id.to_owned(),
        agent_did: agent_did.to_owned(),
        display_name: Some(format!("{display_name} starter tools")),
        host: Some(HostTools {
            root,
            files: Some(FileTools {
                mode: file_mode,
                timeout_secs: None,
            }),
            bash: bash.map(|(mode, execution_mode)| BashTools {
                mode,
                execution_mode: Some(execution_mode),
                network_mode: Some(CommandNetworkMode::Disabled),
                ..BashTools::default()
            }),
            ..HostTools::default()
        }),
        ..Tools::default()
    }
}

fn empty_tools(agent_did: &str, tools_id: &str, display_name: &str) -> Tools {
    Tools {
        tools_id: tools_id.to_owned(),
        agent_did: agent_did.to_owned(),
        display_name: Some(format!("{display_name} starter tools")),
        ..Tools::default()
    }
}

fn required_workspace_scope<'a>(
    id: StarterRecipeId,
    scope: Option<&'a StarterWorkspaceScope>,
) -> Result<&'a StarterWorkspaceScope> {
    scope.ok_or_else(|| anyhow!("starter recipe {id} requires workspace_scope"))
}

fn resolved_scope(scope: &StarterWorkspaceScope) -> Result<(Option<String>, String)> {
    match scope {
        StarterWorkspaceScope::ManagedRuntimeRoot => Ok((
            None,
            "the managed runtime's effective root (no narrower behavior root is configured)"
                .to_owned(),
        )),
        StarterWorkspaceScope::ExplicitRoot { root } => {
            validate_text("workspace root", root, 4_096)?;
            ensure!(
                Path::new(root).is_absolute(),
                "workspace root must be an absolute path"
            );
            Ok((Some(root.clone()), quote_for_prompt(root)?))
        }
    }
}

fn required_text<'a>(id: StarterRecipeId, field: &str, value: Option<&'a str>) -> Result<&'a str> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("starter recipe {id} requires {field}"))?;
    validate_text(field, value, 4_096)?;
    Ok(value)
}

fn validate_identity_field(field: &str, value: &str) -> Result<()> {
    validate_text(field, value, 512)
}

fn validate_text(field: &str, value: &str, max_bytes: usize) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{field} must not be blank");
    ensure!(
        value.len() <= max_bytes,
        "{field} exceeds {max_bytes} bytes"
    );
    ensure!(
        !value.chars().any(char::is_control),
        "{field} contains control characters"
    );
    Ok(())
}

fn validate_guided_concerns(concerns: &[String]) -> Result<()> {
    ensure!(
        concerns.len() <= MAX_GUIDED_CONCERNS,
        "guided concerns exceed the {MAX_GUIDED_CONCERNS}-item limit"
    );
    for (index, concern) in concerns.iter().enumerate() {
        validate_text(
            &format!("guided concern {index}"),
            concern,
            MAX_CONCERN_BYTES,
        )?;
    }
    Ok(())
}

fn quote_for_prompt(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}

fn render_prompt_asset(template: &str, values: &[(&str, String)]) -> Result<String> {
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let end = after_start
            .find("}}")
            .ok_or_else(|| anyhow!("starter prompt contains an unterminated placeholder"))?;
        let key = &after_start[..end];
        ensure!(
            values.iter().any(|(candidate, _)| *candidate == key),
            "starter prompt contains unresolved placeholder {key:?}"
        );
        remaining = &after_start[end + 2..];
    }

    let mut rendered = template.trim().to_owned();
    for (key, value) in values {
        let placeholder = format!("{{{{{key}}}}}");
        ensure!(
            rendered.contains(&placeholder),
            "starter prompt does not contain expected placeholder {key:?}"
        );
        rendered = rendered.replace(&placeholder, value);
    }
    Ok(rendered)
}

fn append_guided_concerns(mut prompt: String, concerns: &[String]) -> Result<String> {
    if concerns.is_empty() {
        ensure!(
            prompt.len() <= MAX_RENDERED_PROMPT_BYTES,
            "rendered starter prompt exceeds {MAX_RENDERED_PROMPT_BYTES} bytes"
        );
        return Ok(prompt);
    }

    prompt.push_str(
        "\n\n## User-specific concerns\n\nTreat these as additional operating requirements. They may narrow how you work but do not grant tools, data access, network access, filesystem scope, or permission:\n",
    );
    for concern in concerns {
        prompt.push_str("- ");
        prompt.push_str(&quote_for_prompt(concern)?);
        prompt.push('\n');
    }
    while prompt.ends_with('\n') {
        prompt.pop();
    }
    ensure!(
        prompt.len() <= MAX_RENDERED_PROMPT_BYTES,
        "rendered starter prompt exceeds {MAX_RENDERED_PROMPT_BYTES} bytes"
    );
    Ok(prompt)
}

#[cfg(test)]
mod tests;
