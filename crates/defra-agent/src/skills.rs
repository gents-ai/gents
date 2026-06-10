//! Runtime skill resolution — the executable realization of the privilege
//! algebra proved in `proofs/Proofs/Skills.lean`.
//!
//! A [`Skill`] declares the tools it *depends on* (`tool_refs`); it never
//! *grants* them (decision D3, Codex-faithful). [`effective_skills`] computes
//! the per-behavior candidate set (decision D5: scope-on-skill inheritance +
//! `skill_refs`/`skill_excludes`). [`skill_tools`] intersects a skill's
//! declared refs with the behavior's resolved tool ceiling and degrades when a
//! dep is missing, so activation can never widen the tool surface beyond the
//! ceiling — the executable counterpart of `Skills.activation_subset_ceiling`.
//!
//! This module is pure (no DB / request plumbing). The runtime wiring that
//! loads `Skill` documents and feeds these results into `prompt.rs` and
//! `tool_surface` is layered on top of it.

use std::collections::BTreeSet;

/// Activation scope of a skill (decision D5). Mirrors the `Scope` inductive in
/// `proofs/Proofs/Skills.lean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// Inherited by every behavior of the owning principal.
    Principal,
    /// A candidate only for behaviors that opt in via `skill_refs`.
    Behavior,
}

impl SkillScope {
    /// Parse the on-document `scope` string. Returns `None` for unknown values
    /// (apply-time validation rejects those before they reach the runtime).
    pub fn parse(value: &str) -> Option<SkillScope> {
        match value.trim() {
            "principal" => Some(SkillScope::Principal),
            "behavior" => Some(SkillScope::Behavior),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SkillScope::Principal => "principal",
            SkillScope::Behavior => "behavior",
        }
    }
}

/// Runtime view of a `Skill` document, owned by a principal (`agent_did`).
#[derive(Debug, Clone)]
pub struct Skill {
    pub skill_id: String,
    pub agent_did: String,
    pub scope: SkillScope,
    pub name: String,
    pub description: String,
    pub instructions: String,
    /// Declared tool dependencies (host tool kinds, mcp service ids, cli names).
    /// Advisory — intersected with the behavior ceiling, never granted (D3).
    pub tool_refs: Vec<String>,
    /// Optional UI display name (from `agents/openai.yaml` `interface.display_name`).
    /// Preferred over `name` for the catalog/activation label when present; opaque
    /// to privilege (decision: UI metadata, not load-bearing).
    pub display_name: Option<String>,
    pub enabled: bool,
}

/// The D5 effective candidate set for a behavior. Mirrors `Skills.candidates`
/// in `proofs/Proofs/Skills.lean`:
///
/// `{ s : s.agent_did == principal ∧ s.enabled ∧ (s.scope == principal ∨ s.id ∈ skill_refs) } − skill_excludes`
///
/// A principal-scoped skill of the owning principal is inherited by every
/// behavior; a behavior-scoped skill is a candidate only when listed in
/// `skill_refs`; `skill_excludes` removes inherited principal-scoped skills.
pub fn effective_skills<'a>(
    skills: &'a [Skill],
    behavior_principal: &str,
    skill_refs: &[String],
    skill_excludes: &[String],
) -> Vec<&'a Skill> {
    let refs: BTreeSet<&str> = skill_refs.iter().map(String::as_str).collect();
    let excludes: BTreeSet<&str> = skill_excludes.iter().map(String::as_str).collect();
    skills
        .iter()
        .filter(|skill| {
            skill.agent_did == behavior_principal
                && skill.enabled
                && (skill.scope == SkillScope::Principal || refs.contains(skill.skill_id.as_str()))
                && !excludes.contains(skill.skill_id.as_str())
        })
        .collect()
}

/// The D3 tool ceiling a skill's `tool_refs` are evaluated against.
///
/// `names` is the behavior's built tool names UNION its explicitly-allowed MCP
/// service ids (MCP services are callable via `call_tool` but never appear in
/// the built tool-name list). `mcp_unrestricted` is the default "any MCP service
/// allowed" case — meta tools enabled AND an EMPTY allowlist (see
/// `meta_tools::mcp_service_allowed`): there we cannot enumerate the reachable
/// services, so a ref that isn't a built tool must be assumed reachable rather
/// than flagged unavailable (it may well be an MCP service the behavior can
/// call). Treating it as missing would be a spurious degrade note.
///
/// Tradeoff (accepted): under `mcp_unrestricted` this also suppresses the
/// degrade note for a genuinely-absent host/CLI ref (e.g. a skill naming `bash`
/// when bash is off) — indistinguishable from an MCP service id without a
/// built-in-tool registry. The note is advisory only (privilege is unaffected:
/// the ceiling never *grants* a tool, it only annotates), so we prefer missing a
/// hint over emitting false "unavailable" notes for reachable MCP services.
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

    /// Whether the behavior can actually use `tool`. A built/allowlisted tool is
    /// in `names`; under unrestricted MCP everything else is given the benefit
    /// of the doubt (it may be a reachable MCP service).
    pub fn allows(&self, tool: &str) -> bool {
        self.mcp_unrestricted || self.names.contains(tool)
    }
}

/// Build the D3 ceiling from a behavior's resolved tool surface. `mcp_enabled`
/// is whether the behavior can call MCP at all (meta tools effectively on); an
/// empty `allowed_mcp_service_ids` then means "any service" (unrestricted).
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

/// Tools an active skill may use against a behavior's resolved `ceiling`: the
/// declared `tool_refs` intersected with the ceiling (D3 intersect+degrade).
/// Never returns a tool the ceiling disallows — the executable form of
/// `Skills.skillTools` / `Skills.activation_subset_ceiling`.
pub fn skill_tools<'a>(skill: &'a Skill, ceiling: &SkillToolCeiling) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| ceiling.allows(tool))
        .collect()
}

/// Declared `tool_refs` the behavior ceiling does NOT grant. Used to annotate
/// the activated-skill prompt so the model adapts (D3 degrade), and never to
/// expand the tool surface.
pub fn missing_tool_refs<'a>(skill: &'a Skill, ceiling: &SkillToolCeiling) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| !ceiling.allows(tool))
        .collect()
}

fn skill_label(skill: &Skill) -> &str {
    // Prefer the UI display name, then the canonical name, then the id.
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

/// Cached-layer skill **catalog**: name + description per candidate skill — the
/// progressive-disclosure "discovery" tier. The full body is NOT included; the
/// model loads it on demand via the `load_skill` tool. Returns `None` when the
/// behavior has no skills (so the preamble is unchanged). This is the design
/// shared by Anthropic Agent Skills, Codex, and Hermes: descriptions always in
/// context, bodies on demand.
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

/// Render a single skill's full body for on-demand activation (the `load_skill`
/// tool output). Appends a degrade note for any `tool_refs` the behavior ceiling
/// does not grant (D3), so the model knows the capability is unavailable rather
/// than silently failing.
pub fn render_activated_skill(skill: &Skill, ceiling: &SkillToolCeiling) -> String {
    let mut out = format!("Skill: {}\n\n{}", skill_label(skill), skill.instructions);
    let missing = missing_tool_refs(skill, ceiling);
    if !missing.is_empty() {
        out.push_str(&format!(
            "\n\nNote: this skill references tools that are not available to this behavior \
             and cannot be used: {}.",
            missing.join(", ")
        ));
    }
    out
}

/// Find a skill by display name, `name`, or `skill_id` (exact, then
/// case-insensitive). The catalog labels skills with `display_name` when set
/// (see `skill_label`), so the model may call `load_skill` with that label —
/// it must resolve here too, or a cataloged skill becomes unloadable. Also used
/// to resolve an explicitly-selected skill id against a behavior's effective set
/// for deterministic per-turn injection.
pub fn find_skill<'a>(skills: &'a [Skill], needle: &str) -> Option<&'a Skill> {
    let needle = needle.trim();
    // Trim the stored display name to match the catalog, which renders the
    // trimmed label (see `skill_label`) — otherwise a name with incidental
    // whitespace shows as loadable but fails to resolve.
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

/// Extract skill ids named by leading slash commands in a prompt.
///
/// This is intentionally only a reference extractor. It does not resolve the
/// skill or edit the prompt text; the runtime later resolves each id against
/// the behavior's effective skill set before injecting any skill body. Keeping
/// the prompt unchanged avoids corrupting legitimate prompts that happen to
/// start with an absolute path.
pub fn selected_skill_ids_from_prompt_slash_commands(prompt: &str) -> Vec<String> {
    let mut selected = Vec::new();
    let mut saw_command = false;

    for line in prompt.lines() {
        if line.trim().is_empty() && !saw_command {
            continue;
        }
        let Some(skill_id) = leading_slash_skill_id(line) else {
            break;
        };
        if !selected.iter().any(|existing| existing == &skill_id) {
            selected.push(skill_id);
        }
        saw_command = true;
    }

    selected
}

fn leading_slash_skill_id(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(rest.len());
    let skill_id = &rest[..end];
    if skill_id.is_empty() {
        return None;
    }

    // Treat `/work/file.rs` and similar as a path, not a slash command. If this
    // rejects a path-shaped skill id, the model can still load it through the
    // normal catalog-driven `load_skill` tool.
    if rest[end..].starts_with('/') {
        return None;
    }

    Some(skill_id.to_string())
}

/// Error type for [`LoadSkillTool`]. A missing skill is returned as readable
/// `Ok` text (so the model can recover), not an error.
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
    /// The skill name or skill_id from the Skills catalog.
    pub name: String,
}

/// The `load_skill` tool — progressive-disclosure activation. Given a skill name
/// (or id) from the catalog, returns that skill's full instructions, with tool
/// dependencies intersected against the behavior's tool ceiling (D3). It holds
/// the behavior's resolved effective skill set + ceiling, so it can only reveal
/// skills in the behavior's candidate set and never widens the tool surface.
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

impl crate::llm::tool::Tool for LoadSkillTool {
    const NAME: &'static str = "load_skill";
    type Error = LoadSkillError;
    type Args = LoadSkillArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> crate::llm::tool::ToolDefinition {
        crate::llm::tool::ToolDefinition {
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

    fn skill(id: &str, principal: &str, scope: SkillScope, tool_refs: &[&str]) -> Skill {
        Skill {
            skill_id: id.to_string(),
            agent_did: principal.to_string(),
            scope,
            name: format!("{id}-name"),
            description: format!("{id}-desc"),
            instructions: format!("{id}-instructions"),
            tool_refs: tool_refs.iter().map(|s| s.to_string()).collect(),
            display_name: None,
            enabled: true,
        }
    }

    /// A restricted ceiling (MCP not unrestricted) listing exactly `tools`.
    fn ceiling(tools: &[&str]) -> SkillToolCeiling {
        SkillToolCeiling::new(tools.iter().map(|s| s.to_string()).collect(), false)
    }

    fn ids(skills: &[&Skill]) -> Vec<String> {
        skills.iter().map(|s| s.skill_id.clone()).collect()
    }

    #[test]
    fn principal_scope_is_inherited_without_refs() {
        let skills = vec![skill("a", "did:p", SkillScope::Principal, &[])];
        let got = effective_skills(&skills, "did:p", &[], &[]);
        assert_eq!(ids(&got), vec!["a"]);
    }

    #[test]
    fn behavior_scope_requires_an_explicit_ref() {
        let skills = vec![skill("a", "did:p", SkillScope::Behavior, &[])];
        assert!(effective_skills(&skills, "did:p", &[], &[]).is_empty());
        let got = effective_skills(&skills, "did:p", &["a".to_string()], &[]);
        assert_eq!(ids(&got), vec!["a"]);
    }

    #[test]
    fn excludes_remove_inherited_principal_skills() {
        let skills = vec![skill("a", "did:p", SkillScope::Principal, &[])];
        let got = effective_skills(&skills, "did:p", &[], &["a".to_string()]);
        assert!(got.is_empty());
    }

    #[test]
    fn disabled_and_foreign_principal_skills_are_excluded() {
        let mut disabled = skill("a", "did:p", SkillScope::Principal, &[]);
        disabled.enabled = false;
        let foreign = skill("b", "did:other", SkillScope::Principal, &[]);
        let skills = vec![disabled, foreign];
        assert!(effective_skills(&skills, "did:p", &[], &[]).is_empty());
    }

    /// S-Skill-3 (candidate_set respects principal): every effective skill
    /// belongs to the behavior's principal and is enabled.
    #[test]
    fn effective_skills_respect_principal() {
        let skills = vec![
            skill("a", "did:p", SkillScope::Principal, &[]),
            skill("b", "did:p", SkillScope::Behavior, &[]),
            skill("c", "did:other", SkillScope::Principal, &[]),
        ];
        for got in effective_skills(&skills, "did:p", &["b".to_string()], &[]) {
            assert_eq!(got.agent_did, "did:p");
            assert!(got.enabled);
        }
    }

    /// S-Skill-1 (activation_subset_ceiling): the union of every active skill's
    /// resolved tools is a subset of the behavior ceiling — activation never
    /// widens the tool surface.
    #[test]
    fn skill_tools_never_widen_the_ceiling() {
        let ceiling = ceiling(&["read", "bash"]);
        let s = skill(
            "a",
            "did:p",
            SkillScope::Principal,
            &["read", "bash", "net"],
        );
        let resolved = skill_tools(&s, &ceiling);
        assert_eq!(resolved, vec!["read", "bash"]); // "net" degraded away
        for tool in &resolved {
            assert!(ceiling.allows(tool));
        }
        assert_eq!(missing_tool_refs(&s, &ceiling), vec!["net"]);
    }

    #[test]
    fn catalog_lists_descriptions_not_bodies() {
        assert!(render_skill_catalog(&[]).is_none());
        let skills = vec![
            skill("a", "did:p", SkillScope::Principal, &[]),
            skill("b", "did:p", SkillScope::Behavior, &[]),
        ];
        let catalog = render_skill_catalog(&skills).expect("catalog");
        assert!(catalog.contains("## Skills"));
        assert!(catalog.contains("load_skill")); // mandate to load on demand
        assert!(catalog.contains("a-name"));
        assert!(catalog.contains("a-desc"));
        assert!(catalog.contains("b-name"));
        // Progressive disclosure: bodies are NOT in the catalog.
        assert!(!catalog.contains("a-instructions"));
        assert!(!catalog.contains("b-instructions"));
    }

    #[test]
    fn catalog_prefers_display_name_over_name() {
        let mut s = skill("a", "did:p", SkillScope::Principal, &[]);
        s.display_name = Some("Pretty Label".to_string());
        let catalog = render_skill_catalog(std::slice::from_ref(&s)).expect("catalog");
        assert!(catalog.contains("Pretty Label"));
        assert!(!catalog.contains("a-name")); // the raw name is superseded by the UI label
    }

    #[test]
    fn render_activated_skill_appends_degrade_note() {
        let ceiling = ceiling(&["read"]);
        let s = skill("a", "did:p", SkillScope::Principal, &["read", "net"]);
        let body = render_activated_skill(&s, &ceiling);
        assert!(body.contains("a-instructions"));
        assert!(body.contains("net")); // degrade note names the missing tool
        let s_ok = skill("b", "did:p", SkillScope::Principal, &["read"]);
        assert!(!render_activated_skill(&s_ok, &ceiling).contains("not available"));
    }

    #[tokio::test]
    async fn load_skill_tool_returns_body_on_demand_and_handles_unknown() {
        use crate::llm::tool::Tool;
        let ceiling = ceiling(&["read"]);
        let skills = vec![skill(
            "research",
            "did:p",
            SkillScope::Principal,
            &["read", "net"],
        )];
        let tool = LoadSkillTool::new(skills, ceiling);

        // load by name -> full body + degrade note for the ungranted "net" ref.
        let body = tool
            .call(LoadSkillArgs {
                name: "research-name".to_string(),
            })
            .await
            .expect("load_skill");
        assert!(body.contains("research-instructions"));
        assert!(body.contains("net"));

        // load by skill_id also works.
        assert!(tool
            .call(LoadSkillArgs {
                name: "research".to_string(),
            })
            .await
            .expect("load by id")
            .contains("research-instructions"));

        // unknown skill -> readable Ok message listing what is available.
        let miss = tool
            .call(LoadSkillArgs {
                name: "nope".to_string(),
            })
            .await
            .expect("unknown is Ok text");
        assert!(miss.contains("No skill named"));
        assert!(miss.contains("research-name"));
    }

    #[test]
    fn skill_tool_ceiling_folds_in_explicit_mcp_service_ids() {
        // Restricted allowlist: built tools AND the explicit MCP ids are allowed.
        let ceiling = skill_tool_ceiling(
            vec!["read".to_string(), "bash".to_string()],
            &["x-data".to_string(), "observability-mcp".to_string()],
            /*mcp_enabled*/ true,
        );
        assert!(ceiling.allows("read"));
        assert!(ceiling.allows("x-data"));
        assert!(ceiling.allows("observability-mcp"));
        assert!(!ceiling.allows("unlisted-service")); // restricted: unknown is denied

        let mut mcp_skill = skill("a", "did:p", SkillScope::Principal, &["x-data"]);
        mcp_skill.tool_refs = vec!["x-data".to_string()];
        assert!(missing_tool_refs(&mcp_skill, &ceiling).is_empty());
    }

    #[test]
    fn skill_tool_ceiling_unrestricted_mcp_allows_any_service() {
        // Default behavior: meta tools on + EMPTY allowlist == any MCP service
        // allowed. A skill's MCP tool_ref must NOT be flagged unavailable, since
        // it may well be a reachable service we cannot enumerate.
        let ceiling = skill_tool_ceiling(
            vec!["read".to_string()],
            &[], // empty allowlist
            /*mcp_enabled*/ true,
        );
        assert!(ceiling.allows("read"));
        assert!(ceiling.allows("some-mcp-service")); // benefit of the doubt
        let mut mcp_skill = skill("a", "did:p", SkillScope::Principal, &["some-mcp-service"]);
        mcp_skill.tool_refs = vec!["some-mcp-service".to_string()];
        assert!(
            missing_tool_refs(&mcp_skill, &ceiling).is_empty(),
            "unrestricted MCP must not flag a service ref as unavailable"
        );

        // But with MCP disabled (no call_tool), an empty allowlist grants nothing.
        let no_mcp = skill_tool_ceiling(vec!["read".to_string()], &[], /*mcp_enabled*/ false);
        assert!(!no_mcp.allows("some-mcp-service"));
        assert_eq!(
            missing_tool_refs(&mcp_skill, &no_mcp),
            vec!["some-mcp-service"]
        );
    }

    #[tokio::test]
    async fn load_skill_resolves_by_display_name() {
        use crate::llm::tool::Tool;
        let mut s = skill("research", "did:p", SkillScope::Principal, &["read"]);
        s.display_name = Some("Deep Research".to_string());
        let tool = LoadSkillTool::new(vec![s], ceiling(&["read"]));
        // The catalog labels it "Deep Research"; load_skill with that label must
        // resolve (else a cataloged skill is unloadable).
        let body = tool
            .call(LoadSkillArgs {
                name: "Deep Research".to_string(),
            })
            .await
            .expect("load by display_name");
        assert!(body.contains("research-instructions"));
    }

    #[test]
    fn leading_slash_commands_select_skills_without_rewriting_prompt() {
        let ids = selected_skill_ids_from_prompt_slash_commands(
            "\n/vuln-scan /work --focus parser\n/triage\nRun the task.",
        );
        assert_eq!(ids, vec!["vuln-scan", "triage"]);

        assert!(
            selected_skill_ids_from_prompt_slash_commands("Run /vuln-scan as plain text",)
                .is_empty()
        );
        assert!(
            selected_skill_ids_from_prompt_slash_commands("/work/entry.c is an absolute path",)
                .is_empty()
        );
    }

    #[test]
    fn scope_parse_round_trips_and_rejects_unknown() {
        assert_eq!(SkillScope::parse("principal"), Some(SkillScope::Principal));
        assert_eq!(SkillScope::parse(" behavior "), Some(SkillScope::Behavior));
        assert_eq!(SkillScope::parse("global"), None);
        assert_eq!(SkillScope::Principal.as_str(), "principal");
    }
}
