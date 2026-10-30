use std::collections::BTreeSet;

pub mod import;
pub use import::{parse_skill_md, SkillFrontmatter};

#[derive(Debug, Clone)]
pub struct Skill {
    pub skill_id: String,
    pub node_did: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub source_directory: Option<String>,
    pub tool_refs: Vec<String>,
    pub display_name: Option<String>,
    pub enabled: bool,
}

pub fn effective_skills<'a>(
    skills: &'a [Skill],
    node_did: &str,
    skill_ids: &[String],
) -> Vec<&'a Skill> {
    let selected: BTreeSet<&str> = skill_ids.iter().map(String::as_str).collect();
    skills
        .iter()
        .filter(|skill| {
            skill.node_did == node_did
                && skill.enabled
                && selected.contains(skill.skill_id.as_str())
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct SkillToolCeiling {
    names: BTreeSet<String>,
    mcp_unrestricted: bool,
}

impl SkillToolCeiling {
    pub fn new(names: BTreeSet<String>, mcp_unrestricted: bool) -> Self {
        Self {
            names,
            mcp_unrestricted,
        }
    }

    pub fn allows(&self, tool: &str) -> bool {
        self.mcp_unrestricted || self.names.contains(tool)
    }
}

pub fn skill_tool_ceiling(
    tool_names: impl IntoIterator<Item = String>,
    allowed_mcp_service_ids: &[String],
    mcp_enabled: bool,
) -> SkillToolCeiling {
    let mut names: BTreeSet<String> = tool_names.into_iter().collect();
    names.extend(allowed_mcp_service_ids.iter().cloned());
    let mcp_unrestricted = mcp_enabled && allowed_mcp_service_ids.is_empty();
    SkillToolCeiling::new(names, mcp_unrestricted)
}

pub fn skill_tools<'a>(skill: &'a Skill, ceiling: &SkillToolCeiling) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| ceiling.allows(tool))
        .collect()
}

pub fn missing_tool_refs<'a>(skill: &'a Skill, ceiling: &SkillToolCeiling) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| !ceiling.allows(tool))
        .collect()
}

fn skill_label(skill: &Skill) -> &str {
    if let Some(display_name) = skill
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return display_name;
    }
    if skill.name.trim().is_empty() {
        skill.skill_id.as_str()
    } else {
        skill.name.as_str()
    }
}

pub fn render_skill_catalog(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Skills\n\nThese skills are available. Before acting, scan them; if one is relevant \
         to the task, call the `load_skill` tool with its name and follow the returned \
         instructions. Skip skills only when none are relevant.\n",
    );
    for skill in skills {
        out.push_str(&format!(
            "\n- {}: {}",
            skill_label(skill),
            skill.description
        ));
    }
    Some(out)
}

pub fn render_activated_skill(skill: &Skill, ceiling: &SkillToolCeiling) -> String {
    let mut out = format!("Skill: {}\n\n", skill_label(skill));
    if let Some(directory) = &skill.source_directory {
        out.push_str(&format!(
            "Source directory: {}. Resolve supporting-file paths relative to this directory, not the working directory. This location grants no file or execution permissions; use your existing tools and report unavailable paths.\n\n",
            serde_json::to_string(directory).expect("serialize skill directory")
        ));
    }
    out.push_str(&skill.instructions);
    let missing = missing_tool_refs(skill, ceiling);
    if !missing.is_empty() {
        out.push_str(&format!(
            "\n\nNote: this skill references tools that are not available to this agent \
             and cannot be used: {}.",
            missing.join(", ")
        ));
    }
    out
}

pub fn find_skill<'a>(skills: &'a [Skill], needle: &str) -> Option<&'a Skill> {
    let needle = needle.trim();
    let display_name = |skill: &Skill| {
        skill
            .display_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    skills
        .iter()
        .find(|skill| {
            skill.name == needle || skill.skill_id == needle || display_name(skill) == needle
        })
        .or_else(|| {
            skills.iter().find(|skill| {
                skill.name.eq_ignore_ascii_case(needle)
                    || skill.skill_id.eq_ignore_ascii_case(needle)
                    || display_name(skill).eq_ignore_ascii_case(needle)
            })
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSlashSkillSelection {
    pub selected_skill_ids: Vec<String>,
    pub prompt: String,
}

pub fn prompt_slash_skill_selection(prompt: &str) -> PromptSlashSkillSelection {
    let mut selected = Vec::new();
    let mut body_lines = Vec::new();
    let mut saw_selector = false;

    let mut lines = prompt.lines();
    while let Some(line) = lines.next() {
        if line.trim().is_empty() && !saw_selector {
            continue;
        }

        let Some(selector) = leading_slash_skill_selector(line) else {
            if !saw_selector {
                return PromptSlashSkillSelection {
                    selected_skill_ids: Vec::new(),
                    prompt: prompt.to_string(),
                };
            }
            body_lines.push(line.to_string());
            body_lines.extend(lines.map(ToOwned::to_owned));
            break;
        };

        if !selected
            .iter()
            .any(|existing| existing == &selector.skill_id)
        {
            selected.push(selector.skill_id);
        }
        if !selector.remainder.is_empty() {
            body_lines.push(selector.remainder);
        }
        saw_selector = true;
    }

    if !saw_selector {
        return PromptSlashSkillSelection {
            selected_skill_ids: Vec::new(),
            prompt: prompt.to_string(),
        };
    }

    PromptSlashSkillSelection {
        selected_skill_ids: selected,
        prompt: body_lines.join("\n"),
    }
}

pub fn selected_skill_ids_from_prompt_slash_commands(prompt: &str) -> Vec<String> {
    prompt_slash_skill_selection(prompt).selected_skill_ids
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlashSkillSelector {
    skill_id: String,
    remainder: String,
}

fn leading_slash_skill_selector(line: &str) -> Option<SlashSkillSelector> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(rest.len());
    let command = &rest[..end];
    if command.is_empty() {
        return None;
    }

    let after_command = &rest[end..];
    if command.eq_ignore_ascii_case("skill") {
        let remainder = after_command.trim_start();
        return parse_skill_command_argument(remainder);
    }

    if is_reserved_client_slash_command(command) {
        return None;
    }

    if rest[end..].starts_with('/') {
        return None;
    }

    Some(SlashSkillSelector {
        skill_id: command.to_string(),
        remainder: after_command.trim_start().to_string(),
    })
}

fn parse_skill_command_argument(input: &str) -> Option<SlashSkillSelector> {
    let end = input
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(input.len());
    let skill_id = &input[..end];
    if skill_id.is_empty() || input[end..].starts_with('/') {
        return None;
    }
    Some(SlashSkillSelector {
        skill_id: skill_id.to_string(),
        remainder: input[end..].trim_start().to_string(),
    })
}

fn is_reserved_client_slash_command(command: &str) -> bool {
    matches!(
        command.to_ascii_lowercase().as_str(),
        "agent"
            | "apps"
            | "approve"
            | "btw"
            | "clean"
            | "clear"
            | "compact"
            | "copy"
            | "debug-config"
            | "debug-m-drop"
            | "debug-m-update"
            | "diff"
            | "exit"
            | "experimental"
            | "feedback"
            | "fork"
            | "goal"
            | "hooks"
            | "ide"
            | "init"
            | "keymap"
            | "logout"
            | "mcp"
            | "memories"
            | "mention"
            | "model"
            | "new"
            | "permissions"
            | "personality"
            | "pet"
            | "pets"
            | "plan"
            | "plugins"
            | "ps"
            | "quit"
            | "raw"
            | "realtime"
            | "rename"
            | "resume"
            | "review"
            | "rollout"
            | "sandbox-add-read-dir"
            | "settings"
            | "setup-default-sandbox"
            | "side"
            | "skills"
            | "status"
            | "statusline"
            | "stop"
            | "agents"
            | "test-approval"
            | "theme"
            | "title"
            | "vim"
    )
}

#[derive(Debug)]
pub struct LoadSkillError(pub String);

impl std::fmt::Display for LoadSkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoadSkillError {}

#[derive(Debug, serde::Deserialize)]
pub struct LoadSkillArgs {
    pub name: String,
}

#[derive(Clone)]
pub struct LoadSkillTool {
    skills: Vec<Skill>,
    ceiling: SkillToolCeiling,
}

impl LoadSkillTool {
    pub fn new(skills: Vec<Skill>, ceiling: SkillToolCeiling) -> Self {
        Self { skills, ceiling }
    }
}

impl crate::inference::llm::tool::Tool for LoadSkillTool {
    const NAME: &'static str = "load_skill";
    type Error = LoadSkillError;
    type Args = LoadSkillArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> crate::inference::llm::tool::ToolDefinition {
        crate::inference::llm::tool::ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Load a skill's full instructions by name (or skill_id), then follow \
                them for the task. Choose a skill from the Skills catalog in your system prompt."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name or skill_id from the Skills catalog."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        match find_skill(&self.skills, &args.name) {
            Some(skill) => Ok(render_activated_skill(skill, &self.ceiling)),
            None => {
                let available = self
                    .skills
                    .iter()
                    .map(skill_label)
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok(format!(
                    "No skill named {:?}. Available skills: {available}.",
                    args.name.trim()
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str, node_did: &str, tool_refs: &[&str]) -> Skill {
        Skill {
            skill_id: id.to_string(),
            node_did: node_did.to_string(),
            name: format!("{id}-name"),
            description: format!("{id}-desc"),
            instructions: format!("{id}-instructions"),
            source_directory: None,
            tool_refs: tool_refs.iter().map(|s| s.to_string()).collect(),
            display_name: None,
            enabled: true,
        }
    }

    fn ceiling(tools: &[&str]) -> SkillToolCeiling {
        SkillToolCeiling::new(tools.iter().map(|s| s.to_string()).collect(), false)
    }

    #[test]
    fn source_directory_informs_references_without_granting_tools() {
        let mut imported = skill("review", "did:p", &["read_file", "bash"]);
        let ceiling = ceiling(&["read_file"]);
        let before = skill_tools(&imported, &ceiling)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        imported.source_directory = Some("/skills/code review".into());
        assert_eq!(skill_tools(&imported, &ceiling), before);
        let rendered = render_activated_skill(&imported, &ceiling);
        assert!(rendered.contains("/skills/code review"));
        assert!(rendered.contains("relative to this directory"));
        assert!(rendered.contains(&imported.instructions));
        assert_eq!(missing_tool_refs(&imported, &ceiling), vec!["bash"]);
    }

    #[test]
    fn leading_slash_commands_select_skills_and_strip_control_syntax() {
        let selection = prompt_slash_skill_selection(
            "\n/vuln-scan /work --focus parser\n/triage\nRun the task.",
        );
        assert_eq!(selection.selected_skill_ids, vec!["vuln-scan", "triage"]);
        assert_eq!(selection.prompt, "/work --focus parser\nRun the task.");

        assert!(
            selected_skill_ids_from_prompt_slash_commands("Run /vuln-scan as plain text",)
                .is_empty()
        );
        assert!(
            selected_skill_ids_from_prompt_slash_commands("/work/entry.c is an absolute path",)
                .is_empty()
        );

        let explicit = prompt_slash_skill_selection("/skill review inspect the diff");
        assert_eq!(explicit.selected_skill_ids, vec!["review"]);
        assert_eq!(explicit.prompt, "inspect the diff");

        let codex_command = prompt_slash_skill_selection("/review inspect the diff");
        assert!(codex_command.selected_skill_ids.is_empty());
        assert_eq!(codex_command.prompt, "/review inspect the diff");
    }
}
