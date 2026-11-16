use gents_host::args::StoreKeyCustodyArg;
use std::path::PathBuf;

#[derive(clap::Args)]
pub(crate) struct ShellArgs {
    /// Node home to set up or open. Defaults to GENTS_HOME, the folder chosen at setup, then ~/.gents.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    /// As on `init`: tests pass `file` so they never write login-keychain items.
    #[arg(long, value_enum, hide = true)]
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
}
