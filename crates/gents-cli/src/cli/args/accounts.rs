use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::output_format::OutputFormat;
use crate::CLOUD_AFTER_HELP;

#[derive(clap::Args)]
pub(crate) struct CodexAuthProbeArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "chatgpt-codex")]
    pub(crate) provider: String,
    #[arg(
        long,
        default_value_t = 20,
        help = "Maximum number of model slugs to print"
    )]
    pub(crate) max_models: usize,
}

#[derive(clap::Args)]
pub(crate) struct AccountsTargetArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the accounts")]
    pub(crate) node_did: Option<String>,
    #[arg(long, value_parser = ACCOUNT_PROVIDERS, help = "Only accounts of this sign-in provider")]
    pub(crate) provider: Option<String>,
}

pub(super) const ACCOUNT_PROVIDERS: [&str; 3] =
    ["chatgpt-codex", "claude-subscription", "xai-oauth"];
pub(super) const ACCOUNT_HELP: &str = "The account: its label, credential_id or account_ref";

#[derive(Subcommand)]
pub(crate) enum AccountsCommand {
    #[command(
        about = "List accounts, and backends that use no account, with the profiles that use them"
    )]
    List {
        #[command(flatten)]
        target: AccountsTargetArgs,
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
        #[arg(
            long,
            help = "Read current usage even after a recent read; fail when usage cannot be read, as without a running runtime"
        )]
        refresh: bool,
    },
    #[command(about = "Rename an account")]
    Label {
        #[command(flatten)]
        target: AccountsTargetArgs,
        #[arg(help = ACCOUNT_HELP)]
        account: String,
        #[arg(help = "The new label")]
        label: String,
    },
    #[command(
        about = "Disable an account; profiles using it fail until moved or it is signed in again"
    )]
    Disable {
        #[command(flatten)]
        target: AccountsTargetArgs,
        #[arg(help = ACCOUNT_HELP)]
        account: String,
    },
    #[command(
        about = "Delete an account's tokens on this node and the backend sign-in created for it, unless a profile uses it"
    )]
    Remove {
        #[command(flatten)]
        target: AccountsTargetArgs,
        #[arg(help = ACCOUNT_HELP)]
        account: String,
        #[arg(long, help = "Remove without asking")]
        yes: bool,
    },
}

const LOGIN_LABEL_HELP: &str = "Name for the account signed in to: names a new account or renames a stored one (default: the product name, numbered for a second account)";

#[derive(clap::Args)]
pub(crate) struct CodexLoginArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "chatgpt-codex", value_parser = ["chatgpt-codex"], help = "OAuth provider key (only chatgpt-codex)")]
    pub(crate) provider: String,
    #[arg(long, help = LOGIN_LABEL_HELP)]
    pub(crate) label: Option<String>,
    #[arg(long, default_value_t = false, help = "Use ChatGPT device-code login")]
    pub(crate) device_auth: bool,
    #[arg(long, help = "OAuth issuer override for testing")]
    pub(crate) issuer: Option<String>,
    #[arg(long, help = "OAuth client ID override for testing")]
    pub(crate) client_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct GrokAuthProbeArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "xai-oauth")]
    pub(crate) provider: String,
    #[arg(
        long,
        default_value_t = 20,
        help = "Maximum number of model slugs to print"
    )]
    pub(crate) max_models: usize,
}

#[derive(clap::Args)]
pub(crate) struct GrokLoginArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "xai-oauth", value_parser = ["xai-oauth"], help = "OAuth provider key (only xai-oauth)")]
    pub(crate) provider: String,
    #[arg(long, help = LOGIN_LABEL_HELP)]
    pub(crate) label: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ClaudeLoginArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "claude-subscription", value_parser = ["claude-subscription"], help = "OAuth provider key (only claude-subscription)")]
    pub(crate) provider: String,
    #[arg(long, help = LOGIN_LABEL_HELP)]
    pub(crate) label: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Manual-paste login (no localhost callback, no browser)"
    )]
    pub(crate) manual: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Print the login URL instead of opening a browser"
    )]
    pub(crate) no_browser: bool,
    #[arg(long, help = "OAuth client ID override for testing")]
    pub(crate) client_id: Option<String>,
    #[arg(long, help = "OAuth token endpoint override for testing")]
    pub(crate) token_url: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum CloudCommand {
    #[command(
        name = "login",
        about = "Sign in to a gents cloud and store the workspace token in DefraDB",
        after_help = CLOUD_AFTER_HELP
    )]
    Login(CloudLoginArgs),
}

#[derive(clap::Args)]
pub(crate) struct CloudLoginArgs {
    #[arg(
        long,
        env = "GENTS_CLOUD",
        value_name = "HOST",
        help = "gents cloud host to sign in to, for example app.dev.gents.xyz"
    )]
    pub(crate) cloud: String,
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the target gents node")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Node DID that owns the OAuthCredential document")]
    pub(crate) node_did: Option<String>,
    #[arg(long, default_value = "gents-cloud")]
    pub(crate) provider: String,
}
