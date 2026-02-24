use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(about = "FUSE passthrough proxy with dynamic file interception")]
pub(crate) struct Config {
    /// Directory to mount over (the real project directory)
    #[arg(long)]
    pub dir: PathBuf,

    /// Path to SQLite database (should be outside the mount tree)
    #[arg(long)]
    pub db: PathBuf,

    /// Glob patterns for files to intercept (e.g. 'CLAUDE.md')
    #[arg(long = "intercept")]
    pub patterns: Vec<String>,
}
