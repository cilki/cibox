use crate::error::Result;
use crate::platforms::gitlab::models::GitLabCI;
use crate::traits::ToGitLab;
use std::collections::BTreeMap;

use super::Rust;

impl ToGitLab for Rust {
    fn to_gitlab(&self) -> Result<GitLabCI> {
        use crate::platforms::gitlab::models::{
            GitLabArtifacts, GitLabCache, GitLabJob, GitLabOnly,
        };

        let mut jobs = BTreeMap::new();
        let mut stages = vec!["test".to_string()];

        // Test job (always present)
        let mut test_script = vec![
            format!("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {}", self.rust_version),
            "source $HOME/.cargo/env".to_string(),
            "cargo test --all-features".to_string(),
        ];

        if self.enable_coverage {
            test_script.push("cargo install cargo-tarpaulin".to_string());
            test_script.push("cargo tarpaulin --out Xml --all-features".to_string());
        }

        if self.build_release {
            stages.push("build".to_string());
        }

        jobs.insert(
            "rust-test".to_string(),
            GitLabJob {
                stage: "test".to_string(),
                image: Some("rust:latest".to_string()),
                script: test_script,
                before_script: None,
                after_script: None,
                needs: None,
                cache: Some(GitLabCache {
                    key: "rust-cache".to_string(),
                    paths: vec!["target/".to_string(), ".cargo/".to_string()],
                }),
                artifacts: if self.enable_coverage {
                    Some(GitLabArtifacts {
                        paths: vec!["cobertura.xml".to_string()],
                        name: Some("coverage".to_string()),
                    })
                } else {
                    None
                },
                only: Some(GitLabOnly {
                    refs: Some(vec![
                        "main".to_string(),
                        "master".to_string(),
                        "merge_requests".to_string(),
                    ]),
                }),
                timeout: Some("30m".to_string()),
            },
        );

        // Build job (optional)
        if self.build_release {
            jobs.insert(
                "rust-build".to_string(),
                GitLabJob {
                    stage: "build".to_string(),
                    image: Some(format!("rust:{}", self.rust_version)),
                    script: vec!["cargo build --release".to_string()],
                    before_script: None,
                    after_script: None,
                    needs: None,
                    cache: Some(GitLabCache {
                        key: "rust-cache".to_string(),
                        paths: vec!["target/".to_string()],
                    }),
                    artifacts: Some(GitLabArtifacts {
                        paths: vec!["target/release/".to_string()],
                        name: None,
                    }),
                    only: None,
                    timeout: None,
                },
            );
        }

        // Lint job (optional)
        if self.enable_linter {
            if !stages.contains(&"lint".to_string()) {
                stages.push("lint".to_string());
            }
            jobs.insert(
                "rust-lint".to_string(),
                GitLabJob {
                    stage: "lint".to_string(),
                    image: Some("rust:latest".to_string()),
                    script: vec![
                        format!("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {} --component clippy", self.rust_version),
                        "source $HOME/.cargo/env".to_string(),
                        "cargo clippy --all-features -- -D warnings".to_string(),
                    ],
                    before_script: None,
                    after_script: None,
                    needs: None,
                    cache: Some(GitLabCache {
                        key: "rust-cache".to_string(),
                        paths: vec!["target/".to_string(), ".cargo/".to_string()],
                    }),
                    artifacts: None,
                    only: Some(GitLabOnly {
                        refs: Some(vec!["main".to_string(), "master".to_string(), "merge_requests".to_string()]),
                    }),
                    timeout: Some("15m".to_string()),
                },
            );
        }

        // Format check job (optional)
        if self.enable_format_check {
            if !stages.contains(&"lint".to_string()) {
                stages.push("lint".to_string());
            }
            jobs.insert(
                "rust-format".to_string(),
                GitLabJob {
                    stage: "lint".to_string(),
                    image: Some("rust:latest".to_string()),
                    script: vec![
                        format!("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain {} --component rustfmt", self.rust_version),
                        "source $HOME/.cargo/env".to_string(),
                        "cargo fmt -- --check".to_string(),
                    ],
                    before_script: None,
                    after_script: None,
                    needs: None,
                    cache: None,
                    artifacts: None,
                    only: Some(GitLabOnly {
                        refs: Some(vec!["main".to_string(), "master".to_string(), "merge_requests".to_string()]),
                    }),
                    timeout: Some("10m".to_string()),
                },
            );
        }

        // Security scan job (optional)
        if self.enable_security_scan {
            if !stages.contains(&"security".to_string()) {
                stages.push("security".to_string());
            }
            jobs.insert(
                "rust-security".to_string(),
                GitLabJob {
                    stage: "security".to_string(),
                    image: Some("rust:latest".to_string()),
                    script: vec![
                        "cargo install cargo-audit".to_string(),
                        "cargo audit".to_string(),
                    ],
                    before_script: None,
                    after_script: None,
                    needs: None,
                    cache: Some(GitLabCache {
                        key: "cargo-audit-cache".to_string(),
                        paths: vec![".cargo/".to_string()],
                    }),
                    artifacts: None,
                    only: Some(GitLabOnly {
                        refs: Some(vec![
                            "main".to_string(),
                            "master".to_string(),
                            "merge_requests".to_string(),
                        ]),
                    }),
                    timeout: Some("10m".to_string()),
                },
            );
        }

        Ok(GitLabCI {
            stages: Some(stages),
            variables: None,
            cache: None,
            jobs,
        })
    }
}
