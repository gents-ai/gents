use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use gents_protocol::schemas::{
    AGENT_OWNERSHIP_POLICY_ID, IDENTITY_POLICY_BINDINGS, MAILBOX_ITEM_NAME, MAILBOX_POLICY_ID,
    MAILBOX_POLICY_RESOURCE,
};

fn graphql_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            graphql_files(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "graphql") {
            files.push(path);
        }
    }
    Ok(())
}

fn type_position(source: &str, name: &str) -> Option<usize> {
    let prefix = format!("type {name}");
    source.match_indices(&prefix).find_map(|(pos, _)| {
        let line_start = source[..pos].rfind('\n').map_or(0, |i| i + 1);
        let next = source[pos + prefix.len()..].chars().next();
        (source[line_start..pos].trim().is_empty()
            && next.is_some_and(|c| c.is_whitespace() || c == '@' || c == '{'))
        .then_some(pos + prefix.len())
    })
}

fn bind(
    source: &str,
    name: &str,
    resource: &str,
    new_id: &str,
    old_ids: &[&str],
) -> Result<String> {
    let pos = type_position(source, name).context("collection declaration is missing")?;
    let body = source[pos..]
        .find('{')
        .context("collection body is missing")?
        + pos;
    let header = &source[pos..body];
    let directive = format!("@policy(id: \"{new_id}\", resource: \"{resource}\")");
    let mut result = source.to_owned();
    if let Some(start) = header.find("@policy(") {
        ensure!(
            !header[start + 1..].contains("@policy("),
            "duplicate policy directive"
        );
        let start = pos + start;
        let end = source[start..body]
            .find(')')
            .context("unterminated policy directive")?
            + start
            + 1;
        let previous = &source[start..end];
        ensure!(
            previous == directive
                || old_ids.iter().any(
                    |id| previous == format!("@policy(id: \"{id}\", resource: \"{resource}\")")
                ),
            "unexpected existing policy: {previous}"
        );
        result.replace_range(start..end, &directive);
    } else {
        result.insert_str(pos, &format!(" {directive}"));
    }
    Ok(result)
}

fn build_models(proofs: &Path) -> Result<()> {
    let build = Command::new("lake")
        .args([
            "build",
            "Proofs.Identity.Household",
            "Proofs.Identity.MailboxAccess",
        ])
        .current_dir(proofs)
        .status()
        .context("building native policy models")?;
    ensure!(build.success(), "native policy model build failed");
    Ok(())
}

fn run_lean(proofs: &Path, script: &str) -> Result<String> {
    let output = Command::new("lake")
        .args(["env", "lean", "--run", script])
        .current_dir(proofs)
        .output()
        .with_context(|| format!("executing {script}"))?;
    ensure!(
        output.status.success(),
        "native policy emitter {script} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let check = match args.next().as_deref() {
        None => false,
        Some("--check") => true,
        Some(arg) => bail!("unexpected argument {arg:?}"),
    };
    ensure!(args.next().is_none(), "unexpected additional argument");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let proofs = root.join("crates/gents/proofs");
    build_models(&proofs)?;
    let yaml = run_lean(&proofs, "GenerateHouseholdPolicy.lean")?;
    let parsed = acp::policy_yaml::parse_policy_yaml(&yaml).map_err(anyhow::Error::msg)?;
    let id = acp::policy_yaml::generate_policy_id(&parsed, 0);
    acp::policy_yaml::validate_policy_expressions(&parsed).map_err(anyhow::Error::msg)?;
    let native_policy = acp::policy_yaml::build_policy(&parsed, 0)?;
    ensure!(native_policy.id == id, "native policy ID mismatch");
    let yaml_path = root.join("crates/gents-schemas/schemas/agent/agent_ownership_policy.yaml");
    let previous_yaml = fs::read_to_string(&yaml_path)?;
    let previous_policy =
        acp::policy_yaml::parse_policy_yaml(&previous_yaml).map_err(anyhow::Error::msg)?;
    let previous_id = acp::policy_yaml::generate_policy_id(&previous_policy, 0);
    let old_ids = [AGENT_OWNERSHIP_POLICY_ID, previous_id.as_str()];
    let mailbox_yaml = run_lean(&proofs, "GenerateMailboxPolicy.lean")?;
    let mailbox_parsed =
        acp::policy_yaml::parse_policy_yaml(&mailbox_yaml).map_err(anyhow::Error::msg)?;
    acp::policy_yaml::validate_policy_expressions(&mailbox_parsed).map_err(anyhow::Error::msg)?;
    let mailbox_id = acp::policy_yaml::generate_policy_id(&mailbox_parsed, 0);
    ensure!(
        acp::policy_yaml::build_policy(&mailbox_parsed, 0)?.id == mailbox_id,
        "native mailbox policy ID mismatch"
    );
    ensure!(
        mailbox_id != id,
        "mailbox and Household policies must be distinct"
    );
    ensure!(
        mailbox_parsed.resources.len() == 1
            && mailbox_parsed.resources[0].name == MAILBOX_POLICY_RESOURCE,
        "mailbox policy resource differs from its binding"
    );
    let mailbox_yaml_path = root.join("crates/gents-schemas/schemas/agent/mailbox_policy.yaml");
    let previous_mailbox_yaml = fs::read_to_string(&mailbox_yaml_path)?;
    let previous_mailbox =
        acp::policy_yaml::parse_policy_yaml(&previous_mailbox_yaml).map_err(anyhow::Error::msg)?;
    let previous_mailbox_id = acp::policy_yaml::generate_policy_id(&previous_mailbox, 0);
    let mailbox_old_ids = [MAILBOX_POLICY_ID, previous_mailbox_id.as_str()];
    let resource_names: BTreeSet<_> = parsed
        .resources
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    let mut bindings = BTreeMap::new();
    for &(name, resource) in IDENTITY_POLICY_BINDINGS {
        ensure!(
            resource_names.contains(resource),
            "unknown policy resource {resource:?} for {name}"
        );
        ensure!(
            bindings.insert(name, resource).is_none(),
            "duplicate binding for {name}"
        );
    }

    let mut files = Vec::new();
    graphql_files(&root.join("crates/gents-schemas/schemas"), &mut files)?;
    graphql_files(&root.join("crates/gents-protocol/schemas"), &mut files)?;
    files.sort();
    let mut changes = Vec::new();
    let mut found = BTreeSet::new();
    let mut found_mailbox = false;
    for path in files {
        let source = fs::read_to_string(&path)?;
        let matches: Vec<_> = bindings
            .iter()
            .filter(|(name, _)| type_position(&source, name).is_some())
            .collect();
        ensure!(
            matches.len() <= 1,
            "multiple catalog collections in {}",
            path.display()
        );
        if let Some((name, resource)) = matches.first() {
            ensure!(
                !mailbox_old_ids.iter().any(|id| source.contains(id))
                    && !source.contains(&mailbox_id),
                "mailbox binding appears on Household collection {}",
                path.display()
            );
            ensure!(found.insert(**name), "duplicate collection {name}");
            let updated = bind(&source, name, resource, &id, &old_ids)
                .with_context(|| path.display().to_string())?;
            changes.push((path, source, updated));
        } else if type_position(&source, MAILBOX_ITEM_NAME).is_some() {
            ensure!(
                !old_ids.iter().any(|id| source.contains(id)) && !source.contains(&id),
                "Household binding appears on mailbox collection {}",
                path.display()
            );
            ensure!(!found_mailbox, "duplicate mailbox collection");
            found_mailbox = true;
            let updated = bind(
                &source,
                MAILBOX_ITEM_NAME,
                MAILBOX_POLICY_RESOURCE,
                &mailbox_id,
                &mailbox_old_ids,
            )
            .with_context(|| path.display().to_string())?;
            changes.push((path, source, updated));
        } else {
            ensure!(
                !old_ids.iter().any(|id| source.contains(id)) && !source.contains(&id),
                "unlisted Household binding in {}",
                path.display()
            );
            ensure!(
                !mailbox_old_ids.iter().any(|id| source.contains(id))
                    && !source.contains(&mailbox_id),
                "unlisted mailbox binding in {}",
                path.display()
            );
        }
    }
    ensure!(found_mailbox, "mailbox collection has no schema");
    for name in bindings.keys() {
        ensure!(
            found.contains(name),
            "catalog collection {name} has no schema"
        );
    }
    let id_path = root.join("crates/gents-schemas/src/lib.rs");
    let id_source = fs::read_to_string(&id_path)?;
    let old = format!(
        "pub const AGENT_OWNERSHIP_POLICY_ID: &str =\n    \"{AGENT_OWNERSHIP_POLICY_ID}\";"
    );
    let replacement = format!("pub const AGENT_OWNERSHIP_POLICY_ID: &str =\n    \"{id}\";");
    ensure!(
        id_source.matches(&old).count() == 1,
        "Household policy ID constant is missing or duplicated"
    );
    let mut id_updated = id_source.replacen(&old, &replacement, 1);
    let old_mailbox = format!("pub const MAILBOX_POLICY_ID: &str =\n    \"{MAILBOX_POLICY_ID}\";");
    let new_mailbox = format!("pub const MAILBOX_POLICY_ID: &str =\n    \"{mailbox_id}\";");
    ensure!(
        id_updated.matches(&old_mailbox).count() == 1,
        "mailbox policy ID constant is missing or duplicated"
    );
    id_updated = id_updated.replacen(&old_mailbox, &new_mailbox, 1);
    changes.push((id_path, id_source, id_updated));
    changes.push((yaml_path, previous_yaml, yaml));
    changes.push((mailbox_yaml_path, previous_mailbox_yaml, mailbox_yaml));

    let mut drift = Vec::new();
    for (path, old, new) in changes {
        if old != new {
            if check {
                drift.push(path);
            } else {
                fs::write(&path, new)?;
            }
        }
    }
    ensure!(
        drift.is_empty(),
        "native policy artifacts differ: {drift:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_updates_only_the_expected_policy_and_is_idempotent() {
        let old = "old-policy";
        let new = "new-policy";
        let source = format!("type Skill @policy(id: \"{old}\", resource: \"configuration\") @index(fields: [\"skill_id\"]) {{ skill_id: String }}\n");
        let updated = bind(&source, "Skill", "configuration", new, &[old]).unwrap();
        assert_eq!(updated, source.replace(old, new));
        assert_eq!(
            bind(&updated, "Skill", "configuration", new, &[old]).unwrap(),
            updated
        );
        assert!(bind(&source, "Skill", "operations", new, &[old]).is_err());
        assert!(bind(&source, "Skill", "configuration", new, &["other"]).is_err());
    }

    #[test]
    fn catalog_binding_adds_policy_to_an_unbound_multiline_type() {
        let source =
            "type CallbackResult\n    @index(fields: [\"node_did\"]) { node_did: String }\n";
        let updated = bind(source, "CallbackResult", "agent_children", "policy", &[]).unwrap();
        assert_eq!(updated, "type CallbackResult @policy(id: \"policy\", resource: \"agent_children\")\n    @index(fields: [\"node_did\"]) { node_did: String }\n");
        assert!(bind(source, "Callback", "agent_children", "policy", &[]).is_err());
    }
}
