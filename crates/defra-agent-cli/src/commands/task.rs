use anyhow::Result;

use crate::cli::TaskCommand;

pub(crate) async fn dispatch(command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::Run(args) => crate::commands::config::task_run::config_task_run(args).await,
    }
}
