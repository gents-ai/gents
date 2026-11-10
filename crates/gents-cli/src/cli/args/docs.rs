use std::path::PathBuf;

#[derive(clap::Args)]
pub(crate) struct DocsBundleArgs {
    /// Missing or empty directory that receives the bundle files.
    #[arg(long, value_name = "DIR")]
    pub(crate) out: PathBuf,
    /// Also write the bundle as a gzipped tar with the files at its root.
    #[arg(long, value_name = "FILE")]
    pub(crate) archive: Option<PathBuf>,
    /// Release tag the bundle documents; defaults to the tag this binary was built at.
    #[arg(long, value_name = "TAG")]
    pub(crate) tag: Option<String>,
    /// Full commit SHA the bundle documents; defaults to the commit this binary was built from.
    #[arg(long, value_name = "SHA")]
    pub(crate) commit: Option<String>,
}
