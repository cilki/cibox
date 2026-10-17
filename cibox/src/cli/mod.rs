pub mod commands;

use crate::config::Platform;
use clap::{Args, Parser, Subcommand};

/// Which platforms `update` writes. Any combination may be given; with none,
/// every platform that already has a cibox-managed pipeline is updated, or
/// the inferred one (existing CI, then the git remote, then GitHub) when
/// there is none yet.
#[derive(Args, Debug, Default)]
#[group(multiple = true)]
pub struct PlatformFlags {
    /// Write GitHub Actions workflows (.github/workflows/{ci,release}.yml)
    #[arg(long)]
    pub github: bool,
    /// Write Gitea Actions workflows (.gitea/workflows/{ci,release}.yml)
    #[arg(long)]
    pub gitea: bool,
    /// Write GitLab CI config (.gitlab-ci.yml)
    #[arg(long)]
    pub gitlab: bool,
    /// Write CircleCI config (.circleci/config.yml)
    #[arg(long)]
    pub circleci: bool,
}

impl PlatformFlags {
    /// The platforms named, in canonical order; empty when no flag was given
    pub fn selected(&self) -> Vec<Platform> {
        [
            (self.github, Platform::GitHub),
            (self.gitea, Platform::Gitea),
            (self.gitlab, Platform::GitLab),
            (self.circleci, Platform::CircleCI),
        ]
        .into_iter()
        .filter_map(|(on, platform)| on.then_some(platform))
        .collect()
    }
}

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
    ///
    /// Without a platform flag, every platform that already has a
    /// cibox-managed file is updated; with none on disk yet, the platform is
    /// inferred from existing CI config, then the git remote, then GitHub.
    /// Platform flags can be combined.
    Update {
        /// Path to cibox.ron override file (optional)
        #[arg(default_value = "cibox.ron")]
        config: String,

        #[command(flatten)]
        targets: PlatformFlags,

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
