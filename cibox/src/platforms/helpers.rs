use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::platforms::gitea::models::GiteaWorkflow;
use crate::platforms::github::models::GitHubWorkflow;
use crate::platforms::gitlab::models::GitLabCI;
use crate::platforms::jenkins::models::JenkinsConfig;

/// A platform-specific CI configuration before serialization, so multiple
/// presets can be merged into one document for single-file platforms
#[derive(Debug, Clone)]
pub enum PlatformConfig {
    GitHub(GitHubWorkflow),
    Gitea(GiteaWorkflow),
    GitLab(GitLabCI),
    CircleCI(CircleCIConfig),
    Jenkins(JenkinsConfig),
}

impl PlatformConfig {
    /// Serialize to the platform's file format (YAML, or Groovy for Jenkins)
    pub fn render(&self) -> Result<String> {
        match self {
            PlatformConfig::GitHub(workflow) => Ok(serde_yaml::to_string(workflow)?),
            PlatformConfig::Gitea(workflow) => Ok(serde_yaml::to_string(workflow)?),
            PlatformConfig::GitLab(config) => Ok(serde_yaml::to_string(config)?),
            PlatformConfig::CircleCI(config) => Ok(serde_yaml::to_string(config)?),
            PlatformConfig::Jenkins(config) => Ok(jenkins_to_string(config)),
        }
    }
}

/// Convert a JenkinsConfig to Groovy pipeline string
///
/// Jenkins uses a Groovy-based DSL for its declarative pipelines,
/// so we need custom string formatting instead of YAML serialization.
///
/// # Arguments
/// * `config` - The Jenkins configuration to convert
///
/// # Returns
/// A string containing the Groovy pipeline definition
pub fn jenkins_to_string(config: &JenkinsConfig) -> String {
    let mut result = String::new();
    result.push_str("pipeline {\n");
    result.push_str("    agent {\n");
    result.push_str(&format!("        label '{}'\n", config.agent));
    result.push_str("    }\n\n");

    if !config.environment.is_empty() {
        result.push_str("    environment {\n");
        for (key, value) in &config.environment {
            result.push_str(&format!("        {} = '{}'\n", key, value));
        }
        result.push_str("    }\n\n");
    }

    result.push_str("    stages {\n");
    for stage in &config.stages {
        result.push_str(&format!("        stage('{}') {{\n", stage.name));
        if let Some(tag) = &stage.when_tag {
            result.push_str(&format!("            when {{ tag '{}' }}\n", tag));
        }
        result.push_str("            steps {\n");
        for step in &stage.steps {
            result.push_str(&format!("                {}\n", step));
        }
        result.push_str("            }\n");
        result.push_str("        }\n");
    }
    result.push_str("    }\n");
    result.push_str("}\n");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platforms::jenkins::models::JenkinsStage;

    #[test]
    fn test_jenkins_to_string_basic() {
        let config = JenkinsConfig {
            agent: "docker".to_string(),
            environment: vec![],
            stages: vec![JenkinsStage {
                name: "Build".to_string(),
                steps: vec!["sh 'cargo build'".to_string()],
                when_tag: None,
            }],
        };

        let result = jenkins_to_string(&config);
        assert!(result.contains("pipeline {"));
        assert!(result.contains("agent {"));
        assert!(result.contains("label 'docker'"));
        assert!(result.contains("stage('Build')"));
        assert!(result.contains("sh 'cargo build'"));
    }

    #[test]
    fn test_jenkins_when_tag() {
        let config = JenkinsConfig {
            agent: "any".to_string(),
            environment: vec![],
            stages: vec![JenkinsStage {
                name: "Cargo publish".to_string(),
                steps: vec!["sh 'cargo publish'".to_string()],
                when_tag: Some("v*".to_string()),
            }],
        };

        let result = jenkins_to_string(&config);
        assert!(result.contains("when { tag 'v*' }"), "{result}");
    }

    #[test]
    fn test_jenkins_to_string_with_environment() {
        let env = vec![
            ("RUST_VERSION".to_string(), "1.70".to_string()),
            ("CARGO_HOME".to_string(), "/tmp/cargo".to_string()),
        ];

        let config = JenkinsConfig {
            agent: "any".to_string(),
            environment: env,
            stages: vec![],
        };

        let result = jenkins_to_string(&config);
        assert!(result.contains("environment {"));
        assert!(result.contains("RUST_VERSION = '1.70'"));
        assert!(result.contains("CARGO_HOME = '/tmp/cargo'"));
    }

    #[test]
    fn test_jenkins_to_string_multiple_stages() {
        let config = JenkinsConfig {
            agent: "linux".to_string(),
            environment: vec![],
            stages: vec![
                JenkinsStage {
                    name: "Test".to_string(),
                    steps: vec!["sh 'cargo test'".to_string()],
                    when_tag: None,
                },
                JenkinsStage {
                    name: "Deploy".to_string(),
                    steps: vec![
                        "sh 'docker build .'".to_string(),
                        "sh 'docker push'".to_string(),
                    ],
                    when_tag: None,
                },
            ],
        };

        let result = jenkins_to_string(&config);
        assert!(result.contains("stage('Test')"));
        assert!(result.contains("sh 'cargo test'"));
        assert!(result.contains("stage('Deploy')"));
        assert!(result.contains("sh 'docker build .'"));
        assert!(result.contains("sh 'docker push'"));
    }
}
