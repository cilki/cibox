use crate::ir::{Job, PresetJobs, Step};
use crate::platforms::github::models::{
    GitHubJob, GitHubStep, GitHubTriggerConfig, GitHubTriggers, GitHubWorkflow,
};
use crate::platforms::lower::job_key;
use std::collections::BTreeMap;

pub fn lower_github(presets: &[PresetJobs]) -> GitHubWorkflow {
    let mut jobs = BTreeMap::new();
    let mut any_tags_only = false;

    for preset in presets {
        for job in &preset.jobs {
            any_tags_only |= job.tags_only;
            jobs.insert(job_key(&preset.slug, &job.id), lower_job(&preset.slug, job));
        }
    }

    let branches = Some(vec!["main".to_string(), "master".to_string()]);
    GitHubWorkflow {
        name: "CI".to_string(),
        on: GitHubTriggers::Detailed(BTreeMap::from([
            (
                "push".to_string(),
                GitHubTriggerConfig {
                    branches: branches.clone(),
                    tags: any_tags_only.then(|| vec!["v*".to_string()]),
                },
            ),
            (
                "pull_request".to_string(),
                GitHubTriggerConfig {
                    branches,
                    tags: None,
                },
            ),
        ])),
        env: None,
        jobs,
    }
}

fn lower_job(slug: &str, job: &Job) -> GitHubJob {
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
        needs: (!job.needs.is_empty())
            .then(|| job.needs.iter().map(|n| job_key(slug, n)).collect()),
        timeout_minutes: job.timeout_minutes,
        continue_on_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, Stage, Step};

    fn preset(jobs: Vec<Job>) -> PresetJobs {
        PresetJobs::new("Rust", "Rust", jobs)
    }

    #[test]
    fn test_job_keys_are_prefixed() {
        let workflow = lower_github(&[preset(vec![Job::new("test", "Test", Stage::Test)])]);
        assert!(workflow.jobs.contains_key("rust-test"));
    }

    #[test]
    fn test_checkout_full_history() {
        let workflow = lower_github(&[preset(vec![Job::new("test", "Test", Stage::Test)
            .with_steps(vec![Step::checkout_full_history()])])]);
        let step = &workflow.jobs["rust-test"].steps[0];
        assert_eq!(step.uses.as_deref(), Some("actions/checkout@v4"));
        assert_eq!(
            step.with.as_ref().unwrap()["fetch-depth"],
            serde_yaml::Value::Number(0.into())
        );
    }

    #[test]
    fn test_container_and_docker() {
        let workflow = lower_github(&[preset(vec![
            Job::new("test", "Test", Stage::Test).with_image("rust:1.80"),
            Job::new("build", "Build", Stage::Build)
                .with_image("ignored")
                .with_docker(),
        ])]);
        assert_eq!(
            workflow.jobs["rust-test"].container.as_deref(),
            Some("rust:1.80")
        );
        assert_eq!(workflow.jobs["rust-build"].container, None);
    }

    #[test]
    fn test_secrets_become_job_env() {
        let workflow = lower_github(&[preset(vec![Job::new("build", "Build", Stage::Build)
            .with_secrets(vec!["DOCKER_PASSWORD".to_string()])])]);
        assert_eq!(
            workflow.jobs["rust-build"].env.as_ref().unwrap()["DOCKER_PASSWORD"],
            "${{ secrets.DOCKER_PASSWORD }}"
        );
    }

    #[test]
    fn test_tags_only_adds_tag_trigger() {
        let workflow =
            lower_github(&[preset(vec![Job::new("build", "Build", Stage::Build).tags_only()])]);
        let GitHubTriggers::Detailed(triggers) = &workflow.on else {
            panic!("expected detailed triggers");
        };
        assert_eq!(triggers["push"].tags, Some(vec!["v*".to_string()]));
        assert_eq!(triggers["pull_request"].tags, None);
    }

    #[test]
    fn test_cache_lowered_after_checkout() {
        let workflow = lower_github(&[preset(vec![Job::new("test", "Test", Stage::Test)
            .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
            .with_cache("rust-cache", vec!["target/".to_string()])])]);
        let steps = &workflow.jobs["rust-test"].steps;
        assert_eq!(steps[1].uses.as_deref(), Some("actions/cache@v4"));
    }
}
