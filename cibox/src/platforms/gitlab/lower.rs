use crate::ir::{Job, Stage, Step};
use crate::platforms::gitlab::models::{
    GitLabArtifacts, GitLabCI, GitLabCache, GitLabJob, GitLabMatrixCell, GitLabParallel, GitLabRule,
};
use std::collections::BTreeMap;

/// Release jobs hold the registry credentials, so they must fire on the same
/// `v*` tags as every other backend — not on any tag that happens to be
/// pushed. Matched against `$CI_COMMIT_TAG`, which is only set in a tag
/// pipeline, so branch pushes can't satisfy it.
const TAG_CONDITION: &str = "$CI_COMMIT_TAG =~ /^v/";

/// Image holding the docker CLI for jobs that build images
const DOCKER_IMAGE: &str = "docker:latest";

/// GitLab runners give a job no docker daemon of its own: the `docker:latest`
/// image is only the client, and `/var/run/docker.sock` inside the job's
/// container belongs to nothing. A daemon has to be linked in as a service,
/// and the client pointed at it over the network — otherwise every `docker`
/// command dies with "Cannot connect to the Docker daemon". The service is
/// reachable as `docker`, and `dind` generates its TLS material into
/// `DOCKER_TLS_CERTDIR`, shared with the job through the services mount.
const DIND_SERVICE: &str = "docker:dind";
const DOCKER_CERT_DIR: &str = "/certs";

fn dind_client_variables() -> BTreeMap<String, String> {
    [
        ("DOCKER_HOST", "tcp://docker:2376".to_string()),
        ("DOCKER_TLS_CERTDIR", DOCKER_CERT_DIR.to_string()),
        ("DOCKER_TLS_VERIFY", "1".to_string()),
        ("DOCKER_CERT_PATH", format!("{DOCKER_CERT_DIR}/client")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

pub fn lower_gitlab(jobs: &[Job]) -> GitLabCI {
    let mut lowered = BTreeMap::new();
    let mut stages: Vec<Stage> = Vec::new();

    for job in jobs {
        if !stages.contains(&job.stage) {
            stages.push(job.stage);
        }
        lowered.insert(job.id.clone(), lower_job(job));
    }

    stages.sort();
    GitLabCI {
        stages: (!stages.is_empty())
            .then(|| stages.iter().map(|s| s.as_str().to_string()).collect()),
        cache: None,
        jobs: lowered,
    }
}

fn lower_job(job: &Job) -> GitLabJob {
    // Each job's environment stays on the job. Hoisted to the top level it
    // would reach every other job: the nightly doc job's `RUSTDOCFLAGS=--cfg
    // docsrs` would then be in force while `rust-test` builds its doctests on
    // stable, and the full-history clone gitleaks needs would be paid by the
    // whole pipeline.
    let mut variables: BTreeMap<String, String> = job.env.iter().cloned().collect();
    if job
        .steps
        .iter()
        .any(|step| matches!(step, Step::Checkout { full_history } if *full_history))
    {
        // GitLab clones shallowly by default
        variables.insert("GIT_DEPTH".to_string(), "0".to_string());
    }
    if job.needs_docker {
        variables.extend(dind_client_variables());
    }

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
            Some(DOCKER_IMAGE.to_string())
        } else if job.matrix.is_some() {
            // Expanded per leg from the parallel:matrix variables
            Some("$IMAGE".to_string())
        } else {
            job.image.clone()
        },
        services: job.needs_docker.then(|| vec![DIND_SERVICE.to_string()]),
        variables: (!variables.is_empty()).then_some(variables),
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
        rules: job.tags_only.then(|| {
            vec![GitLabRule {
                if_expr: TAG_CONDITION.to_string(),
            }]
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
    fn test_full_history_sets_git_depth_on_that_job_only() {
        let config = lower_gitlab(&[
            Job::new("gitleaks", "Gitleaks", Stage::Security)
                .with_steps(vec![Step::checkout_full_history()]),
            Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")]),
        ]);
        assert_eq!(
            config.jobs["gitleaks"].variables.as_ref().unwrap()["GIT_DEPTH"],
            "0"
        );
        // Every other job keeps GitLab's shallow clone
        assert_eq!(config.jobs["rust-test"].variables, None);
    }

    #[test]
    fn test_job_env_stays_on_its_own_job() {
        let config = lower_gitlab(&[
            Job::new("rust-doc", "Cargo doc", Stage::Lint)
                .with_env("RUSTDOCFLAGS", "--cfg docsrs")
                .with_env("CARGO_HOME", ".cargo"),
            Job::new("rust-test", "Cargo test", Stage::Test).with_env("CARGO_HOME", ".cargo"),
        ]);
        let doc = config.jobs["rust-doc"].variables.as_ref().unwrap();
        assert_eq!(doc["RUSTDOCFLAGS"], "--cfg docsrs");
        assert_eq!(doc["CARGO_HOME"], ".cargo");
        // rust-test builds its doctests on stable, where `--cfg docsrs` can
        // mean nightly-only attributes: it must not inherit the doc job's flags
        let test = config.jobs["rust-test"].variables.as_ref().unwrap();
        assert_eq!(test["CARGO_HOME"], ".cargo");
        assert!(!test.contains_key("RUSTDOCFLAGS"), "{test:?}");
    }

    #[test]
    fn test_docker_job_uses_docker_image() {
        let config =
            lower_gitlab(&[Job::new("docker-release", "Docker push", Stage::Deploy).with_docker()]);
        let job = &config.jobs["docker-release"];
        assert_eq!(job.image.as_deref(), Some("docker:latest"));
    }

    #[test]
    fn test_docker_job_gets_a_daemon_to_talk_to() {
        let config = lower_gitlab(&[Job::new("docker-build", "Docker build", Stage::Build)
            .with_docker()
            .with_steps(vec![
                Step::checkout(),
                Step::run("Build image", "docker build -t owner/app ."),
            ])]);
        let job = &config.jobs["docker-build"];
        // The docker image ships the client only, so without a linked daemon
        // every docker command in the script fails to connect
        assert_eq!(
            job.services.as_deref(),
            Some(["docker:dind".to_string()].as_slice())
        );
        let vars = job
            .variables
            .as_ref()
            .expect("docker job needs DOCKER_HOST");
        assert_eq!(vars["DOCKER_HOST"], "tcp://docker:2376");
        // dind writes its certificates here and the client reads them back
        // out of the same shared mount
        assert_eq!(vars["DOCKER_TLS_CERTDIR"], "/certs");
        assert_eq!(vars["DOCKER_TLS_VERIFY"], "1");
        assert_eq!(vars["DOCKER_CERT_PATH"], "/certs/client");
    }

    #[test]
    fn test_non_docker_job_gets_no_services() {
        let config = lower_gitlab(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_image("rust:latest")
            .with_steps(vec![Step::run("Test", "cargo test")])]);
        let job = &config.jobs["rust-test"];
        assert_eq!(job.services, None);
        assert_eq!(job.variables, None);
    }

    #[test]
    fn test_release_job_runs_on_version_tags_only() {
        let config = lower_gitlab(&[
            Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only(),
            Job::new("rust-test", "Cargo test", Stage::Test),
        ]);
        // A bare `tags` ref would publish on every tag pushed, not just the
        // `v*` releases the other backends gate on
        assert_eq!(
            config.jobs["rust-release"].rules.as_deref(),
            Some(
                [GitLabRule {
                    if_expr: "$CI_COMMIT_TAG =~ /^v/".to_string(),
                }]
                .as_slice()
            )
        );
        // Everything else keeps GitLab's default "run in every pipeline"
        assert_eq!(config.jobs["rust-test"].rules, None);
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
