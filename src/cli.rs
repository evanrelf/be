pub mod format;
pub mod lint;
pub mod query;

use clap::ArgAction;

#[derive(clap::Parser)]
#[command(disable_help_subcommand = true)]
pub struct Args {
    /// Increase verbosity of output
    #[arg(short, long, action = ArgAction::Count, group = "verbosity")]
    pub verbose: u8,

    /// Increase verbosity of output (expanded)
    #[arg(short = 'V', long = "VERBOSE", action = ArgAction::Count, group = "verbosity")]
    pub verbose_expanded: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Format code
    Format(format::Args),

    /// Lint code
    Lint(lint::Args),

    /// Query Haskell code
    Query(query::Args),
}
