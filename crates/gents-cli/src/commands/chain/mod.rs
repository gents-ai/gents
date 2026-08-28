pub(crate) mod key;

use anyhow::Result;

use crate::cli::args::ChainCommand;

pub(crate) async fn dispatch(command: ChainCommand) -> Result<()> {
    match command {
        ChainCommand::Key { command } => key::dispatch(command).await,
    }
}
