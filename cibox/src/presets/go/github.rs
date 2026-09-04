use crate::error::Result;
use crate::platforms::github::models::{
    GitHubJob, GitHubStep, GitHubTriggerConfig, GitHubTriggers, GitHubWorkflow,
};
use crate::traits::ToGitHub;
use std::collections::BTreeMap;

use super::GoApp;

impl ToGitHub for GoApp {
    fn to_github(&self) -> Result<GitHubWorkflow> {
        let mut jobs = BTreeMap::new();

        // Test job (always present)
        let test_steps = vec![
            GitHubStep {
                name: Some("Checkout code".to_string()),
                uses: Some("actions/checkout@v4".to_string()),
                run: None,
                with: None,
                env: None,
            },
            GitHubStep {
                name: Some("Setup Go".to_string()),
                uses: Some("actions/setup-go@v5".to_string()),
                run: None,
                with: Some(BTreeMap::from([(
                    "go-version".to_string(),
                    serde_yaml::Value::String(self.go_version.clone()),
                )])),
                env: None,
            },
            GitHubStep {
                name: Some("Download dependencies".to_string()),
                uses: None,
                run: Some("go mod download".to_string()),
                with: None,
                env: None,
            },
            GitHubStep {
                name: Some("Run tests".to_string()),
                uses: None,
                run: Some("go test -v ./...".to_string()),
                with: None,
                env: None,
            },
            GitHubStep {
                name: Some("Build".to_string()),
                uses: None,
                run: Some("go build -v ./...".to_string()),
                with: None,
                env: None,
            },
        ];

        jobs.insert(
            "go-test".to_string(),
            GitHubJob {
                runs_on: "ubuntu-latest".to_string(),
                steps: test_steps,
                needs: None,
                timeout_minutes: Some(30),
                continue_on_error: None,
            },
        );

        // Lint job (optional)
        if self.enable_linter {
            jobs.insert(
                "go-lint".to_string(),
                GitHubJob {
                    runs_on: "ubuntu-latest".to_string(),
                    steps: vec![
                        GitHubStep {
                            name: Some("Checkout code".to_string()),
                            uses: Some("actions/checkout@v4".to_string()),
                            run: None,
                            with: None,
                            env: None,
                        },
                        GitHubStep {
                            name: Some("Setup Go".to_string()),
                            uses: Some("actions/setup-go@v5".to_string()),
                            run: None,
                            with: Some(BTreeMap::from([(
                                "go-version".to_string(),
                                serde_yaml::Value::String(self.go_version.clone()),
                            )])),
                            env: None,
                        },
                        GitHubStep {
                            name: Some("Run golangci-lint".to_string()),
                            uses: Some("golangci/golangci-lint-action@v3".to_string()),
                            run: None,
                            with: Some(BTreeMap::from([(
                                "version".to_string(),
                                serde_yaml::Value::String("latest".to_string()),
                            )])),
                            env: None,
                        },
                    ],
                    needs: None,
                    timeout_minutes: Some(15),
                    continue_on_error: None,
                },
            );
        }

        // Security scan job (optional)
        if self.enable_security_scan {
            jobs.insert(
                "go-security".to_string(),
                GitHubJob {
                    runs_on: "ubuntu-latest".to_string(),
                    steps: vec![
                        GitHubStep {
                            name: Some("Checkout code".to_string()),
                            uses: Some("actions/checkout@v4".to_string()),
                            run: None,
                            with: None,
                            env: None,
                        },
                        GitHubStep {
                            name: Some("Setup Go".to_string()),
                            uses: Some("actions/setup-go@v5".to_string()),
                            run: None,
                            with: Some(BTreeMap::from([(
                                "go-version".to_string(),
                                serde_yaml::Value::String(self.go_version.clone()),
                            )])),
                            env: None,
                        },
                        GitHubStep {
                            name: Some("Run gosec".to_string()),
                            uses: Some("securego/gosec@master".to_string()),
                            run: None,
                            with: Some(BTreeMap::from([(
                                "args".to_string(),
                                serde_yaml::Value::String("./...".to_string()),
                            )])),
                            env: None,
                        },
                    ],
                    needs: None,
                    timeout_minutes: Some(10),
                    continue_on_error: None,
                },
            );
        }

        Ok(GitHubWorkflow {
            name: "CI".to_string(),
            on: GitHubTriggers::Detailed(BTreeMap::from([
                (
                    "push".to_string(),
                    GitHubTriggerConfig {
                        branches: Some(vec!["main".to_string(), "master".to_string()]),
                        tags: None,
                    },
                ),
                (
                    "pull_request".to_string(),
                    GitHubTriggerConfig {
                        branches: Some(vec!["main".to_string(), "master".to_string()]),
                        tags: None,
                    },
                ),
            ])),
            env: None,
            jobs,
        })
    }
}
