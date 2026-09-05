use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::platforms::gitea::models::GiteaWorkflow;
use crate::platforms::github::models::GitHubWorkflow;
use crate::platforms::gitlab::models::GitLabCI;
use crate::platforms::jenkins::models::JenkinsConfig;

/// Trait for converting a preset to GitHub Actions workflow
pub trait ToGitHub {
    fn to_github(&self) -> Result<GitHubWorkflow>;
}

/// Trait for converting a preset to Gitea Actions workflow
pub trait ToGitea {
    fn to_gitea(&self) -> Result<GiteaWorkflow>;
}

/// Trait for converting a preset to GitLab CI config
pub trait ToGitLab {
    fn to_gitlab(&self) -> Result<GitLabCI>;
}

/// Trait for converting a preset to CircleCI config
pub trait ToCircleCI {
    fn to_circleci(&self) -> Result<CircleCIConfig>;
}

/// Trait for converting a preset to Jenkins pipeline
pub trait ToJenkins {
    fn to_jenkins(&self) -> Result<JenkinsConfig>;
}
