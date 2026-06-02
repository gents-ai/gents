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
                && (skill.scope == SkillScope::Principal
                    || refs.contains(skill.skill_id.as_str()))
                && !excludes.contains(skill.skill_id.as_str())
        })
        .collect()
}

/// Tools an active skill may use against a behavior's resolved tool `ceiling`:
/// the declared `tool_refs` intersected with the ceiling (D3 intersect+degrade).
/// Never returns a tool absent from the ceiling — the executable form of
/// `Skills.skillTools` / `Skills.activation_subset_ceiling`.
pub fn skill_tools<'a>(skill: &'a Skill, ceiling: &BTreeSet<String>) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| ceiling.contains(*tool))
        .collect()
}

/// Declared `tool_refs` the behavior ceiling does NOT grant. Used to annotate
/// the activated-skill prompt so the model adapts (D3 degrade), and never to
/// expand the tool surface.
pub fn missing_tool_refs<'a>(skill: &'a Skill, ceiling: &BTreeSet<String>) -> Vec<&'a str> {
    skill
        .tool_refs
        .iter()
        .map(String::as_str)
        .filter(|tool| !ceiling.contains(*tool))
        .collect()
}

/// Cached-layer listing of the candidate skill set (name + description), the
/// progressive-disclosure block the model uses to decide which skill to
/// activate. Returns `None` when there are no candidates (so the preamble adds
/// nothing). Mirrors Codex's "available skills" context block.
pub fn render_available_skills(candidates: &[&Skill]) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Skills\n\nThe following skills are available. If the user names a skill, or the \
         task clearly matches a skill's description, follow that skill's instructions for the \
         turn.\n",
    );
    for skill in candidates {
        out.push_str(&format!("\n- {}: {}", skill.name, skill.description));
    }
    Some(out)
}

/// Per-turn activated-skill body for injection as a `<system-reminder>`. Appends
/// a degrade note for any `tool_refs` the behavior ceiling does not grant (D3),
/// so the model knows the capability is unavailable rather than silently
/// failing.
pub fn render_activated_skill(skill: &Skill, ceiling: &BTreeSet<String>) -> String {
    let mut out = format!("Skill: {}\n\n{}", skill.name, skill.instructions);
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
            enabled: true,
        }
    }

    fn ceiling(tools: &[&str]) -> BTreeSet<String> {
        tools.iter().map(|s| s.to_string()).collect()
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
        let s = skill("a", "did:p", SkillScope::Principal, &["read", "bash", "net"]);
        let resolved = skill_tools(&s, &ceiling);
        assert_eq!(resolved, vec!["read", "bash"]); // "net" degraded away
        for tool in &resolved {
            assert!(ceiling.contains(*tool));
        }
        assert_eq!(missing_tool_refs(&s, &ceiling), vec!["net"]);
    }

    #[test]
    fn render_available_skills_is_none_when_empty() {
        assert!(render_available_skills(&[]).is_none());
        let skills = vec![skill("a", "did:p", SkillScope::Principal, &[])];
        let candidates = effective_skills(&skills, "did:p", &[], &[]);
        let listing = render_available_skills(&candidates).expect("listing");
        assert!(listing.contains("a-name"));
        assert!(listing.contains("a-desc"));
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

    #[test]
    fn scope_parse_round_trips_and_rejects_unknown() {
        assert_eq!(SkillScope::parse("principal"), Some(SkillScope::Principal));
        assert_eq!(SkillScope::parse(" behavior "), Some(SkillScope::Behavior));
        assert_eq!(SkillScope::parse("global"), None);
        assert_eq!(SkillScope::Principal.as_str(), "principal");
    }
}
