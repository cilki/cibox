pub mod commands;
pub mod display;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cibox")]
#[command(about = "Control your CI/CD configuration")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Update CI config in place: conform, re-add, and prune cibox-managed
    /// jobs, keeping jobs you added; comment a job out to keep it disabled
    /// but maintained
    Update {
        /// Path to cibox.ron override file (optional)
        #[arg(default_value = "cibox.ron")]
        config: String,

        /// Target platform (default: inferred from existing CI files or the
        /// git remote)
        #[arg(short, long)]
        platform: Option<String>,

        /// Rewrite files completely, discarding customizations
        #[arg(short, long)]
        force: bool,

        /// Touch only this rule's jobs (repeatable); everything else,
        /// including other cibox-managed jobs, is left as-is
        #[arg(long = "rule", value_name = "RULE", conflicts_with = "force")]
        rules: Vec<String>,
    },

    /// Validate cibox.ron and show the resulting rule resolution
    Validate {
        /// Path to cibox.ron config file
        #[arg(default_value = "cibox.ron")]
        config: String,
    },

    /// Run interactive editor (default)
    Editor {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        dir: String,
    },

    /// Show project facts and which rules they trigger
    Detect {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        dir: String,
    },

    /// Serve a language server (LSP) for cibox.ron over stdio
    Lsp,
}
