use crate::ir::{Job, Stage, Step};
use crate::platforms::gitlab::models::{
    GitLabArtifacts, GitLabCI, GitLabCache, GitLabJob, GitLabMatrixCell, GitLabOnly, GitLabParallel,
};
use std::collections::BTreeMap;

pub fn lower_gitlab(jobs: &[Job]) -> GitLabCI {
    let mut lowered = BTreeMap::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut variables: BTreeMap<String, String> = BTreeMap::new();

    for job in jobs {
        if !stages.contains(&job.stage) {
            stages.push(job.stage);
        }
        for (key, value) in &job.env {
            variables.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // GitLab clones shallowly by default; a full-history checkout
        // needs GIT_DEPTH=0
        if job
            .steps
            .iter()
            .any(|step| matches!(step, Step::Checkout { full_history } if *full_history))
        {
            variables.insert("GIT_DEPTH".to_string(), "0".to_string());
        }
        lowered.insert(job.id.clone(), lower_job(job));
    }

    stages.sort();
    GitLabCI {
        stages: (!stages.is_empty())
            .then(|| stages.iter().map(|s| s.as_str().to_string()).collect()),
        variables: (!variables.is_empty()).then_some(variables),
        cache: None,
        jobs: lowered,
    }
}

fn lower_job(job: &Job) -> GitLabJob {
    let script = job
        .steps
        .iter()
        .filter_map(|step| match step {
            // Checkout is implicit on GitLab
            Step::Checkout { .. } => None,
            Step::Run { command, .. } => Some(command.clone()),
        })
        .collect();

    GitLabJob {
        stage: job.stage.as_str().to_string(),
        image: if job.needs_docker {
            Some("docker:latest".to_string())
        } else if job.matrix.is_some() {
            // Expanded per leg from the parallel:matrix variables
            Some("$IMAGE".to_string())
        } else {
            job.image.clone()
        },
        script,
        before_script: None,
        after_script: None,
        needs: (!job.needs.is_empty()).then(|| job.needs.clone()),
        cache: job.cache.as_ref().map(|cache| GitLabCache {
            // Matrix legs run different toolchains; sharing one cache key
            // would make them overwrite each other
            key: match &job.matrix {
                Some(_) => format!("{}-$VERSION", cache.key),
                None => cache.key.clone(),
            },
            paths: cache.paths.clone(),
        }),
        artifacts: (!job.artifacts.is_empty()).then(|| GitLabArtifacts {
            paths: job.artifacts.clone(),
            name: None,
        }),
        only: job.tags_only.then(|| GitLabOnly {
            refs: Some(vec!["tags".to_string()]),
        }),
        timeout: job.timeout_minutes.map(|minutes| format!("{minutes}m")),
        parallel: job.matrix.as_ref().map(|entries| GitLabParallel {
            matrix: entries
                .iter()
                .map(|e| GitLabMatrixCell {
                    version: e.version.clone(),
                    image: e.image.clone(),
                })
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, MatrixEntry, Stage, Step};

    #[test]
    fn test_stages_union_in_order() {
        let config = lower_gitlab(&[
            Job::new("docker-build", "Docker build", Stage::Build),
            Job::new("rust-test", "Cargo test", Stage::Test),
        ]);
        assert_eq!(
            config.stages,
            Some(vec!["test".to_string(), "build".to_string()])
        );
        assert!(config.jobs.contains_key("docker-build"));
        assert!(config.jobs.contains_key("rust-test"));
    }

    #[test]
    fn test_full_history_sets_git_depth() {
        let config = lower_gitlab(&[Job::new("gitleaks", "Gitleaks", Stage::Security)
            .with_steps(vec![Step::checkout_full_history()])]);
        assert_eq!(config.variables.unwrap()["GIT_DEPTH"], "0");
    }

    #[test]
    fn test_docker_job_uses_docker_image() {
        let config = lower_gitlab(&[Job::new("docker-release", "Docker push", Stage::Deploy)
            .with_docker()
            .tags_only()]);
        let job = &config.jobs["docker-release"];
        assert_eq!(job.image.as_deref(), Some("docker:latest"));
        assert_eq!(
            job.only.as_ref().unwrap().refs,
            Some(vec!["tags".to_string()])
        );
    }

    #[test]
    fn test_matrix_job() {
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
        let config = lower_gitlab(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_cache("rust-cache", vec!["target/".to_string()])
            .with_matrix(entries)]);
        let job = &config.jobs["rust-test"];
        assert_eq!(job.image.as_deref(), Some("$IMAGE"));
        assert_eq!(job.cache.as_ref().unwrap().key, "rust-cache-$VERSION");
        let cells = &job.parallel.as_ref().unwrap().matrix;
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].version, "1.85");
        assert_eq!(cells[1].image, "rustlang/rust:nightly");
        // VERSION must serialize before IMAGE: GitLab derives the leg name
        // from variable order
        let yaml = serde_yaml::to_string(&config).unwrap();
        let version_pos = yaml.find("VERSION: '1.85'").unwrap();
        let image_pos = yaml.find("IMAGE: rust:1.85").unwrap();
        assert!(version_pos < image_pos, "{yaml}");
    }

    #[test]
    fn test_checkout_excluded_from_script() {
        let config = lower_gitlab(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])]);
        assert_eq!(config.jobs["rust-test"].script, vec!["cargo test"]);
    }
}
