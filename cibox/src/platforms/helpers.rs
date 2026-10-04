use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::platforms::gitea::models::GiteaWorkflow;
use crate::platforms::github::models::GitHubWorkflow;
use crate::platforms::gitlab::models::GitLabCI;

/// A platform-specific CI configuration before serialization, so multiple
/// presets can be merged into one document for single-file platforms
#[derive(Debug, Clone)]
pub enum PlatformConfig {
    GitHub(GitHubWorkflow),
    Gitea(GiteaWorkflow),
    GitLab(GitLabCI),
    CircleCI(CircleCIConfig),
}

impl PlatformConfig {
    /// Serialize to the platform's YAML file format
    pub fn render(&self) -> Result<String> {
        match self {
            PlatformConfig::GitHub(workflow) => Ok(serde_yaml::to_string(workflow)?),
            PlatformConfig::Gitea(workflow) => Ok(serde_yaml::to_string(workflow)?),
            PlatformConfig::GitLab(config) => Ok(serde_yaml::to_string(config)?),
            PlatformConfig::CircleCI(config) => Ok(serde_yaml::to_string(config)?),
        }
    }
}
