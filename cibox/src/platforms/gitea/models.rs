// Gitea Actions is compatible with GitHub Actions workflow syntax
// We re-export GitHub Actions models with Gitea-specific type aliases

pub use crate::platforms::github::{
    GitHubJob as GiteaJob, GitHubStep as GiteaStep, GitHubTriggerConfig as GiteaTriggerConfig,
    GitHubTriggers as GiteaTriggers, GitHubWorkflow as GiteaWorkflow,
};
