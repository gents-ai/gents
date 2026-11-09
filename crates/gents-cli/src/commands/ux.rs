//! `gents ux`: the UX plugins the desktop loads from a home, seen from the
//! terminal. `list` and `module` are the same two reads the desktop bridge
//! makes (`gents_server::packs::{ux_list, ux_module}`), so an author can see
//! exactly what the webview will receive; `lint` runs the static lint on
//! one module or stylesheet before it is ever in a pack.

use anyhow::{Context as _, Result};

use crate::cli::args::{UxCommand, UxLintArgs, UxListArgs, UxModuleArgs, UxPreviewArgs};

pub(crate) async fn dispatch(command: UxCommand) -> Result<()> {
    match command {
        UxCommand::List(args) => list(args),
        UxCommand::Module(args) => module(args).await,
        UxCommand::Lint(args) => lint(args),
        UxCommand::Preview(args) => preview(args).await,
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

async fn preview(args: UxPreviewArgs) -> Result<()> {
    use gents::observe::ux_preview as preview;
    use std::io::Write as _;

    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let home_for_run = home.clone();
    let target = preview::resolve_target(&home, &args.target, |namespace, plugin| async move {
        crate::commands::plugin::call_installed(
            &home_for_run,
            &namespace,
            &plugin,
            gents::plugin::ux::producer_input(),
        )
        .await
    })
    .await?;
    let (access, home) = crate::resolve_config_access(Some(&home), None).await?;
    let node_did = crate::packs::ux_installing_node(&home)?;
    let rerun = format!(
        "gents ux preview {} --home {} --out {}",
        args.target,
        home.display(),
        args.out.display()
    );
    let report = preview::run(
        preview::PreviewRequest {
            home,
            access,
            node_did,
            out: args.out,
            viewport: args.viewport,
        },
        target,
    )
    .await?;
    std::io::stdout()
        .write_all(report.to_text(&rerun).as_bytes())
        .context("writing the preview report")
}

fn lint(args: UxLintArgs) -> Result<()> {
    let findings = gents::plugin::ux::lint_file(&args.file)?;
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
