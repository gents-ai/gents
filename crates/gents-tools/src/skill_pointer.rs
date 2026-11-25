use std::sync::Mutex;

use gents_documents::skills::{skill_label, Skill};

/// The skills in the caller's own catalog that cover a help page or a failing
/// call (Lean `SkillPointer.step`), shared by the config and schema tools one
/// surface builds. A caller whose catalog it cannot load from holds an empty
/// one and is named nothing.
#[derive(Debug, Default)]
pub struct SkillPointers {
    catalog: Vec<Skill>,
    memory: Mutex<Memory>,
}

#[derive(Debug, Default)]
struct Memory {
    request: Option<String>,
    failed: Vec<Vec<String>>,
}

impl SkillPointers {
    pub fn new(catalog: Vec<Skill>) -> Self {
        Self {
            catalog,
            memory: Mutex::default(),
        }
    }

    /// The line a help page at `path` ends with.
    pub fn help(&self, path: &[&str]) -> Option<String> {
        self.pointer(path).map(|(skill, _)| line(skill))
    }

    /// The line the failure of a non-help call at `path` in `request` carries.
    pub fn failure(&self, request: &str, path: &[&str]) -> Option<String> {
        let mut memory = self
            .memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if memory.request.as_deref() != Some(request) {
            *memory = Memory {
                request: Some(request.to_owned()),
                failed: Vec::new(),
            };
        }
        let (skill, cover) = self.pointer(path)?;
        if memory.failed.contains(&cover) {
            return None;
        }
        memory.failed.push(cover);
        Some(line(skill))
    }

    /// Lean `SkillPointer.pointer`.
    fn pointer(&self, path: &[&str]) -> Option<(&Skill, Vec<String>)> {
        let mut best: Option<(&Skill, Vec<String>)> = None;
        for skill in &self.catalog {
            let Some(cover) = matched(skill, path) else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, b)| b.len() < cover.len()) {
                best = Some((skill, cover));
            }
        }
        best
    }
}

/// Lean `SkillPointer.matched`.
fn matched(skill: &Skill, path: &[&str]) -> Option<Vec<String>> {
    let mut best: Option<Vec<String>> = None;
    for cover in &skill.covers {
        let words = cover.split_whitespace().collect::<Vec<_>>();
        if !words.is_empty()
            && path.starts_with(&words)
            && best.as_ref().is_none_or(|b| b.len() < words.len())
        {
            best = Some(words.into_iter().map(str::to_owned).collect());
        }
    }
    best
}

fn line(skill: &Skill) -> String {
    format!("First load `{}` if not loaded.", skill_label(skill))
}
