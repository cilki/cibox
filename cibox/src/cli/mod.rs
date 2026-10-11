pub mod commands;

use crate::config::Platform;
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cibox")]
#[command(about = "Control your CI/CD configuration")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// One flag per platform, so `--help` lists the choices and the shell can
/// complete them. At most one may be given; none means "infer".
#[derive(Args)]
#[group(multiple = false)]
pub struct PlatformFlag {
    /// Target GitHub Actions
    #[arg(long)]
    pub github: bool,

    /// Target Gitea Actions
    #[arg(long)]
    pub gitea: bool,

    /// Target GitLab CI
    #[arg(long)]
    pub gitlab: bool,

    /// Target CircleCI
    #[arg(long)]
    pub circleci: bool,
}

impl PlatformFlag {
    /// The platform named on the command line, if any
    pub fn selected(&self) -> Option<Platform> {
        [
            (self.github, Platform::GitHub),
            (self.gitea, Platform::Gitea),
            (self.gitlab, Platform::GitLab),
            (self.circleci, Platform::CircleCI),
        ]
        .into_iter()
        .find_map(|(given, platform)| given.then_some(platform))
    }
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

        // Target platform; default is inferred from existing CI files or the
        // git remote
        #[command(flatten)]
        platform: PlatformFlag,

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

        // Platform to open on; default is inferred, and it can be changed from
        // inside the editor either way
        #[command(flatten)]
        platform: PlatformFlag,
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn update_platform(args: &[&str]) -> Option<Platform> {
        let argv = [&["cibox", "update"], args].concat();
        match Cli::try_parse_from(argv).expect("parses").command {
            Some(Commands::Update { platform, .. }) => platform.selected(),
            other => panic!("expected update, got {}", other.is_some()),
        }
    }

    #[test]
    fn test_each_platform_has_a_flag() {
        for platform in Platform::all() {
            let flag = format!("--{}", platform.to_string().to_lowercase());
            assert_eq!(
                update_platform(&[&flag]),
                Some(platform),
                "{flag} should select {platform}"
            );
        }
    }

    #[test]
    fn test_no_flag_leaves_the_platform_to_inference() {
        assert_eq!(update_platform(&[]), None);
    }

    #[test]
    fn test_two_platforms_are_rejected() {
        // Lowering targets one platform, so the group must not let two through
        // and silently pick a winner
        let err = match Cli::try_parse_from(["cibox", "update", "--github", "--gitlab"]) {
            Err(e) => e,
            Ok(_) => panic!("two platforms must not parse"),
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn test_cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}
