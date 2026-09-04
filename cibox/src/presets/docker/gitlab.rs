use crate::error::Result;
use crate::platforms::gitlab::models::GitLabCI;
use crate::traits::ToGitLab;
use std::collections::BTreeMap;

use super::{Docker, DockerRegistry};

impl ToGitLab for Docker {
    fn to_gitlab(&self) -> Result<GitLabCI> {
        use crate::platforms::gitlab::models::*;

        let mut jobs = BTreeMap::new();

        let mut script = vec![
            "docker login -u $CI_REGISTRY_USER -p $CI_REGISTRY_PASSWORD $CI_REGISTRY".to_string(),
        ];

        // Prepare build command
        let mut build_cmd = format!(
            "docker build -t {} -f {}",
            self.image_name, self.dockerfile_path
        );

        // Build args removed for simplicity

        build_cmd.push_str(&format!(" {}", self.build_context));
        script.push(build_cmd);

        // Push if registry is configured
        if self.registry != DockerRegistry::None {
            script.push(format!("docker push {}", self.image_name));
        }

        jobs.insert(
            "docker-build".to_string(),
            GitLabJob {
                stage: "build".to_string(),
                image: Some("docker:latest".to_string()),
                script,
                before_script: None,
                after_script: None,
                needs: None,
                cache: None,
                artifacts: None,
                only: if self.push_on_tags_only {
                    Some(GitLabOnly {
                        refs: Some(vec!["tags".to_string()]),
                    })
                } else {
                    None
                },
                timeout: None,
            },
        );

        Ok(GitLabCI {
            stages: Some(vec!["build".to_string()]),
            variables: None,
            cache: None,
            jobs,
        })
    }
}
