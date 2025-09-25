use camino::Utf8PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Format Haskell code
    Haskell(HaskellArgs),

    /// Format Nix code
    Nix(NixArgs),
}

#[derive(clap::Args)]
pub struct HaskellArgs {
    /// Only format specific paths
    #[arg(group = "input")]
    pub paths: Vec<Utf8PathBuf>,

    /// Format code piped to `stdin`
    #[arg(long, group = "input")]
    pub stdin: bool,
}

#[derive(clap::Args)]
pub struct NixArgs {
    /// Only format specific paths
    #[arg(group = "input")]
    pub paths: Vec<Utf8PathBuf>,

    /// Format code piped to `stdin`
    #[arg(long, group = "input")]
    pub stdin: bool,
}
