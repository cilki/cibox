use crate::error::Result;
use crate::platforms::github::models::{
    GitHubJob, GitHubStep, GitHubTriggerConfig, GitHubTriggers, GitHubWorkflow,
};
use crate::traits::ToGitHub;
use std::collections::BTreeMap;

use super::{Docker, DockerRegistry};

impl ToGitHub for Docker {
    fn to_github(&self) -> Result<GitHubWorkflow> {
        let mut jobs = BTreeMap::new();

        // Build and optionally push Docker image
        let mut build_steps = vec![GitHubStep {
            name: Some("Checkout code".to_string()),
            uses: Some("actions/checkout@v4".to_string()),
            run: None,
            with: None,
            env: None,
        }];

        // Add Docker buildx setup for better caching and multi-platform support
        build_steps.push(GitHubStep {
            name: Some("Set up Docker Buildx".to_string()),
            uses: Some("docker/setup-buildx-action@v3".to_string()),
            run: None,
            with: None,
            env: None,
        });

        // Login step depends on registry type
        match &self.registry {
            DockerRegistry::DockerHub => {
                build_steps.push(GitHubStep {
                    name: Some("Login to Docker Hub".to_string()),
                    uses: Some("docker/login-action@v3".to_string()),
                    run: None,
                    with: Some(BTreeMap::from([
                        (
                            "username".to_string(),
                            serde_yaml::Value::String("${{ secrets.DOCKER_USERNAME }}".to_string()),
                        ),
                        (
                            "password".to_string(),
                            serde_yaml::Value::String("${{ secrets.DOCKER_PASSWORD }}".to_string()),
                        ),
                    ])),
                    env: None,
                });
            }
            DockerRegistry::GitHubRegistry => {
                build_steps.push(GitHubStep {
                    name: Some("Login to GitHub Container Registry".to_string()),
                    uses: Some("docker/login-action@v3".to_string()),
                    run: None,
                    with: Some(BTreeMap::from([
                        (
                            "registry".to_string(),
                            serde_yaml::Value::String("ghcr.io".to_string()),
                        ),
                        (
                            "username".to_string(),
                            serde_yaml::Value::String("${{ github.actor }}".to_string()),
                        ),
                        (
                            "password".to_string(),
                            serde_yaml::Value::String("${{ secrets.GITHUB_TOKEN }}".to_string()),
                        ),
                    ])),
                    env: None,
                });
            }
            DockerRegistry::None => {}
        }

        // Extract metadata for tags and labels
        build_steps.push(GitHubStep {
            name: Some("Extract Docker metadata".to_string()),
            uses: Some("docker/metadata-action@v5".to_string()),
            run: None,
            with: Some(BTreeMap::from([
                (
                    "images".to_string(),
                    serde_yaml::Value::String(match &self.registry {
                        DockerRegistry::GitHubRegistry => {
                            format!("ghcr.io/${{ github.repository_owner }}/{}", self.image_name)
                        }
                        _ => self.image_name.clone(),
                    }),
                ),
                (
                    "tags".to_string(),
                    serde_yaml::Value::String(
                        "type=ref,event=branch\ntype=ref,event=pr\ntype=semver,pattern={{version}}\ntype=semver,pattern={{major}}.{{minor}}".to_string(),
                    ),
                ),
            ])),
            env: None,
        });

        // Build and push step
        let mut build_push_with = BTreeMap::from([
            (
                "context".to_string(),
                serde_yaml::Value::String(self.build_context.clone()),
            ),
            (
                "file".to_string(),
                serde_yaml::Value::String(self.dockerfile_path.clone()),
            ),
            (
                "tags".to_string(),
                serde_yaml::Value::String("${{ steps.meta.outputs.tags }}".to_string()),
            ),
            (
                "labels".to_string(),
                serde_yaml::Value::String("${{ steps.meta.outputs.labels }}".to_string()),
            ),
        ]);

        // Only push if registry is configured
        if self.registry != DockerRegistry::None {
            build_push_with.insert(
                "push".to_string(),
                serde_yaml::Value::String("true".to_string()),
            );
        }

        // Add caching if enabled
        if self.enable_cache {
            build_push_with.insert(
                "cache-from".to_string(),
                serde_yaml::Value::String("type=gha".to_string()),
            );
            build_push_with.insert(
                "cache-to".to_string(),
                serde_yaml::Value::String("type=gha,mode=max".to_string()),
            );
        }

        // Build args removed for simplicity - can be added later if needed

        build_steps.push(GitHubStep {
            name: Some("Build and push Docker image".to_string()),
            uses: Some("docker/build-push-action@v5".to_string()),
            run: None,
            with: Some(build_push_with),
            env: None,
        });

        jobs.insert(
            "docker-build".to_string(),
            GitHubJob {
                runs_on: "ubuntu-latest".to_string(),
                steps: build_steps,
                needs: None,
                timeout_minutes: Some(30),
                continue_on_error: None,
            },
        );

        // Configure triggers based on push settings
        let triggers = if self.push_on_tags_only {
            GitHubTriggers::Detailed(BTreeMap::from([
                (
                    "push".to_string(),
                    GitHubTriggerConfig {
                        branches: None,
                        tags: Some(vec!["v*".to_string()]),
                    },
                ),
                (
                    "pull_request".to_string(),
                    GitHubTriggerConfig {
                        branches: Some(vec!["main".to_string(), "master".to_string()]),
                        tags: None,
                    },
                ),
            ]))
        } else {
            GitHubTriggers::Detailed(BTreeMap::from([
                (
                    "push".to_string(),
                    GitHubTriggerConfig {
                        branches: Some(vec!["main".to_string(), "master".to_string()]),
                        tags: Some(vec!["v*".to_string()]),
                    },
                ),
                (
                    "pull_request".to_string(),
                    GitHubTriggerConfig {
                        branches: Some(vec!["main".to_string(), "master".to_string()]),
                        tags: None,
                    },
                ),
            ]))
        };

        Ok(GitHubWorkflow {
            name: "Docker Build and Push".to_string(),
            on: triggers,
            env: None,
            jobs,
        })
    }
}
