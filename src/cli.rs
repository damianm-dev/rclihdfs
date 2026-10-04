use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rclihdfs",
    about = "Tool to move/copy/remove/mkdir in HDFS with Kerberos-aware auth switch"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// copy data from one hdfs path to another
    Cp { source: String, target: String },
    /// move data from one hdfs path to another
    Mv {
        source: String,
        target: String,
        /// skip the confirmation prompt
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// move data to trash
    Rm {
        source: String,
        /// skip the confirmation prompt
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// create directory (mkdir -p)
    Mkdir { path: String },
}
