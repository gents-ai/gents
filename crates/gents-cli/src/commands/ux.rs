//! `gents ux`: the UX plugins the desktop loads from a home, seen from the
//! terminal. `list` and `module` are the same two reads the desktop bridge
//! makes (`gents_server::packs::{ux_list, ux_module}`), so an author can see
//! exactly what the webview will receive; `lint` runs the static lint on
//! one module or stylesheet before it is ever in a pack.

use anyhow::{Context, Result};

use crate::cli::args::{UxCommand, UxLintArgs, UxListArgs, UxModuleArgs};

pub(crate) async fn dispatch(command: UxCommand) -> Result<()> {
    match command {
        UxCommand::List(args) => list(args),
        UxCommand::Module(args) => module(args).await,
        UxCommand::Lint(args) => lint(args),
    }
}

fn list(args: UxListArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let plugins = crate::packs::ux_list(&home)?;
    crate::print_json(&serde_json::json!({ "home": home, "plugins": plugins }))
}

async fn module(args: UxModuleArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let module = crate::packs::ux_module(home, args.id).await?;
    crate::print_json(&serde_json::to_value(module)?)
}

fn lint(args: UxLintArgs) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("reading {}", args.file.display()))?;
    let is_stylesheet = args.file.extension().is_some_and(|e| e == "css");
    // Either file may declare a custom property the other reads, so each is
    // linted with the one the frame would load beside it.
    let beside = |name: &str| std::fs::read_to_string(args.file.with_file_name(name)).ok();
    let findings = if is_stylesheet {
        let entry = beside(gents::plugin::ux::DEV_ENTRY).unwrap_or_default();
        let mut findings = gents::plugin::ux::lint::lint_plugin(&entry, Some(&text));
        findings.retain(|finding| finding.stylesheet);
        findings
    } else {
        let css = beside(gents::plugin::ux::DEV_CSS);
        gents::plugin::ux::lint::lint_plugin(&text, css.as_deref())
    };
    crate::print_json(&serde_json::json!({
        "file": args.file,
        "admitted": findings.is_empty(),
        "findings": findings,
    }))?;
    anyhow::ensure!(
        findings.is_empty(),
        "{} is refused by the static lint:\n{}",
        args.file.display(),
        gents::plugin::ux::lint::describe(&findings)
    );
    Ok(())
}
