use crate::ir::{Job, PresetJobs, Step};
use crate::platforms::circleci::models::{
    CircleCICache, CircleCICacheSave, CircleCIConfig, CircleCIDocker, CircleCIJob,
    CircleCIJobRequires, CircleCIRun, CircleCIStep, CircleCIStoreArtifacts, CircleCIWorkflow,
    CircleCIWorkflowJob,
};
use crate::platforms::lower::job_key;
use std::collections::BTreeMap;

const DEFAULT_IMAGE: &str = "cimg/base:stable";

pub fn lower_circleci(presets: &[PresetJobs]) -> CircleCIConfig {
    let mut jobs = BTreeMap::new();
    let mut workflow_jobs = Vec::new();

    for preset in presets {
        for job in &preset.jobs {
            let key = job_key(&preset.slug, &job.id);
            jobs.insert(key.clone(), lower_job(job));
            if job.needs.is_empty() {
                workflow_jobs.push(CircleCIWorkflowJob::Simple(key));
            } else {
                workflow_jobs.push(CircleCIWorkflowJob::WithRequires {
                    job: BTreeMap::from([(
                        key,
                        CircleCIJobRequires {
                            requires: job
                                .needs
                                .iter()
                                .map(|n| job_key(&preset.slug, n))
                                .collect(),
                        },
                    )]),
                });
            }
        }
    }

    CircleCIConfig {
        version: "2.1".to_string(),
        orbs: None,
        jobs,
        workflows: BTreeMap::from([(
            "main".to_string(),
            CircleCIWorkflow { jobs: workflow_jobs },
        )]),
    }
}

fn lower_job(job: &Job) -> CircleCIJob {
    let image = if job.needs_docker {
        DEFAULT_IMAGE.to_string()
    } else {
        job.image.clone().unwrap_or_else(|| DEFAULT_IMAGE.to_string())
    };

    let mut steps = Vec::new();
    for step in &job.steps {
        match step {
            // CircleCI clones full history by default
            Step::Checkout { .. } => {
                steps.push(CircleCIStep::Simple("checkout".to_string()));
                if job.needs_docker {
                    steps.push(CircleCIStep::Simple("setup_remote_docker".to_string()));
                }
                if let Some(cache) = &job.cache {
                    steps.push(CircleCIStep::Cache {
                        restore_cache: CircleCICache {
                            keys: vec![cache.key.clone()],
                        },
                    });
                }
            }
            Step::Run { name, command } => {
                steps.push(CircleCIStep::Command {
                    run: CircleCIRun::Detailed {
                        name: name.clone(),
                        command: command.clone(),
                    },
                });
            }
        }
    }

    if let Some(cache) = &job.cache {
        steps.push(CircleCIStep::SaveCache {
            save_cache: CircleCICacheSave {
                key: cache.key.clone(),
                paths: cache.paths.clone(),
            },
        });
    }
    for path in &job.artifacts {
        steps.push(CircleCIStep::StoreArtifacts {
            store_artifacts: CircleCIStoreArtifacts { path: path.clone() },
        });
    }

    let env: BTreeMap<String, String> = job.env.iter().cloned().collect();
    CircleCIJob {
        docker: vec![CircleCIDocker { image }],
        steps,
        environment: (!env.is_empty()).then_some(env),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, Stage, Step};

    #[test]
    fn test_single_main_workflow() {
        let config = lower_circleci(&[
            PresetJobs::new("Rust", "Rust", vec![Job::new("test", "Test", Stage::Test)]),
            PresetJobs::new(
                "Docker",
                "Docker",
                vec![Job::new("build", "Build", Stage::Build)],
            ),
        ]);
        assert_eq!(config.workflows.len(), 1);
        assert_eq!(config.workflows["main"].jobs.len(), 2);
        assert!(config.jobs.contains_key("rust-test"));
        assert!(config.jobs.contains_key("docker-build"));
    }

    #[test]
    fn test_docker_job_gets_setup_remote_docker() {
        let config = lower_circleci(&[PresetJobs::new(
            "Docker",
            "Docker",
            vec![Job::new("build", "Build", Stage::Build)
                .with_docker()
                .with_steps(vec![Step::checkout(), Step::run("Build", "docker build .")])],
        )]);
        let job = &config.jobs["docker-build"];
        assert_eq!(job.docker[0].image, DEFAULT_IMAGE);
        assert_eq!(
            job.steps[1],
            CircleCIStep::Simple("setup_remote_docker".to_string())
        );
    }

    #[test]
    fn test_cache_restore_and_save() {
        let config = lower_circleci(&[PresetJobs::new(
            "Rust",
            "Rust",
            vec![Job::new("test", "Test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])],
        )]);
        let steps = &config.jobs["rust-test"].steps;
        assert!(matches!(steps[1], CircleCIStep::Cache { .. }));
        assert!(matches!(steps.last(), Some(CircleCIStep::SaveCache { .. })));
    }
}
