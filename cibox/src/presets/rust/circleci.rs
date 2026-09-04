use crate::error::Result;
use crate::platforms::circleci::models::CircleCIConfig;
use crate::traits::ToCircleCI;
use std::collections::BTreeMap;

use super::Rust;

impl ToCircleCI for Rust {
    fn to_circleci(&self) -> Result<CircleCIConfig> {
        use crate::platforms::circleci::models::*;

        let mut jobs = BTreeMap::new();
        let mut workflow_jobs = vec![];

        // Test job (always present)
        let mut test_steps = vec![
            CircleCIStep::Simple("checkout".to_string()),
            CircleCIStep::Cache {
                restore_cache: CircleCICache {
                    keys: vec!["v1-cargo-cache-{{ checksum \"Cargo.lock\" }}".to_string()],
                },
            },
            CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Install Rust".to_string(),
                    command: format!(
                        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {}",
                        self.rust_version
                    ),
                },
            },
            CircleCIStep::Command {
                run: CircleCIRun::Simple("source $HOME/.cargo/env".to_string()),
            },
            CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Run tests".to_string(),
                    command: "cargo test --all-features".to_string(),
                },
            },
        ];

        if self.enable_coverage {
            test_steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Install tarpaulin".to_string(),
                    command: "cargo install cargo-tarpaulin".to_string(),
                },
            });
            test_steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Generate coverage".to_string(),
                    command: "cargo tarpaulin --out Xml --all-features".to_string(),
                },
            });
        }

        if self.build_release {
            test_steps.push(CircleCIStep::Command {
                run: CircleCIRun::Detailed {
                    name: "Build release".to_string(),
                    command: "cargo build --release".to_string(),
                },
            });
        }

        test_steps.push(CircleCIStep::SaveCache {
            save_cache: CircleCICacheSave {
                key: "v1-cargo-cache-{{ checksum \"Cargo.lock\" }}".to_string(),
                paths: vec!["~/.cargo".to_string(), "./target".to_string()],
            },
        });

        jobs.insert(
            "rust-test".to_string(),
            CircleCIJob {
                docker: vec![CircleCIDocker {
                    image: "rust:latest".to_string(),
                }],
                steps: test_steps,
                environment: None,
            },
        );

        workflow_jobs.push(CircleCIWorkflowJob::Simple("rust-test".to_string()));

        // Lint job (optional)
        if self.enable_linter {
            jobs.insert(
                "rust-lint".to_string(),
                CircleCIJob {
                    docker: vec![CircleCIDocker {
                        image: "rust:latest".to_string(),
                    }],
                    steps: vec![
                        CircleCIStep::Simple("checkout".to_string()),
                        CircleCIStep::Command {
                            run: CircleCIRun::Detailed {
                                name: "Install Rust with clippy".to_string(),
                                command: format!(
                                    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {} --component clippy",
                                    self.rust_version
                                ),
                            },
                        },
                        CircleCIStep::Command {
                            run: CircleCIRun::Simple("source $HOME/.cargo/env".to_string()),
                        },
                        CircleCIStep::Command {
                            run: CircleCIRun::Detailed {
                                name: "Run clippy".to_string(),
                                command: "cargo clippy --all-features -- -D warnings".to_string(),
                            },
                        },
                    ],
                    environment: None,
                },
            );
            workflow_jobs.push(CircleCIWorkflowJob::Simple("rust-lint".to_string()));
        }

        // Format check job (optional)
        if self.enable_format_check {
            jobs.insert(
                "rust-format".to_string(),
                CircleCIJob {
                    docker: vec![CircleCIDocker {
                        image: "rust:latest".to_string(),
                    }],
                    steps: vec![
                        CircleCIStep::Simple("checkout".to_string()),
                        CircleCIStep::Command {
                            run: CircleCIRun::Detailed {
                                name: "Install Rust with rustfmt".to_string(),
                                command: format!(
                                    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {} --component rustfmt",
                                    self.rust_version
                                ),
                            },
                        },
                        CircleCIStep::Command {
                            run: CircleCIRun::Simple("source $HOME/.cargo/env".to_string()),
                        },
                        CircleCIStep::Command {
                            run: CircleCIRun::Detailed {
                                name: "Check formatting".to_string(),
                                command: "cargo fmt -- --check".to_string(),
                            },
                        },
                    ],
                    environment: None,
                },
            );
            workflow_jobs.push(CircleCIWorkflowJob::Simple("rust-format".to_string()));
        }

        Ok(CircleCIConfig {
            version: "2.1".to_string(),
            orbs: None,
            jobs,
            workflows: BTreeMap::from([(
                "ci".to_string(),
                CircleCIWorkflow {
                    jobs: workflow_jobs,
                },
            )]),
        })
    }
}
