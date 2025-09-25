#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Index modules to build dependency graph
    Index,

    /// Module imports
    Imports(QueryArgs),
}

#[derive(clap::Args)]
pub struct QueryArgs {
    /// Module names or paths
    // TODO: Make a type for Haskell module names
    #[arg(group = "input", required = true)]
    pub modules: Vec<String>,

    /// Query code piped to `stdin`
    #[arg(long, group = "input")]
    pub stdin: bool,
}
