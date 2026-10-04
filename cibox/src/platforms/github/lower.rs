use crate::ir::{Job, Step};
use crate::platforms::github::models::{
    GitHubJob, GitHubStep, GitHubTriggerConfig, GitHubTriggers, GitHubWorkflow,
};
use std::collections::BTreeMap;

/// Which workflow file is being produced. CI runs on branch pushes and pull
/// requests; Release runs only on version tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowKind {
    Ci,
    Release,
}

pub fn lower_github(jobs: &[Job], kind: WorkflowKind) -> GitHubWorkflow {
    let lowered = jobs
        .iter()
        .map(|job| (job.id.clone(), lower_job(job, kind)))
        .collect();

    let on = match kind {
        WorkflowKind::Ci => {
            let branches = Some(vec!["main".to_string(), "master".to_string()]);
            GitHubTriggers::Detailed(BTreeMap::from([
                (
                    "push".to_string(),
                    GitHubTriggerConfig {
                        branches: branches.clone(),
                        tags: None,
                    },
                ),
                (
                    "pull_request".to_string(),
                    GitHubTriggerConfig {
                        branches,
                        tags: None,
                    },
                ),
            ]))
        }
        WorkflowKind::Release => GitHubTriggers::Detailed(BTreeMap::from([(
            "push".to_string(),
            GitHubTriggerConfig {
                branches: None,
                tags: Some(vec!["v*".to_string()]),
            },
        )])),
    };

    GitHubWorkflow {
        name: match kind {
            WorkflowKind::Ci => "CI".to_string(),
            WorkflowKind::Release => "Release".to_string(),
        },
        on,
        env: None,
        jobs: lowered,
    }
}

fn lower_job(job: &Job, kind: WorkflowKind) -> GitHubJob {
    let mut steps = Vec::new();

    for step in &job.steps {
        match step {
            Step::Checkout { full_history } => {
                steps.push(GitHubStep {
                    name: Some("Checkout code".to_string()),
                    uses: Some("actions/checkout@v4".to_string()),
                    run: None,
                    with: full_history.then(|| {
                        BTreeMap::from([(
                            "fetch-depth".to_string(),
                            serde_yaml::Value::Number(0.into()),
                        )])
                    }),
                    env: None,
                });
                if let Some(cache) = &job.cache {
                    steps.push(GitHubStep {
                        name: Some("Cache".to_string()),
                        uses: Some("actions/cache@v4".to_string()),
                        run: None,
                        with: Some(BTreeMap::from([
                            (
                                "path".to_string(),
                                serde_yaml::Value::String(cache.paths.join("\n")),
                            ),
                            (
                                "key".to_string(),
                                serde_yaml::Value::String(cache.key.clone()),
                            ),
                        ])),
                        env: None,
                    });
                }
            }
            Step::Run { name, command } => {
                steps.push(GitHubStep {
                    name: Some(name.clone()),
                    uses: None,
                    run: Some(command.clone()),
                    with: None,
                    env: None,
                });
            }
        }
    }

    for path in &job.artifacts {
        steps.push(GitHubStep {
            name: Some("Upload artifacts".to_string()),
            uses: Some("actions/upload-artifact@v4".to_string()),
            run: None,
            with: Some(BTreeMap::from([
                (
                    "name".to_string(),
                    serde_yaml::Value::String(format!("{}-artifacts", job.id)),
                ),
                ("path".to_string(), serde_yaml::Value::String(path.clone())),
            ])),
            env: None,
        });
    }

    let mut env: BTreeMap<String, String> = job.env.iter().cloned().collect();
    for secret in &job.secrets {
        env.insert(secret.clone(), format!("${{{{ secrets.{secret} }}}}"));
    }

    GitHubJob {
        runs_on: "ubuntu-latest".to_string(),
        // Docker-daemon jobs run directly on the host runner
        container: if job.needs_docker {
            None
        } else {
            job.image.clone()
        },
        env: (!env.is_empty()).then_some(env),
        steps,
        needs: (!job.needs.is_empty()).then(|| job.needs.clone()),
        timeout_minutes: job.timeout_minutes,
        continue_on_error: None,
        // A tags-only job inside a mixed CI workflow must not run on
        // branch pushes or PRs; the release workflow is already tag-gated
        if_expr: (job.tags_only && kind == WorkflowKind::Ci)
            .then(|| "startsWith(github.ref, 'refs/tags/v')".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, Stage, Step};

    #[test]
    fn test_job_keys_are_rule_ids() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)],
            WorkflowKind::Ci,
        );
        assert!(workflow.jobs.contains_key("rust-test"));
    }

    #[test]
    fn test_checkout_full_history() {
        let workflow = lower_github(
            &[Job::new("gitleaks", "Gitleaks", Stage::Security)
                .with_steps(vec![Step::checkout_full_history()])],
            WorkflowKind::Ci,
        );
        let step = &workflow.jobs["gitleaks"].steps[0];
        assert_eq!(step.uses.as_deref(), Some("actions/checkout@v4"));
        assert_eq!(
            step.with.as_ref().unwrap()["fetch-depth"],
            serde_yaml::Value::Number(0.into())
        );
    }

    #[test]
    fn test_container_and_docker() {
        let workflow = lower_github(
            &[
                Job::new("rust-test", "Cargo test", Stage::Test).with_image("rust:1.80"),
                Job::new("docker-build", "Docker build", Stage::Build)
                    .with_image("ignored")
                    .with_docker(),
            ],
            WorkflowKind::Ci,
        );
        assert_eq!(
            workflow.jobs["rust-test"].container.as_deref(),
            Some("rust:1.80")
        );
        assert_eq!(workflow.jobs["docker-build"].container, None);
    }

    #[test]
    fn test_secrets_become_job_env() {
        let workflow = lower_github(
            &[Job::new("docker-release", "Docker push", Stage::Deploy)
                .with_secrets(vec!["DOCKER_PASSWORD".to_string()])],
            WorkflowKind::Release,
        );
        assert_eq!(
            workflow.jobs["docker-release"].env.as_ref().unwrap()["DOCKER_PASSWORD"],
            "${{ secrets.DOCKER_PASSWORD }}"
        );
    }

    #[test]
    fn test_release_workflow_triggers_on_tags_only() {
        let workflow = lower_github(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Release,
        );
        let GitHubTriggers::Detailed(triggers) = &workflow.on else {
            panic!("expected detailed triggers");
        };
        assert_eq!(workflow.name, "Release");
        assert_eq!(triggers["push"].tags, Some(vec!["v*".to_string()]));
        assert_eq!(triggers["push"].branches, None);
        assert!(!triggers.contains_key("pull_request"));
        // No per-job guard needed in a tag-gated workflow
        assert_eq!(workflow.jobs["rust-release"].if_expr, None);
    }

    #[test]
    fn test_tags_only_job_in_ci_workflow_gets_if_guard() {
        let workflow = lower_github(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Ci,
        );
        assert_eq!(
            workflow.jobs["rust-release"].if_expr.as_deref(),
            Some("startsWith(github.ref, 'refs/tags/v')")
        );
    }

    #[test]
    fn test_cache_lowered_after_checkout() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])],
            WorkflowKind::Ci,
        );
        let steps = &workflow.jobs["rust-test"].steps;
        assert_eq!(steps[1].uses.as_deref(), Some("actions/cache@v4"));
    }
}
