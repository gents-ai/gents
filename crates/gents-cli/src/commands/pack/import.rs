//! `gents pack import`: turn another ecosystem's plugin into a pack
//! directory for review. It never installs.
//!
//! Recognized sources: Agent Plugins (`plugin.json`), Claude Code
//! (`.claude-plugin/plugin.json`), Codex (`.codex-plugin/plugin.json`),
//! Hermes (`plugin.yaml`), an MCP registry `server.json`, and a single Agent
//! Skill (`SKILL.md`), from a directory or a git URL. Every part is built with
//! the same operations `gents pack add` uses, so the result passes the same
//! checks, and the generated README says what was converted, what needs a
//! port, and what was dropped and why.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::edit::{apply_add, PackEdit};
use crate::cli::{PackAddCommand, PackImportArgs, PackImportFrom, PackScaffoldArgs, PackTemplate};

#[derive(Default)]
struct Report {
    converted: Vec<String>,
    needs_port: Vec<String>,
    dropped: Vec<String>,
}

pub(crate) async fn import(args: PackImportArgs) -> Result<()> {
    let checkout;
    let (root, pin) = if is_git_url(&args.source) {
        checkout = tempfile::tempdir().context("creating a checkout directory")?;
        let pin = clone(&args.source, checkout.path())?;
        (checkout.path().to_path_buf(), Some(pin))
    } else {
        (PathBuf::from(&args.source), None)
    };
    let from = match args.from {
        Some(from) => from,
        None => detect(&root)?,
    };
    let manifest = plugin_manifest(&root, from)?;
    let name = args
        .name
        .clone()
        .or_else(|| manifest["name"].as_str().map(snake))
        .or_else(|| root.file_name().and_then(|name| name.to_str()).map(snake))
        .context("name the pack with --name")?;
    let out = args.out.clone().unwrap_or_else(|| PathBuf::from(&name));
    anyhow::ensure!(
        !out.join("manifest.json").exists(),
        "{} already holds a pack",
        out.display()
    );
    super::scaffold::scaffold(
        &out,
        &name,
        &PackScaffoldArgs {
            kind: None,
            namespace: args.namespace.clone(),
            template: Some(PackTemplate::Assets),
            language: None,
        },
    )?;

    let mut pack = PackEdit::load(&out)?;
    let mut report = Report::default();
    import_parts(&root, from, &mut pack, &mut report)?;
    if let Some(description) = manifest["description"].as_str() {
        pack.manifest["description"] = json!(description);
    }
    if let Some(version) = manifest["version"].as_str() {
        if semver::Version::parse(version).is_ok() {
            pack.manifest["version"] = json!(version);
        }
    }
    pack.save()?;
    std::fs::write(
        out.join("README.md"),
        readme(
            &name,
            &args.source,
            pin.as_deref(),
            &manifest,
            from,
            &report,
        ),
    )
    .context("writing README.md")?;

    let check = super::check::check_dir(&out).await;
    crate::print_json(&json!({
        "pack": name,
        "dir": out,
        "from": format!("{from:?}"),
        "converted": report.converted,
        "needs_port": report.needs_port,
        "dropped": report.dropped,
        "check": check.problems,
    }))
}

fn is_git_url(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("git@") || source.ends_with(".git")
}

/// Clones `url` shallowly and returns the commit it checked out.
fn clone(url: &str, into: &Path) -> Result<String> {
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", url])
        .arg(into)
        .status()
        .context("running git clone")?;
    anyhow::ensure!(status.success(), "git clone of {url} failed");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(into)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("reading the cloned commit")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn detect(root: &Path) -> Result<PackImportFrom> {
    Ok(if root.join(".claude-plugin/plugin.json").is_file() {
        PackImportFrom::Claude
    } else if root.join(".codex-plugin/plugin.json").is_file() {
        PackImportFrom::Codex
    } else if root.join("plugin.json").is_file() {
        PackImportFrom::AgentPlugins
    } else if root.join("plugin.yaml").is_file() {
        PackImportFrom::Hermes
    } else if root.join("server.json").is_file() {
        PackImportFrom::Mcp
    } else if root.join("SKILL.md").is_file() {
        PackImportFrom::Skill
    } else {
        anyhow::bail!(
            "{} is not a plugin gents recognizes; name its kind with --from",
            root.display()
        )
    })
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("{} is not valid JSON", path.display()))
}

fn plugin_manifest(root: &Path, from: PackImportFrom) -> Result<Value> {
    Ok(match from {
        PackImportFrom::Claude => read_json(&root.join(".claude-plugin/plugin.json"))?,
        PackImportFrom::Codex => read_json(&root.join(".codex-plugin/plugin.json"))?,
        PackImportFrom::AgentPlugins => read_json(&root.join("plugin.json"))?,
        PackImportFrom::Mcp => read_json(&root.join("server.json"))?,
        PackImportFrom::Hermes => {
            let text =
                std::fs::read_to_string(root.join("plugin.yaml")).context("reading plugin.yaml")?;
            serde_yaml::from_str(&text).context("plugin.yaml is not valid YAML")?
        }
        PackImportFrom::Skill => {
            let text =
                std::fs::read_to_string(root.join("SKILL.md")).context("reading SKILL.md")?;
            let (front, _) = gents::skills::import::parse_skill_md(&text)?;
            json!({ "name": front.name, "description": front.description, "license": front.license })
        }
    })
}

fn import_parts(
    root: &Path,
    from: PackImportFrom,
    pack: &mut PackEdit,
    report: &mut Report,
) -> Result<()> {
    if from == PackImportFrom::Skill {
        return import_skill(root, pack, report);
    }
    if let Ok(entries) = std::fs::read_dir(root.join("skills")) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.join("SKILL.md").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            import_skill(&dir, pack, report)?;
        }
    }
    for file in ["CLAUDE.md", "AGENTS.md"] {
        if root.join(file).is_file() {
            let id = format!(
                "{}-instructions",
                kebab(&file.to_lowercase().replace(".md", ""))
            );
            add(
                pack,
                PackAddCommand::Behavior {
                    id: id.clone(),
                    slot: Some("worker".into()),
                },
            )?;
            write_prompt(
                pack,
                &format!("agent_behaviors/{}/system_prompt.md", snake(&id)),
                &root.join(file),
            )?;
            report
                .converted
                .push(format!("{file} became the {id} behavior's instructions"));
        }
    }
    import_markdown_dir(root, "agents", pack, report, |pack, id, body_path| {
        add(
            pack,
            PackAddCommand::Behavior {
                id: id.to_owned(),
                slot: Some("agents".into()),
            },
        )?;
        write_prompt(
            pack,
            &format!("agent_behaviors/{}/system_prompt.md", snake(id)),
            body_path,
        )?;
        Ok(format!("agent {id} became a behavior"))
    })?;
    import_markdown_dir(root, "commands", pack, report, |pack, id, body_path| {
        let behavior = "commands".to_owned();
        if !has(pack, "agent_behaviors", "behavior_id", &behavior) {
            add(
                pack,
                PackAddCommand::Behavior {
                    id: behavior.clone(),
                    slot: Some("worker".into()),
                },
            )?;
        }
        add(
            pack,
            PackAddCommand::Task {
                id: id.to_owned(),
                behavior,
            },
        )?;
        write_prompt(pack, &format!("tasks/{}/prompt.md", snake(id)), body_path)?;
        Ok(format!("command {id} became a task"))
    })?;
    for file in [".mcp.json", "mcp.json"] {
        if let Ok(value) = read_json(&root.join(file)) {
            let servers = value
                .get("mcpServers")
                .or_else(|| value.get("servers"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (server, config) in servers {
                if config.get("url").is_some() {
                    report.needs_port.push(format!(
                        "MCP server {server} (HTTP): register it as a tool service; packs cannot name remote MCP URLs yet"
                    ));
                } else {
                    report.needs_port.push(format!(
                        "MCP server {server} (stdio): port it to a plugin that serves MCP"
                    ));
                }
            }
        }
    }
    if from == PackImportFrom::Mcp {
        report.needs_port.push(
            "the MCP server itself: register its remote as a tool service, or port a package to a plugin"
                .into(),
        );
    }
    if from == PackImportFrom::Hermes {
        let manifest = plugin_manifest(root, from)?;
        for tool in manifest["provides_tools"].as_array().into_iter().flatten() {
            report.needs_port.push(format!(
                "tool {}: rewrite as a plugin that reads its params as JSON on stdin and writes its result to stdout",
                tool.as_str().unwrap_or("?")
            ));
        }
        if let Some(env) = manifest["requires_env"]
            .as_array()
            .filter(|env| !env.is_empty())
        {
            report.needs_port.push(format!(
                "environment {}: declare it in the ported plugin's authority",
                env.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for (path, why) in [
        (
            "hooks",
            "hooks run unsandboxed in their source and have no portable equivalent",
        ),
        (
            "hooks/hooks.json",
            "hooks run unsandboxed in their source and have no portable equivalent",
        ),
        ("bin", "executables are not run by gents"),
        (
            ".lsp.json",
            "language servers are configured on the host, not in packs",
        ),
        ("output-styles", "output styles have no gents equivalent"),
    ] {
        if root.join(path).exists() {
            report.dropped.push(format!("{path}: {why}"));
        }
    }
    report.dropped.sort();
    report.dropped.dedup();
    Ok(())
}

fn import_skill(dir: &Path, pack: &mut PackEdit, report: &mut Report) -> Result<()> {
    let text = std::fs::read_to_string(dir.join("SKILL.md")).context("reading SKILL.md")?;
    let (front, _) = gents::skills::import::parse_skill_md(&text)?;
    let id = kebab(
        front
            .name
            .as_deref()
            .or_else(|| dir.file_name().and_then(|name| name.to_str()))
            .unwrap_or("skill"),
    );
    add(
        pack,
        PackAddCommand::Skill {
            id: id.clone(),
            from: Some(dir.join("SKILL.md")),
        },
    )?;
    if let Some(row) = pack
        .list("skills")?
        .iter_mut()
        .find(|row| row["skill_id"] == id.as_str())
    {
        if let Some(description) = &front.description {
            row["description"] = json!(description);
        }
        if let Some(tools) = &front.allowed_tools {
            row["tool_refs"] = json!(tools.split_whitespace().collect::<Vec<_>>());
        }
    }
    let base = format!("skills/{}", snake(&id));
    copy_support_files(dir, dir, &base, pack, report, &id)?;
    report.converted.push(format!("skill {id}"));
    Ok(())
}

/// Copies a skill's supporting files as declared assets, renaming them to
/// the snake_case every pack file uses. `scripts/` is reported, never copied:
/// gents does not run skill support files.
fn copy_support_files(
    root: &Path,
    dir: &Path,
    base: &str,
    pack: &mut PackEdit,
    report: &mut Report,
    skill: &str,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || (dir == root && name == "SKILL.md") {
            continue;
        }
        if dir == root && name == "scripts" {
            report.needs_port.push(format!(
                "skill {skill} scripts: gents does not run skill scripts; port them to a plugin"
            ));
            continue;
        }
        let target = format!("{base}/{}", file_name(&name));
        if entry.file_type()?.is_dir() {
            copy_support_files(root, &entry.path(), &target, pack, report, skill)?;
        } else {
            pack.create_file(&target, std::fs::read(entry.path())?)?;
            if file_name(&name) != name {
                report.converted.push(format!(
                    "skill {skill}: {name} is renamed {}; update links to it",
                    file_name(&name)
                ));
            }
        }
    }
    Ok(())
}

fn import_markdown_dir(
    root: &Path,
    dir: &str,
    pack: &mut PackEdit,
    report: &mut Report,
    mut each: impl FnMut(&mut PackEdit, &str, &Path) -> Result<String>,
) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return Ok(());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    for file in files {
        let id = kebab(
            file.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(dir),
        );
        report.converted.push(each(pack, &id, &file)?);
    }
    Ok(())
}

/// Replaces a prompt `pack add` wrote with the body of `source`, without its
/// frontmatter.
fn write_prompt(pack: &PackEdit, path: &str, source: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(source).with_context(|| format!("reading {}", source.display()))?;
    let (_, body) = gents::skills::import::parse_skill_md(&text)?;
    std::fs::write(pack.dir.join(path), body + "\n").with_context(|| format!("writing {path}"))
}

fn add(pack: &mut PackEdit, command: PackAddCommand) -> Result<()> {
    apply_add(pack, command)
}

fn has(pack: &mut PackEdit, key: &str, field: &str, id: &str) -> bool {
    pack.list(key)
        .map(|rows| rows.iter().any(|row| row[field] == id))
        .unwrap_or(false)
}

/// `Extract Tables` or `extract_tables` -> `extract-tables`: a document id.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if out.is_empty() && ch.is_ascii_digit() {
                out.push('x');
            }
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "part".to_owned()
    } else {
        out
    }
}

fn snake(name: &str) -> String {
    kebab(name).replace('-', "_")
}

/// A file name in the snake_case pack files use, keeping its extension.
fn file_name(name: &str) -> String {
    if matches!(name, "README.md" | "SKILL.md" | "Cargo.toml" | "Cargo.lock") {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            format!("{}.{}", snake(stem), ext.to_ascii_lowercase())
        }
        _ => snake(name),
    }
}

fn readme(
    name: &str,
    source: &str,
    pin: Option<&str>,
    manifest: &Value,
    from: PackImportFrom,
    report: &Report,
) -> String {
    let list = |items: &[String]| {
        if items.is_empty() {
            "None.\n".to_owned()
        } else {
            items.iter().map(|item| format!("- {item}\n")).collect()
        }
    };
    format!(
        "# {name}\n\nImported from {source} ({from:?}){pin}. {description}\n\n\
         ## Origin\n\n- Source: {source}\n- Pin: {pin_line}\n- License: {license}\n\n\
         ## Converted\n\n{converted}\n## Needs a port\n\n{needs}\n## Dropped\n\n{dropped}\n\
         ## Installation\n\nReview this directory, port what is listed above, then:\n\n\
         ```sh\ngents pack check ./{name}\ngents pack build ./{name}\n```\n",
        pin = pin.map(|pin| format!(" at {pin}")).unwrap_or_default(),
        description = manifest["description"].as_str().unwrap_or(""),
        pin_line = pin.unwrap_or("a local directory, not pinned"),
        license = manifest["license"].as_str().unwrap_or("not stated"),
        converted = list(&report.converted),
        needs = list(&report.needs_port),
        dropped = list(&report.dropped),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }

    fn args(source: &Path, out: &Path) -> PackImportArgs {
        PackImportArgs {
            source: source.to_string_lossy().into_owned(),
            from: None,
            out: Some(out.to_path_buf()),
            name: None,
            namespace: "acme".into(),
        }
    }

    #[tokio::test]
    async fn a_claude_code_plugin_becomes_a_pack_that_passes_the_check() {
        let source = tempfile::tempdir().unwrap();
        let root = source.path();
        write(
            root,
            ".claude-plugin/plugin.json",
            r#"{"name":"review-kit","description":"Review helpers","version":"1.2.0","license":"MIT"}"#,
        );
        write(root, "skills/extract-tables/SKILL.md", "---\nname: extract-tables\ndescription: Pull tables out of PDFs\nallowed-tools: read_file grep\n---\nUse this when a PDF has tables.\n");
        write(
            root,
            "skills/extract-tables/references/Table-Guide.md",
            "# guide\n",
        );
        write(root, "skills/extract-tables/scripts/run.py", "print(1)\n");
        write(
            root,
            "commands/summarize.md",
            "---\ndescription: summarize\n---\nSummarize the diff.\n",
        );
        write(
            root,
            "agents/security-reviewer.md",
            "---\nname: security-reviewer\n---\nYou review for security.\n",
        );
        write(
            root,
            ".mcp.json",
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp"},"local":{"command":"node"}}}"#,
        );
        write(root, "hooks/hooks.json", "{}");
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("review_kit");
        import(args(root, &dir)).await.unwrap();

        let report = super::super::check::check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["version"], "1.2.0");
        let assets = manifest["assets"].to_string();
        assert!(
            assets.contains("skills/extract_tables/SKILL.md"),
            "{assets}"
        );
        assert!(
            assets.contains("skills/extract_tables/references/table_guide.md"),
            "{assets}"
        );
        assert!(!assets.contains("scripts"), "{assets}");
        let config: Value =
            serde_json::from_slice(&std::fs::read(dir.join("pack_config.json")).unwrap()).unwrap();
        assert_eq!(
            config["skills"][0]["tool_refs"],
            json!(["read_file", "grep"])
        );
        assert!(config["tasks"].to_string().contains("summarize"));
        assert!(config["agent_behaviors"]
            .to_string()
            .contains("security-reviewer"));
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(readme.contains("License: MIT"), "{readme}");
        assert!(readme.contains("MCP server docs (HTTP)"), "{readme}");
        assert!(readme.contains("MCP server local (stdio)"), "{readme}");
        assert!(readme.contains("scripts"), "{readme}");
        assert!(readme.contains("hooks"), "{readme}");
        assert_eq!(
            std::fs::read_to_string(dir.join("tasks/summarize/prompt.md")).unwrap(),
            "Summarize the diff.\n"
        );
    }

    #[tokio::test]
    async fn a_single_agent_skill_becomes_a_pack() {
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("pdf-tools");
        write(&root, "SKILL.md", "---\nname: pdf-tools\ndescription: PDF work\nlicense: Apache-2.0\n---\nHow to work with PDFs.\n");
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("pdf_tools");
        import(args(&root, &dir)).await.unwrap();
        let report = super::super::check::check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        assert!(std::fs::read_to_string(dir.join("README.md"))
            .unwrap()
            .contains("License: Apache-2.0"));
    }

    #[tokio::test]
    async fn an_unrecognized_directory_asks_for_its_kind() {
        let source = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let error = import(args(source.path(), &out.path().join("x")))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("--from"), "{error:#}");
    }
}
