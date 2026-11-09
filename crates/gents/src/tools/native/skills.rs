use crate::documents::skills::{
    find_skill, render_activated_skill, skill_label, Skill, SkillToolCeiling,
};

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
    selection: Option<crate::inference::resolved::SelectedConfiguration>,
}

impl LoadSkillTool {
    #[cfg(test)]
    pub fn new(skills: Vec<Skill>, ceiling: SkillToolCeiling) -> Self {
        Self {
            skills,
            ceiling,
            selection: None,
        }
    }

    pub(crate) fn for_agent(
        skills: Vec<Skill>,
        ceiling: SkillToolCeiling,
        configuration: &crate::inference::resolved::ResolvedConfiguration,
    ) -> Self {
        let selection = match configuration {
            crate::inference::resolved::ResolvedConfiguration::Static => None,
            crate::inference::resolved::ResolvedConfiguration::Documents(selection) => {
                Some(selection.clone())
            }
        };
        Self {
            skills,
            ceiling,
            selection,
        }
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
            Some(skill) => {
                if let Some(selection) = &self.selection {
                    selection
                        .check_skill(&skill.skill_id)
                        .await
                        .map_err(|_| LoadSkillError("selected skill is unavailable".into()))?;
                }
                Ok(render_activated_skill(skill, &self.ceiling))
            }
            None => {
                if let Some(selection) = &self.selection {
                    selection
                        .check_skills(
                            &self
                                .skills
                                .iter()
                                .map(|skill| skill.skill_id.clone())
                                .collect::<Vec<_>>(),
                        )
                        .await
                        .map_err(|_| LoadSkillError("selected skill is unavailable".into()))?;
                }
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
