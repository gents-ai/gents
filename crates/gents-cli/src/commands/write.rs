use crate::cli::args::WriteArgs;
use anyhow::Result;
use gents::llm::tool::Tool;

pub(crate) async fn write(args: WriteArgs) -> Result<()> {
    let call: gents::application_write::WriteParams = serde_json::from_str(&args.call)?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let tool = gents::application_write::WriteTool::new(
        (*access).clone(),
        args.collections.into_iter().collect(),
        None,
    );
    let result = tool.call(call).await?;
    crate::print_json(&serde_json::from_str::<serde_json::Value>(&result)?)
}
