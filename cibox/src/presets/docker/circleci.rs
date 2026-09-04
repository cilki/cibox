use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::traits::ToCircleCI;
use std::collections::BTreeMap;

use super::{Docker, DockerRegistry};

impl ToCircleCI for Docker {
    fn to_circleci(&self) -> Result<CircleCIConfig> {
        use crate::platforms::circleci::models::*;

        let mut steps = vec![CircleCIStep::Simple("checkout".to_string())];

        // Setup remote Docker
        steps.push(CircleCIStep::Simple("setup_remote_docker".to_string()));

        // Login based on registry
        match &self.registry {
            DockerRegistry::DockerHub => {
                steps.push(CircleCIStep::Command {
                    run: CircleCIRun::Simple(
                        "echo $DOCKER_PASSWORD | docker login -u $DOCKER_USERNAME --password-stdin"
                            .to_string(),
                    ),
                });
            }
            DockerRegistry::GitHubRegistry => {
                steps.push(CircleCIStep::Command {
                    run: CircleCIRun::Simple(
                        "echo $GITHUB_TOKEN | docker login ghcr.io -u $GITHUB_USERNAME --password-stdin"
                            .to_string(),
                    ),
                });
            }
            DockerRegistry::None => {}
        }

        // Build command
        let mut build_cmd = format!(
            "docker build -t {} -f {}",
            self.image_name, self.dockerfile_path
        );

        // Build args removed for simplicity

        build_cmd.push_str(&format!(" {}", self.build_context));

        steps.push(CircleCIStep::Command {
            run: CircleCIRun::Detailed {
                name: "Build Docker image".to_string(),
                command: build_cmd,
            },
        });

        // Push if registry configured
        if self.registry != DockerRegistry::None {
            steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Push Docker image".to_string(),
                    command: format!("docker push {}", self.image_name),
                },
            });
        }

        let mut jobs = BTreeMap::new();
        jobs.insert(
            "docker-build".to_string(),
            CircleCIJob {
                docker: vec![CircleCIDocker {
                    image: "cimg/base:stable".to_string(),
                }],
                steps,
                environment: None,
            },
        );

        Ok(CircleCIConfig {
            version: "2.1".to_string(),
            orbs: None,
            jobs,
            workflows: BTreeMap::from([(
                "main".to_string(),
                CircleCIWorkflow {
                    jobs: vec![CircleCIWorkflowJob::Simple("docker-build".to_string())],
                },
            )]),
        })
    }
}
