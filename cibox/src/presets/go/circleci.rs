use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::traits::ToCircleCI;

use super::GoApp;

impl ToCircleCI for GoApp {
    fn to_circleci(&self) -> Result<CircleCIConfig> {
        use crate::platforms::circleci::models::*;
        use std::collections::BTreeMap;

        let mut steps = vec![CircleCIStep::Simple("checkout".to_string())];

        if self.enable_security_scan {
            steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Security scan".to_string(),
                    command: "gosec ./...".to_string(),
                },
            });
        }

        if self.enable_linter {
            steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Lint".to_string(),
                    command: "golangci-lint run".to_string(),
                },
            });
        }

        steps.push(CircleCIStep::Command {
            run: CircleCIRun::Detailed {
                name: "Run tests".to_string(),
                command: "go test -v ./...".to_string(),
            },
        });

        let mut jobs = BTreeMap::new();
        jobs.insert(
            "go-test".to_string(),
            CircleCIJob {
                docker: vec![CircleCIDocker {
                    image: format!("golang:{}", self.go_version),
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
                    jobs: vec![CircleCIWorkflowJob::Simple("go-test".to_string())],
                },
            )]),
        })
    }
}
