use crate::ir::{Job, RunnerOs, Step};
use crate::platforms::github::models::{
    GitHubDefaults, GitHubJob, GitHubMatrix, GitHubMatrixInclude, GitHubRunDefaults, GitHubStep,
    GitHubStrategy, GitHubTriggerConfig, GitHubTriggers, GitHubWorkflow,
};
use std::collections::BTreeMap;

/// Which workflow file is being produced. CI runs on branch pushes and pull
/// requests; Release runs only on version tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowKind {
    Ci,
    Release,
}

/// What keeps a tags-only job — the ones holding the publish credentials —
/// from running on a branch push: the workflow's own triggers, or an `if:`
/// on each such job.
///
/// Rendering a file from scratch, the two are the same question: the release
/// workflow is written with `on: push: tags: [v*]` and nothing else. A merge
/// into a file that already exists is not, because `update` leaves the user's
/// triggers alone — see [`tag_gate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagGate {
    /// The workflow only runs on `v*` tag pushes, so its jobs need nothing
    Workflow,
    /// The workflow can run otherwise, so every tags-only job carries the gate
    Job,
}

impl TagGate {
    /// The gate implied by a file cibox writes whole
    pub fn of(kind: WorkflowKind) -> Self {
        match kind {
            WorkflowKind::Ci => TagGate::Job,
            WorkflowKind::Release => TagGate::Workflow,
        }
    }
}

/// Whether the triggers `on` restrict a workflow to `v*` tag pushes, which
/// is the only thing standing between a tags-only job and a branch push in a
/// release workflow: the jobs in that file carry no `if:` of their own.
///
/// `update` merges into an existing workflow and keeps whatever triggers it
/// finds there, so the file's `on:` is not cibox's to assume. The shape that
/// gates is the one cibox writes itself:
///
/// ```yaml
/// on:
///   push:
///     tags:
///     - v*
/// ```
///
/// Any other trigger beside it (`pull_request`, `workflow_dispatch`,
/// `schedule`), a `branches` filter sharing the push — GitHub ORs the two, so
/// branch pushes fire it — or a tag pattern that admits more than `v*` all
/// mean the workflow can run on something that is not a version tag, and the
/// gate has to move onto the jobs.
///
/// Narrow on purpose. A wrong [`TagGate::Job`] costs an `if:` that is true on
/// a `v*` tag push anyway; a wrong [`TagGate::Workflow`] takes the gate off a
/// job that publishes releases.
pub fn tag_gate(on: &serde_yaml::Value) -> TagGate {
    // `on: [push]` and `on: push` name triggers without configuring them,
    // so neither can carry a tag filter
    let Some(triggers) = on.as_mapping() else {
        return TagGate::Job;
    };
    let mut triggers = triggers.iter();
    let (Some((event, push)), None) = (triggers.next(), triggers.next()) else {
        return TagGate::Job;
    };
    if event.as_str() != Some("push") {
        return TagGate::Job;
    }
    let Some(push) = push.as_mapping() else {
        return TagGate::Job;
    };
    let mut tags = None;
    for (key, value) in push {
        match key.as_str() {
            Some("tags") => tags = Some(value),
            // `branches`, `paths`, `tags-ignore`, anything unrecognized
            _ => return TagGate::Job,
        }
    }
    let gated = match tags {
        // A lone pattern may be written as a string
        Some(serde_yaml::Value::String(pattern)) => pattern.starts_with('v'),
        Some(serde_yaml::Value::Sequence(patterns)) => {
            !patterns.is_empty()
                && patterns
                    .iter()
                    .all(|p| p.as_str().is_some_and(|p| p.starts_with('v')))
        }
        _ => false,
    };
    if gated {
        TagGate::Workflow
    } else {
        TagGate::Job
    }
}

pub fn lower_github(jobs: &[Job], kind: WorkflowKind) -> GitHubWorkflow {
    lower_github_gated(jobs, kind, TagGate::of(kind))
}

/// Lower into a file whose triggers are already decided, which is what a
/// merge has: `gate` says whether they gate the tags-only jobs.
pub fn lower_github_gated(jobs: &[Job], kind: WorkflowKind, gate: TagGate) -> GitHubWorkflow {
    let lowered = jobs
        .iter()
        .map(|job| (job.id.clone(), lower_job(job, gate)))
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
        // Least privilege: checking out the code is all any job gets unless
        // it asks for more. Without this key the token inherits the
        // repository default, which on older repos and organizations is
        // write-all — handing every `run:` step push access.
        permissions: Some(base_permissions()),
        env: None,
        jobs: lowered,
    }
}

/// The scopes every job needs: read the repository so checkout works
fn base_permissions() -> BTreeMap<String, String> {
    BTreeMap::from([("contents".to_string(), "read".to_string())])
}

fn lower_job(job: &Job, gate: TagGate) -> GitHubJob {
    let mut steps = Vec::new();

    for step in &job.steps {
        match step {
            Step::Checkout { full_history } => {
                // actions/checkout leaves the job's token in .git/config as an
                // auth header unless told not to, where every later `run:`
                // step — build scripts, test suites, any dependency code they
                // execute — can read it. Nothing cibox generates talks to git
                // after the clone, so the credential is pure exposure: at
                // minimum read access to a private repository, and whatever
                // the job's extra scopes grant (e.g. `packages: write` on a
                // ghcr.io push).
                let mut with = BTreeMap::from([(
                    "persist-credentials".to_string(),
                    serde_yaml::Value::Bool(false),
                )]);
                if *full_history {
                    with.insert(
                        "fetch-depth".to_string(),
                        serde_yaml::Value::Number(0.into()),
                    );
                }
                steps.push(GitHubStep {
                    name: Some("Checkout code".to_string()),
                    uses: Some("actions/checkout@v4".to_string()),
                    run: None,
                    with: Some(with),
                    env: None,
                });
                if let Some(cache) = &job.cache {
                    // Matrix legs run different toolchains; sharing one cache
                    // key would make them overwrite each other
                    let key = match &job.matrix {
                        Some(_) => format!("{}-${{{{ matrix.version }}}}", cache.key),
                        None => cache.key.clone(),
                    };
                    steps.push(GitHubStep {
                        name: Some("Cache".to_string()),
                        uses: Some("actions/cache@v4".to_string()),
                        run: None,
                        with: Some(BTreeMap::from([
                            (
                                "path".to_string(),
                                serde_yaml::Value::String(cache.paths.join("\n")),
                            ),
                            ("key".to_string(), serde_yaml::Value::String(key)),
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
    // A credential reads from the secrets context and only from there. A
    // repository Variable is not a weaker secret, it is not a secret at all:
    // its value is shown in the settings UI, handed back in plaintext by the
    // variables API, and never registered for log masking — so a token that
    // lives there is printed verbatim the first time anything echoes its
    // environment. Offering `|| vars.NAME` for one would tell users that
    // keeping it there works, which is exactly the problem: it does work.
    for secret in &job.secrets {
        env.insert(secret.clone(), format!("${{{{ secrets.{secret} }}}}"));
    }
    // Configuration is different: nothing is exposed by keeping a registry
    // username in plain Variables, and plenty of users do, so `||` takes
    // whichever context has the value.
    for name in &job.variables {
        env.insert(
            name.clone(),
            format!("${{{{ secrets.{name} || vars.{name} }}}}"),
        );
    }

    // A job-level block replaces the workflow-level one rather than adding to
    // it, so the extra scopes have to be spelled out alongside the base ones
    let permissions = (!job.permissions.is_empty()).then(|| {
        let mut permissions = base_permissions();
        permissions.extend(job.permissions.iter().cloned());
        permissions
    });

    GitHubJob {
        name: job
            .matrix
            .as_ref()
            .map(|_| format!("{} (${{{{ matrix.version }}}})", job.name)),
        runs_on: match job.runs_on {
            RunnerOs::Linux => "ubuntu-latest",
            RunnerOs::Windows => "windows-latest",
        }
        .to_string(),
        strategy: job.matrix.as_ref().map(|entries| GitHubStrategy {
            fail_fast: false,
            matrix: GitHubMatrix {
                include: entries
                    .iter()
                    .map(|e| GitHubMatrixInclude {
                        version: e.version.clone(),
                        image: e.image.clone(),
                    })
                    .collect(),
            },
        }),
        // Generated commands are POSIX shell; windows-latest ships Git Bash
        defaults: (job.runs_on == RunnerOs::Windows).then(|| GitHubDefaults {
            run: GitHubRunDefaults {
                shell: "bash".to_string(),
            },
        }),
        // Docker-daemon jobs run directly on the host runner
        container: if job.needs_docker {
            None
        } else if job.matrix.is_some() {
            Some("${{ matrix.image }}".to_string())
        } else {
            job.image.clone()
        },
        permissions,
        env: (!env.is_empty()).then_some(env),
        steps,
        needs: (!job.needs.is_empty()).then(|| job.needs.clone()),
        timeout_minutes: job.timeout_minutes,
        continue_on_error: None,
        // A tags-only job must not run on branch pushes or PRs, so unless
        // the workflow's own triggers already see to that, it says so itself
        if_expr: (job.tags_only && gate == TagGate::Job)
            .then(|| "startsWith(github.ref, 'refs/tags/v')".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, MatrixEntry, Stage, Step};

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
    fn test_checkout_does_not_persist_credentials() {
        let workflow = lower_github(
            &[
                Job::new("rust-test", "Cargo test", Stage::Test).with_steps(vec![Step::checkout()]),
                Job::new("gitleaks", "Gitleaks", Stage::Security)
                    .with_steps(vec![Step::checkout_full_history()]),
            ],
            WorkflowKind::Ci,
        );
        for id in ["rust-test", "gitleaks"] {
            let with = workflow.jobs[id].steps[0].with.as_ref().unwrap();
            assert_eq!(
                with["persist-credentials"],
                serde_yaml::Value::Bool(false),
                "{id} keeps the token in .git/config"
            );
        }
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
                .with_secrets(vec![
                    "DOCKER_PASSWORD".to_string(),
                    "GITHUB_TOKEN".to_string(),
                ])
                .with_variables(vec!["DOCKER_USERNAME".to_string()])],
            WorkflowKind::Release,
        );
        let env = workflow.jobs["docker-release"].env.as_ref().unwrap();
        // A credential comes from the secrets context and nowhere else
        assert_eq!(env["DOCKER_PASSWORD"], "${{ secrets.DOCKER_PASSWORD }}");
        assert_eq!(env["GITHUB_TOKEN"], "${{ secrets.GITHUB_TOKEN }}");
        // Configuration may come from either store
        assert_eq!(
            env["DOCKER_USERNAME"],
            "${{ secrets.DOCKER_USERNAME || vars.DOCKER_USERNAME }}"
        );
    }

    /// Nothing a rule declares as a credential may be given a `vars`
    /// fallback, on any workflow, for any rule: Variables are readable and
    /// unmasked, so the fallback would make storing a token there work.
    #[test]
    fn test_no_credential_reads_from_the_variables_context() {
        let facts = crate::detection::ProjectFacts::default();
        let mut config = crate::config::CiboxConfig::default();
        for r in crate::rules::resolve(&facts, &config) {
            config.set_enabled_override(r.rule.id(), Some(true));
        }
        let jobs: Vec<Job> = crate::rules::resolve(&facts, &config)
            .iter()
            .flat_map(|r| r.rule.jobs(&facts))
            .collect();

        let mut checked = 0;
        for kind in [WorkflowKind::Ci, WorkflowKind::Release] {
            for (id, lowered) in lower_github(&jobs, kind).jobs {
                let job = jobs.iter().find(|j| j.id == id).unwrap();
                let env = lowered.env.unwrap_or_default();
                for secret in &job.secrets {
                    assert_eq!(
                        env[secret],
                        format!("${{{{ secrets.{secret} }}}}"),
                        "{id} reads the credential {secret} from somewhere other than Secrets"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no job declared a credential to check");
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

    /// The canonical release triggers, as `tag_gate` sees them coming back
    /// off disk
    fn triggers(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_only_a_v_tag_push_gates_the_workflow() {
        // What cibox writes for a release workflow, in both tag spellings
        for gated in [
            "push:\n  tags:\n  - v*\n",
            "push:\n  tags: [v*]\n",
            "push:\n  tags: [v*, v?.?.?]\n",
            "push:\n  tags: v*\n",
        ] {
            assert_eq!(tag_gate(&triggers(gated)), TagGate::Workflow, "{gated}");
        }

        for open in [
            // Any trigger that is not a tag push can fire on a branch
            "pull_request:\n",
            "push: null\n",
            "[push]\n",
            "push\n",
            "workflow_dispatch:\n",
            "push:\n  branches: [main]\n",
            "schedule:\n- cron: 0 0 * * *\n",
            // ...including one sitting next to the tag push
            "push:\n  tags: [v*]\npull_request:\n",
            "workflow_dispatch:\npush:\n  tags: [v*]\n",
            // GitHub ORs the two filters, so branch pushes fire this too
            "push:\n  branches: [main]\n  tags: [v*]\n",
            // Tags that are not version tags
            "push:\n  tags: ['*']\n",
            "push:\n  tags: [v*, nightly]\n",
            "push:\n  tags: []\n",
            "push:\n  tags-ignore: [draft*]\n",
        ] {
            assert_eq!(tag_gate(&triggers(open)), TagGate::Job, "{open}");
        }
    }

    #[test]
    fn test_tags_only_job_gated_by_the_job_when_the_workflow_does_not() {
        // A release workflow whose triggers were changed: the file no longer
        // gates the job, so the job has to
        let workflow = lower_github_gated(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Release,
            TagGate::Job,
        );
        assert_eq!(
            workflow.jobs["rust-release"].if_expr.as_deref(),
            Some("startsWith(github.ref, 'refs/tags/v')")
        );
        // Jobs that run on every push are not gated either way
        let workflow = lower_github_gated(
            &[Job::new("rust-test", "Cargo test", Stage::Test)],
            WorkflowKind::Ci,
            TagGate::Job,
        );
        assert_eq!(workflow.jobs["rust-test"].if_expr, None);
    }

    #[test]
    fn test_windows_job_runs_on_windows_with_bash() {
        let workflow = lower_github(
            &[
                Job::new("docker-release-windows", "Docker push (windows)", Stage::Deploy)
                    .on_windows(),
                Job::new("docker-release-linux", "Docker push (linux)", Stage::Deploy),
            ],
            WorkflowKind::Release,
        );
        let windows = &workflow.jobs["docker-release-windows"];
        assert_eq!(windows.runs_on, "windows-latest");
        assert_eq!(windows.defaults.as_ref().unwrap().run.shell, "bash");
        let linux = &workflow.jobs["docker-release-linux"];
        assert_eq!(linux.runs_on, "ubuntu-latest");
        assert_eq!(linux.defaults, None);
    }

    #[test]
    fn test_workflow_token_is_read_only_by_default() {
        for kind in [WorkflowKind::Ci, WorkflowKind::Release] {
            let workflow = lower_github(&[Job::new("rust-test", "Cargo test", Stage::Test)], kind);
            assert_eq!(
                workflow.permissions.as_ref().unwrap()["contents"],
                "read",
                "{kind:?}"
            );
            // Nothing declared, so the job inherits the workflow scopes
            assert_eq!(workflow.jobs["rust-test"].permissions, None, "{kind:?}");
        }
    }

    #[test]
    fn test_declared_permissions_are_added_to_the_base_scopes() {
        let workflow = lower_github(
            &[Job::new("docker-release", "Docker push", Stage::Deploy)
                .with_permission("packages", "write")],
            WorkflowKind::Release,
        );
        let permissions = workflow.jobs["docker-release"]
            .permissions
            .as_ref()
            .unwrap();
        assert_eq!(permissions["packages"], "write");
        // A job-level block overrides the workflow one outright, so checkout
        // would break without the base scope repeated here
        assert_eq!(permissions["contents"], "read");
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
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])
                .with_matrix(entries)],
            WorkflowKind::Ci,
        );
        let job = &workflow.jobs["rust-test"];
        assert_eq!(job.name.as_deref(), Some("Cargo test (${{ matrix.version }})"));
        assert_eq!(job.container.as_deref(), Some("${{ matrix.image }}"));
        let strategy = job.strategy.as_ref().unwrap();
        assert!(!strategy.fail_fast);
        assert_eq!(strategy.matrix.include.len(), 2);
        assert_eq!(strategy.matrix.include[1].image, "rustlang/rust:nightly");
        // Each leg caches under its own key
        assert_eq!(
            job.steps[1].with.as_ref().unwrap()["key"],
            serde_yaml::Value::String("rust-cache-${{ matrix.version }}".to_string())
        );
    }

    #[test]
    fn test_non_matrix_job_has_no_strategy_or_name() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test).with_image("rust:latest")],
            WorkflowKind::Ci,
        );
        let yaml = serde_yaml::to_string(&workflow).unwrap();
        assert!(!yaml.contains("strategy"), "{yaml}");
        assert!(!yaml.contains("fail-fast"), "{yaml}");
        assert_eq!(workflow.jobs["rust-test"].name, None);
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
