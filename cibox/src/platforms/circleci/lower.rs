use crate::ir::{Job, Step};
use crate::platforms::circleci::models::{
    CircleCICache, CircleCICacheSave, CircleCIConfig, CircleCIDocker, CircleCIFilterPattern,
    CircleCIFilters, CircleCIJob, CircleCIParameter, CircleCIRun, CircleCIStep, CircleCIWorkflow,
    CircleCIWorkflowJob, CircleCIWorkflowJobDetail,
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

        if let Some(entries) = &job.matrix {
            // One invocation per matrix leg: CircleCI's `matrix:` keyword
            // cross-products parameters, which is wrong for paired
            // version/image values
            for entry in entries {
                workflow_jobs.push(CircleCIWorkflowJob::Detailed {
                    job: BTreeMap::from([(
                        job.id.clone(),
                        CircleCIWorkflowJobDetail {
                            name: Some(format!("{}-{}", job.id, entry.version)),
                            requires: (!job.needs.is_empty()).then(|| job.needs.clone()),
                            filters: filters.clone(),
                            params: BTreeMap::from([
                                ("version".to_string(), entry.version.clone()),
                                ("image".to_string(), entry.image.clone()),
                            ]),
                        },
                    )]),
                });
            }
        } else if job.needs.is_empty() && filters.is_none() {
            workflow_jobs.push(CircleCIWorkflowJob::Simple(job.id.clone()));
        } else {
            workflow_jobs.push(CircleCIWorkflowJob::Detailed {
                job: BTreeMap::from([(
                    job.id.clone(),
                    CircleCIWorkflowJobDetail {
                        name: None,
                        requires: (!job.needs.is_empty()).then(|| job.needs.clone()),
                        filters,
                        params: BTreeMap::new(),
                    },
                )]),
            });
        }
    }

    CircleCIConfig {
        version: "2.1".to_string(),
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
    } else if job.matrix.is_some() {
        "<< parameters.image >>".to_string()
    } else {
        job.image.clone().unwrap_or_else(|| DEFAULT_IMAGE.to_string())
    };

    // Matrix legs run different toolchains; sharing one cache key would
    // make them overwrite each other
    let cache_key = |key: &str| match &job.matrix {
        Some(_) => format!("{key}-<< parameters.version >>"),
        None => key.to_string(),
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
                            keys: vec![cache_key(&cache.key)],
                        },
                    });
                }
            }
            Step::Run { name, command } => {
                steps.push(CircleCIStep::Command {
                    run: CircleCIRun {
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
                key: cache_key(&cache.key),
                paths: cache.paths.clone(),
            },
        });
    }

    let env: BTreeMap<String, String> = job.env.iter().cloned().collect();
    CircleCIJob {
        parameters: job.matrix.as_ref().map(|_| {
            let string = || CircleCIParameter {
                param_type: "string".to_string(),
            };
            BTreeMap::from([("version".to_string(), string()), ("image".to_string(), string())])
        }),
        docker: vec![CircleCIDocker { image }],
        steps,
        environment: (!env.is_empty()).then_some(env),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, MatrixEntry, Stage, Step};

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
    fn test_matrix_job_enumerates_invocations() {
        let entries = vec![
            MatrixEntry {
                version: "1.85".to_string(),
                image: "rust:1.85".to_string(),
            },
            MatrixEntry {
                version: "nightly".to_string(),
                image: "rustlang/rust:nightly".to_string(),
            },
        ];
        let config = lower_circleci(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])
            .with_cache("rust-cache", vec!["target/".to_string()])
            .with_matrix(entries)]);

        let job = &config.jobs["rust-test"];
        assert_eq!(job.docker[0].image, "<< parameters.image >>");
        let params = job.parameters.as_ref().unwrap();
        assert_eq!(params["version"].param_type, "string");
        assert_eq!(params["image"].param_type, "string");
        assert!(matches!(
            &job.steps[1],
            CircleCIStep::Cache { restore_cache }
                if restore_cache.keys == vec!["rust-cache-<< parameters.version >>"]
        ));

        let invocations: Vec<&CircleCIWorkflowJobDetail> = config.workflows["main"]
            .jobs
            .iter()
            .filter_map(|j| match j {
                CircleCIWorkflowJob::Detailed { job } => job.get("rust-test"),
                _ => None,
            })
            .collect();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].name.as_deref(), Some("rust-test-1.85"));
        assert_eq!(invocations[0].params["image"], "rust:1.85");
        assert_eq!(invocations[1].params["version"], "nightly");

        // The YAML round-trips through the typed model despite flatten +
        // the untagged workflow-job enum
        let yaml = serde_yaml::to_string(&config).unwrap();
        let reparsed: CircleCIConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(reparsed, config);
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
