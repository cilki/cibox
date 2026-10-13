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
/// complete them. Several may be given to generate for several platforms in
/// one pass; none means "infer".
#[derive(Args)]
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
    /// The platforms named on the command line, in a stable order. Empty means
    /// none was named and the caller should infer one.
    pub fn selected(&self) -> Vec<Platform> {
        [
            (self.github, Platform::GitHub),
            (self.gitea, Platform::Gitea),
            (self.gitlab, Platform::GitLab),
            (self.circleci, Platform::CircleCI),
        ]
        .into_iter()
        .filter_map(|(given, platform)| given.then_some(platform))
        .collect()
    }

    /// The platform to start the editor on. It previews one platform at a
    /// time, so when several flags are given it opens on the first of them in
    /// flag order — and its platform menu can move off that either way.
    pub fn first(&self) -> Option<Platform> {
        self.selected().first().copied()
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

        // Target platforms; repeat the flags to generate for several at once.
        // The default is one platform, inferred from existing CI files or the
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

    fn update_platforms(args: &[&str]) -> Vec<Platform> {
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
                update_platforms(&[&flag]),
                vec![platform],
                "{flag} should select {platform}"
            );
        }
    }

    #[test]
    fn test_no_flag_leaves_the_platform_to_inference() {
        assert!(update_platforms(&[]).is_empty());
    }

    #[test]
    fn test_several_platforms_are_all_selected() {
        // Generating for two platforms at once is a request, not a mistake:
        // nothing about the resolution is platform-specific
        assert_eq!(
            update_platforms(&["--github", "--gitlab"]),
            vec![Platform::GitHub, Platform::GitLab]
        );
        assert_eq!(update_platforms(&["--circleci", "--gitea"]).len(), 2);
    }

    #[test]
    fn test_every_platform_at_once_is_accepted() {
        let flags: Vec<String> = Platform::all()
            .into_iter()
            .map(|p| format!("--{}", p.to_string().to_lowercase()))
            .collect();
        let refs: Vec<&str> = flags.iter().map(String::as_str).collect();
        assert_eq!(update_platforms(&refs), Platform::all().to_vec());
    }

    #[test]
    fn test_editor_opens_on_one_platform() {
        // The editor previews one platform at a time, so several flags collapse
        // to the first in flag order rather than failing the launch
        let argv = ["cibox", "editor", "--gitlab", "--github"];
        match Cli::try_parse_from(argv).expect("parses").command {
            Some(Commands::Editor { platform, .. }) => {
                assert_eq!(platform.first(), Some(Platform::GitHub))
            }
            other => panic!("expected editor, got {}", other.is_some()),
        }
    }

    #[test]
    fn test_cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}
