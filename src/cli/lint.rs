use camino::Utf8PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Lint Haskell code
    Haskell(HaskellArgs),
}

#[derive(clap::Args)]
pub struct HaskellArgs {
    /// Only lint specific paths
    #[arg(group = "input")]
    pub paths: Vec<Utf8PathBuf>,

    /// Lint code piped to `stdin`
    #[arg(long, group = "input")]
    pub stdin: bool,
}
