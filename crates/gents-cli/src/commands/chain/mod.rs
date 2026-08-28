pub(crate) mod key;
pub(crate) mod query;

use anyhow::Result;

use crate::cli::args::ChainCommand;

pub(crate) async fn dispatch(command: ChainCommand) -> Result<()> {
    match command {
        ChainCommand::Key { command } => key::dispatch(command).await,
        ChainCommand::Query(args) => query::dispatch(args).await,
    }
}
