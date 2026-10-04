use crate::ir::{Job, Step};
use crate::platforms::circleci::models::{
    CircleCICache, CircleCICacheSave, CircleCIConfig, CircleCIDocker, CircleCIFilterPattern,
    CircleCIFilters, CircleCIJob, CircleCIRun, CircleCIStep, CircleCIStoreArtifacts,
    CircleCIWorkflow, CircleCIWorkflowJob, CircleCIWorkflowJobDetail,
};
use std::collections::BTreeMap;

const DEFAULT_IMAGE: &str = "cimg/base:stable";

pub fn lower_circleci(jobs: &[Job]) -> CircleCIConfig {
    let mut lowered = BTreeMap::new();
    let mut workflow_jobs = Vec::new();

    for job in jobs {
        lowered.insert(job.id.clone(), lower_job(job));

        // CircleCI runs no job on tag pushes unless it has a tag filter, so
        // tags-only jobs need both the tag filter and a branch ignore
        let filters = job.tags_only.then(|| CircleCIFilters {
            tags: Some(CircleCIFilterPattern {
                only: Some("/^v.*/".to_string()),
                ignore: None,
            }),
            branches: Some(CircleCIFilterPattern {
                only: None,
                ignore: Some("/.*/".to_string()),
            }),
        });

        if job.needs.is_empty() && filters.is_none() {
            workflow_jobs.push(CircleCIWorkflowJob::Simple(job.id.clone()));
        } else {
            workflow_jobs.push(CircleCIWorkflowJob::Detailed {
                job: BTreeMap::from([(
                    job.id.clone(),
                    CircleCIWorkflowJobDetail {
                        requires: (!job.needs.is_empty()).then(|| job.needs.clone()),
                        filters,
                    },
                )]),
            });
        }
    }

    CircleCIConfig {
        version: "2.1".to_string(),
        orbs: None,
        jobs: lowered,
        workflows: BTreeMap::from([(
            "main".to_string(),
            CircleCIWorkflow {
                jobs: workflow_jobs,
            },
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
            Job::new("rust-test", "Cargo test", Stage::Test),
            Job::new("docker-build", "Docker build", Stage::Build),
        ]);
        assert_eq!(config.workflows.len(), 1);
        assert_eq!(config.workflows["main"].jobs.len(), 2);
        assert!(config.jobs.contains_key("rust-test"));
        assert!(config.jobs.contains_key("docker-build"));
    }

    #[test]
    fn test_docker_job_gets_setup_remote_docker() {
        let config = lower_circleci(&[Job::new("docker-build", "Docker build", Stage::Build)
            .with_docker()
            .with_steps(vec![Step::checkout(), Step::run("Build", "docker build .")])]);
        let job = &config.jobs["docker-build"];
        assert_eq!(job.docker[0].image, DEFAULT_IMAGE);
        assert_eq!(
            job.steps[1],
            CircleCIStep::Simple("setup_remote_docker".to_string())
        );
    }

    #[test]
    fn test_cache_restore_and_save() {
        let config = lower_circleci(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])
            .with_cache("rust-cache", vec!["target/".to_string()])]);
        let steps = &config.jobs["rust-test"].steps;
        assert!(matches!(steps[1], CircleCIStep::Cache { .. }));
        assert!(matches!(steps.last(), Some(CircleCIStep::SaveCache { .. })));
    }

    #[test]
    fn test_tags_only_job_gets_filters() {
        let config = lower_circleci(&[
            Job::new("rust-test", "Cargo test", Stage::Test),
            Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only(),
        ]);
        let detailed = config.workflows["main"]
            .jobs
            .iter()
            .find_map(|j| match j {
                CircleCIWorkflowJob::Detailed { job } => job.get("rust-release"),
                _ => None,
            })
            .expect("release job should be detailed");
        let filters = detailed.filters.as_ref().unwrap();
        assert_eq!(
            filters.tags.as_ref().unwrap().only.as_deref(),
            Some("/^v.*/")
        );
        assert_eq!(
            filters.branches.as_ref().unwrap().ignore.as_deref(),
            Some("/.*/")
        );

        // The YAML round-trips
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("filters"), "{yaml}");
    }
}
